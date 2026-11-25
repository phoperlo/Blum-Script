//! Native x86-64 code generator: BlumScript AST -> machine code.
//!
//! Calling convention: Microsoft x64 (RCX, RDX, R8, R9 + shadow space).
//! Every expression leaves its value in RAX.  Locals live at negative offsets
//! from RBP; the frame size is patched into the prologue after the body has
//! been generated.

use std::collections::HashMap;

use crate::abi::*;
use crate::ast::*;
use crate::types::Type;
use crate::x64::*;

pub struct StructInfo {
    pub fields: Vec<(String, Type, usize)>,
    pub size: usize,
}

pub struct Output {
    pub asm: Asm,
    pub rdata: Vec<u8>,
    pub data: Vec<u8>,
    pub entry: u32,
}

pub fn is_ptr(t: &Type) -> bool {
    matches!(t, Type::Ptr(_) | Type::String)
}

fn pointee(t: &Type) -> Result<Type, String> {
    match t {
        Type::Ptr(inner) => Ok((**inner).clone()),
        Type::String => Ok(Type::Char),
        _ => Err(format!("cannot dereference non-pointer type '{}'", t.display())),
    }
}

pub struct Native {
    asm: Asm,
    rdata: Vec<u8>,
    strings: HashMap<String, u32>,
    data: Vec<u8>,
    globals: HashMap<String, (Type, u32)>,
    structs: HashMap<String, StructInfo>,
    fns: HashMap<String, FuncDecl>,
    fn_labels: HashMap<String, u32>,
    fn_order: Vec<String>,
    rt: HashMap<String, u32>,
    scopes: Vec<Vec<(String, Type, usize)>>,
    frame: usize,
    max_frame: usize,
    depth: usize,
    breaks: Vec<u32>,
    continues: Vec<u32>,
    entry: u32,
}

pub fn compile(program: &[Stmt]) -> Result<Output, String> {
    let mut n = Native {
        asm: Asm::new(),
        rdata: Vec::new(),
        strings: HashMap::new(),
        data: vec![0u8; DATA_HEAD],
        globals: HashMap::new(),
        structs: HashMap::new(),
        fns: HashMap::new(),
        fn_labels: HashMap::new(),
        fn_order: Vec::new(),
        rt: HashMap::new(),
        scopes: Vec::new(),
        frame: 0,
        max_frame: 0,
        depth: 0,
        breaks: Vec::new(),
        continues: Vec::new(),
        entry: 0,
    };

    n.collect_structs(program)?;
    n.collect_fns(program)?;
    n.collect_globals(program)?;

    if !n.fn_labels.contains_key("main") {
        return Err("no `main` function found".into());
    }

    let null_off = n.intern("null")?;
    n.rt = crate::runtime::emit(&mut n.asm, null_off);
    n.emit_entry(program)?;

    let order = n.fn_order.clone();
    for name in order {
        let f = n.fns.get(&name).unwrap().clone();
        n.emit_fn(&f)?;
    }

    Ok(Output {
        asm: n.asm,
        rdata: n.rdata,
        data: n.data,
        entry: n.entry,
    })
}

impl Native {
    // ---------------- collection ----------------

    fn collect_structs(&mut self, program: &[Stmt]) -> Result<(), String> {
        let mut pending: Vec<&StructDecl> = program
            .iter()
            .filter_map(|s| match s {
                Stmt::Struct(sd) => Some(sd),
                _ => None,
            })
            .collect();
        let mut guard = 0;
        while !pending.is_empty() {
            guard += 1;
            if guard > 512 {
                return Err("struct layout dependency cycle".into());
            }
            let mut still: Vec<&StructDecl> = Vec::new();
            let mut progress = false;
            for sd in pending {
                if self.build_struct(sd).is_ok() {
                    progress = true;
                } else {
                    still.push(sd);
                }
            }
            if !progress {
                let names: Vec<String> = still.iter().map(|s| s.name.clone()).collect();
                return Err(format!(
                    "cannot resolve struct layout (unknown or cyclic types): {}",
                    names.join(", ")
                ));
            }
            pending = still;
        }
        Ok(())
    }

    fn build_struct(&mut self, sd: &StructDecl) -> Result<(), String> {
        let mut off = 0usize;
        let mut fields = Vec::new();
        for (name, ty) in &sd.fields {
            let sz = self.size_of(ty)?;
            let al = self.align_of(ty)?.max(1);
            off = (off + al - 1) / al * al;
            fields.push((name.clone(), ty.clone(), off));
            off += sz;
        }
        let size = ((off + 7) / 8 * 8).max(8);
        self.structs
            .insert(sd.name.clone(), StructInfo { fields, size });
        Ok(())
    }

    fn collect_fns(&mut self, program: &[Stmt]) -> Result<(), String> {
        for s in program {
            if let Stmt::Func(f) = s {
                if self.fn_labels.contains_key(&f.name) {
                    return Err(format!("duplicate function '{}'", f.name));
                }
                let lbl = self.asm.label();
                self.fn_labels.insert(f.name.clone(), lbl);
                self.fns.insert(f.name.clone(), f.clone());
                self.fn_order.push(f.name.clone());
            }
        }
        Ok(())
    }

    fn collect_globals(&mut self, program: &[Stmt]) -> Result<(), String> {
        for s in program {
            if let Stmt::Let(name, ty, init) | Stmt::Var(name, ty, init) = s {
                let t = match ty {
                    Some(t) => t.clone(),
                    None => self.type_of(init)?,
                };
                let sz = self.size_of(&t)?.max(1);
                let al = self.align_of(&t)?.max(1);
                let off = (self.data.len() + al - 1) / al * al;
                self.data.resize(off + sz.max(8), 0);
                self.globals.insert(name.clone(), (t, off as u32));
            }
        }
        Ok(())
    }

    // ---------------- type queries ----------------

    pub fn size_of(&self, t: &Type) -> Result<usize, String> {
        Ok(match t {
            Type::Void => 0,
            Type::Bool | Type::Char | Type::Byte => 1,
            Type::Int | Type::UInt | Type::Float => 8,
            Type::String | Type::Ptr(_) => 8,
            Type::Array(e, n) => self.size_of(e)? * n,
            Type::Named(s) => self
                .structs
                .get(s)
                .ok_or_else(|| format!("unknown type '{}'", s))?
                .size,
        })
    }

    fn align_of(&self, t: &Type) -> Result<usize, String> {
        Ok(match t {
            Type::Array(e, _) => self.align_of(e)?,
            Type::Named(s) => {
                let si = self
                    .structs
                    .get(s)
                    .ok_or_else(|| format!("unknown type '{}'", s))?;
                let mut a = 1usize;
                for (_, ft, _) in &si.fields {
                    a = a.max(self.align_of(ft)?);
                }
                a
            }
            _ => self.size_of(t)?.min(8).max(1),
        })
    }

    fn find_local(&self, name: &str) -> Option<(Type, usize)> {
        for scope in self.scopes.iter().rev() {
            for (n, t, o) in scope.iter().rev() {
                if n == name {
                    return Some((t.clone(), *o));
                }
            }
        }
        None
    }

    fn field_info(&self, s: &str, f: &str) -> Result<(usize, Type), String> {
        let si = self
            .structs
            .get(s)
            .ok_or_else(|| format!("unknown struct '{}'", s))?;
        for (n, t, o) in &si.fields {
            if n == f {
                return Ok((*o, t.clone()));
            }
        }
        Err(format!("struct '{}' has no field '{}'", s, f))
    }

    fn call_type(&self, name: &str, args: &[Expr]) -> Result<Type, String> {
        match name {
            "print" | "println" => Ok(Type::Void),
            "len" | "strlen" => Ok(Type::Int),
            "malloc" => Ok(Type::Ptr(Box::new(Type::Void))),
            "free" => Ok(Type::Void),
            "to_int" => Ok(Type::Int),
            "to_float" => Ok(Type::Float),
            _ => {
                let f = self
                    .fns
                    .get(name)
                    .ok_or_else(|| format!("undefined function '{}'", name))?;
                if f.params.len() != args.len() {
                    return Err(format!(
                        "function '{}' expects {} argument(s), got {}",
                        name,
                        f.params.len(),
                        args.len()
                    ));
                }
                Ok(f.ret.clone())
            }
        }
    }

    // ---------------- frame / data helpers ----------------

    fn alloc(&mut self, size: usize) -> usize {
        let s = (size.max(1) + 7) & !7;
        self.frame += s;
        if self.frame > self.max_frame {
            self.max_frame = self.frame;
        }
        self.frame
    }

    fn store_slot(&mut self, off: usize, t: &Type) -> Result<(), String> {
        let d = -(off as i32);
        match self.size_of(t)? {
            1 => self.asm.store8(RBP, d, RAX),
            8 => self.asm.store(RBP, d, RAX),
            n => return Err(format!("cannot store a {}-byte aggregate in a variable", n)),
        }
        Ok(())
    }

    fn store_global(&mut self, off: u32, t: &Type) -> Result<(), String> {
        match self.size_of(t)? {
            1 => self.asm.store_rip8(RAX, Fixup::Data(off)),
            8 => self.asm.store_rip(RAX, Fixup::Data(off)),
            n => return Err(format!("cannot store a {}-byte aggregate in a global", n)),
        }
        Ok(())
    }

    fn zero_slot(&mut self, off: usize, size: usize) -> Result<(), String> {
        let words = (size + 7) / 8;
        if words == 0 {
            return Ok(());
        }
        self.asm.lea(RAX, RBP, -(off as i32));
        self.asm.xor_rr(RDX, RDX);
        self.asm.mov_imm64(RCX, words as i64);
        let top = self.asm.label();
        self.asm.bind(top);
        self.asm.store(RAX, 0, RDX);
        self.asm.add_imm32(RAX, 8);
        self.asm.dec_r(RCX);
        self.asm.jcc(CC_NE, top);
        Ok(())
    }

    fn intern(&mut self, s: &str) -> Result<u32, String> {
        if let Some(o) = self.strings.get(s) {
            return Ok(*o);
        }
        let off = self.rdata.len() as u32;
        self.rdata.extend_from_slice(s.as_bytes());
        self.rdata.push(0);
        self.strings.insert(s.to_string(), off);
        Ok(off)
    }

    // ---------------- function prologue / epilogue ----------------

    fn emit_fn(&mut self, f: &FuncDecl) -> Result<(), String> {
        if self.size_of(&f.ret)? > 8 {
            return Err(format!(
                "function '{}' returns an aggregate; return a pointer instead",
                f.name
            ));
        }
        let lbl = *self.fn_labels.get(&f.name).unwrap();
        self.asm.bind(lbl);
        self.asm.push_r(RBP);
        self.asm.mov_rr(RBP, RSP);
        let patch = self.asm.sub_rsp_placeholder();

        self.frame = 0;
        self.max_frame = 0;
        self.depth = 0;
        self.scopes.clear();
        self.breaks.clear();
        self.continues.clear();
        self.scopes.push(Vec::new());

        let regs = [RCX, RDX, R8, R9];
        for (i, (name, ty)) in f.params.iter().enumerate() {
            let sz = self.size_of(ty)?;
            if sz > 8 {
                return Err(format!(
                    "parameter '{}' of '{}' is an aggregate; pass a pointer instead",
                    name, f.name
                ));
            }
            let off = self.alloc(sz);
            if i < 4 {
                self.asm.store(RBP, -(off as i32), regs[i]);
            } else {
                let src = 16 + 8 * (i as i32 - 4);
                self.asm.load(RAX, RBP, src);
                self.asm.store(RBP, -(off as i32), RAX);
            }
            self.scopes
                .last_mut()
                .unwrap()
                .push((name.clone(), ty.clone(), off));
        }

        self.stmts(&f.body)?;

        self.asm.leave();
        self.asm.ret();

        let fsz = ((self.max_frame + 15) / 16 * 16) as u32;
        self.asm.patch_u32(patch, fsz);
        Ok(())
    }

    fn emit_entry(&mut self, program: &[Stmt]) -> Result<(), String> {
        let lbl = self.asm.label();
        self.asm.bind(lbl);
        self.entry = self.asm.labels[lbl as usize].unwrap();
        self.asm.push_r(RBP);
        self.asm.mov_rr(RBP, RSP);
        let patch = self.asm.sub_rsp_placeholder();

        self.frame = 0;
        self.max_frame = 0;
        self.depth = 0;
        self.scopes.clear();
        self.scopes.push(Vec::new());

        // stdout handle
        self.asm.mov_imm64(RCX, -11); // STD_OUTPUT_HANDLE
        self.asm.call_iat(I_GETSTDHANDLE);
        self.asm.store_rip(RAX, Fixup::Data(D_STDOUT));

        // global initialisers and any top-level statements
        for s in program {
            match s {
                Stmt::Func(_) | Stmt::Struct(_) => {}
                Stmt::Let(name, _, init) | Stmt::Var(name, _, init) => {
                    let (t, off) = self
                        .globals
                        .get(name)
                        .cloned()
                        .ok_or_else(|| format!("internal: missing global '{}'", name))?;
                    self.expr(init)?;
                    self.store_global(off, &t)?;
                }
                other => self.stmt(other)?,
            }
        }

        let main = *self.fn_labels.get("main").unwrap();
        self.asm.call_rel(main);
        self.asm.mov_rr(RCX, RAX);
        self.asm.call_iat(I_EXITPROCESS);
        self.asm.leave();
        self.asm.ret();

        let fsz = ((self.max_frame + 15) / 16 * 16) as u32;
        self.asm.patch_u32(patch, fsz);
        Ok(())
    }

    // ---------------- statements ----------------

    fn stmts(&mut self, list: &[Stmt]) -> Result<(), String> {
        self.scopes.push(Vec::new());
        let mut err = None;
        for s in list {
            if let Err(e) = self.stmt(s) {
                err = Some(e);
                break;
            }
        }
        self.scopes.pop();
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Let(name, ty, init) | Stmt::Var(name, ty, init) => {
                let t = match ty {
                    Some(t) => t.clone(),
                    None => self.type_of(init)?,
                };
                let sz = self.size_of(&t)?;
                let off = self.alloc(sz);
                if sz > 8 {
                    if !matches!(init, Expr::Int(0)) {
                        return Err(format!(
                            "aggregate initializer for '{}' is not supported; use a pointer",
                            name
                        ));
                    }
                    self.zero_slot(off, sz)?;
                } else {
                    self.expr(init)?;
                    self.store_slot(off, &t)?;
                }
                self.scopes
                    .last_mut()
                    .unwrap()
                    .push((name.clone(), t, off));
                Ok(())
            }
            Stmt::Assign(name, e) => {
                if let Some((t, off)) = self.find_local(name) {
                    self.expr(e)?;
                    self.store_slot(off, &t)
                } else if let Some((t, off)) = self.globals.get(name).cloned() {
                    self.expr(e)?;
                    self.store_global(off, &t)
                } else {
                    Err(format!("assignment to undefined variable '{}'", name))
                }
            }
            Stmt::If(cond, then_b, else_b) => {
                let lelse = self.asm.label();
                let lend = self.asm.label();
                self.expr(cond)?;
                self.asm.test_rr(RAX, RAX);
                self.asm.jcc(CC_E, lelse);
                self.stmts(then_b)?;
                self.asm.jmp(lend);
                self.asm.bind(lelse);
                self.stmts(else_b)?;
                self.asm.bind(lend);
                Ok(())
            }
            Stmt::While(cond, body) => {
                let top = self.asm.label();
                let end = self.asm.label();
                self.asm.bind(top);
                self.expr(cond)?;
                self.asm.test_rr(RAX, RAX);
                self.asm.jcc(CC_E, end);
                self.breaks.push(end);
                self.continues.push(top);
                let r = self.stmts(body);
                self.breaks.pop();
                self.continues.pop();
                r?;
                self.asm.jmp(top);
                self.asm.bind(end);
                Ok(())
            }
            Stmt::For(init, cond, update, body) => self.for_loop(init, cond, update, body),
            Stmt::Return(v) => {
                match v {
                    Some(e) => {
                        self.expr(e)?;
                    }
                    None => self.asm.mov_imm64(RAX, 0),
                }
                self.asm.leave();
                self.asm.ret();
                Ok(())
            }
            Stmt::Break => {
                let l = *self
                    .breaks
                    .last()
                    .ok_or_else(|| "`break` outside of a loop".to_string())?;
                self.asm.jmp(l);
                Ok(())
            }
            Stmt::Continue => {
                let l = *self
                    .continues
                    .last()
                    .ok_or_else(|| "`continue` outside of a loop".to_string())?;
                self.asm.jmp(l);
                Ok(())
            }
            Stmt::Expr(e) => {
                self.expr(e)?;
                Ok(())
            }
            Stmt::Block(b) => self.stmts(b),
            Stmt::Asm(raw) => self.emit_raw(raw),
            Stmt::Func(_) | Stmt::Struct(_) => Ok(()),
        }
    }

    fn for_loop(
        &mut self,
        init: &Stmt,
        cond: &Expr,
        update: &Stmt,
        body: &[Stmt],
    ) -> Result<(), String> {
        self.scopes.push(Vec::new());
        let mut err = None;
        if let Err(e) = self.stmt(init) {
            err = Some(e);
        }
        if err.is_none() {
            let top = self.asm.label();
            let upd = self.asm.label();
            let end = self.asm.label();
            self.asm.bind(top);
            let r = (|| -> Result<(), String> {
                self.expr(cond)?;
                self.asm.test_rr(RAX, RAX);
                self.asm.jcc(CC_E, end);
                self.breaks.push(end);
                self.continues.push(upd);
                let r = self.stmts(body);
                self.breaks.pop();
                self.continues.pop();
                r?;
                self.asm.bind(upd);
                self.stmt(update)?;
                self.asm.jmp(top);
                self.asm.bind(end);
                Ok(())
            })();
            if let Err(e) = r {
                err = Some(e);
            }
        }
        self.scopes.pop();
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn emit_raw(&mut self, raw: &str) -> Result<(), String> {
        for tok in raw.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
            let t = tok.trim();
            if t.is_empty() {
                continue;
            }
            let t = t
                .strip_prefix("0x")
                .or_else(|| t.strip_prefix("0X"))
                .unwrap_or(t);
            let b = u8::from_str_radix(t, 16)
                .map_err(|_| format!("asm: '{}' is not a hex byte", tok))?;
            self.asm.raw(&[b]);
        }
        Ok(())
    }

    // ---------------- expressions ----------------

    fn type_of(&self, e: &Expr) -> Result<Type, String> {
        match e {
            Expr::Int(_) => Ok(Type::Int),
            Expr::Float(_) => Ok(Type::Float),
            Expr::Str(_) => Ok(Type::String),
            Expr::Char(_) => Ok(Type::Char),
            Expr::Bool(_) => Ok(Type::Bool),
            Expr::Null => Ok(Type::Ptr(Box::new(Type::Void))),
            Expr::Ident(n) => {
                if let Some((t, _)) = self.find_local(n) {
                    Ok(t)
                } else if let Some((t, _)) = self.globals.get(n) {
                    Ok(t.clone())
                } else {
                    Err(format!("undefined variable '{}'", n))
                }
            }
            Expr::Binary(op, l, r) => match op {
                BinOp::And
                | BinOp::Or
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::Le
                | BinOp::Ge
                | BinOp::Eq
                | BinOp::Ne => Ok(Type::Bool),
                _ => {
                    let lt = self.type_of(l)?;
                    let rt = self.type_of(r)?;
                    if is_ptr(&lt) {
                        Ok(lt)
                    } else if is_ptr(&rt) && *op == BinOp::Add {
                        Ok(rt)
                    } else if lt == Type::Float || rt == Type::Float {
                        Ok(Type::Float)
                    } else {
                        Ok(Type::Int)
                    }
                }
            },
            Expr::Unary(op, x) => match op {
                UnOp::Neg => self.type_of(x),
                UnOp::Not => Ok(Type::Bool),
                UnOp::BitNot => Ok(Type::Int),
                UnOp::Deref => pointee(&self.type_of(x)?),
                UnOp::AddrOf => Ok(Type::Ptr(Box::new(self.type_of(x)?))),
            },
            Expr::Call(name, args) => self.call_type(name, args),
            Expr::Index(b, _) => match self.type_of(b)? {
                Type::Array(e, _) => Ok(*e),
                Type::Ptr(e) => Ok(*e),
                Type::String => Ok(Type::Char),
                t => Err(format!("cannot index type '{}'", t.display())),
            },
            Expr::Member(o, f) | Expr::Arrow(o, f) => {
                let ot = self.type_of(o)?;
                let s = match &ot {
                    Type::Named(s) => s.clone(),
                    Type::Ptr(inner) => match &**inner {
                        Type::Named(s) => s.clone(),
                        _ => return Err(format!("type '{}' has no fields", ot.display())),
                    },
                    _ => return Err(format!("type '{}' has no fields", ot.display())),
                };
                Ok(self.field_info(&s, f)?.1)
            }
            Expr::Assign(_, v) | Expr::AssignLValue(_, v) => self.type_of(v),
            Expr::Cast(t, _) => Ok(t.clone()),
            Expr::Sizeof(_) => Ok(Type::Int),
        }
    }

    fn expr(&mut self, e: &Expr) -> Result<Type, String> {
        match e {
            Expr::Int(v) => {
                self.asm.mov_imm64(RAX, *v);
                Ok(Type::Int)
            }
            Expr::Float(_) => Err(
                "floating-point values are not supported by the native backend yet".into(),
            ),
            Expr::Str(s) => {
                let o = self.intern(s)?;
                self.asm.lea_rip(RAX, Fixup::Rdata(o));
                Ok(Type::String)
            }
            Expr::Char(c) => {
                self.asm.mov_imm64(RAX, *c as i64);
                Ok(Type::Char)
            }
            Expr::Bool(b) => {
                self.asm.mov_imm64(RAX, *b as i64);
                Ok(Type::Bool)
            }
            Expr::Null => {
                self.asm.mov_imm64(RAX, 0);
                Ok(Type::Ptr(Box::new(Type::Void)))
            }
            Expr::Ident(name) => {
                if let Some((t, off)) = self.find_local(name) {
                    let d = -(off as i32);
                    match self.size_of(&t)? {
                        1 => self.asm.load8zx(RAX, RBP, d),
                        n if n > 8 => self.asm.lea(RAX, RBP, d),
                        _ => self.asm.load(RAX, RBP, d),
                    }
                    Ok(t)
                } else if let Some((t, off)) = self.globals.get(name).cloned() {
                    let fix = Fixup::Data(off);
                    if self.size_of(&t)? == 1 {
                        self.asm.load8zx_rip(RAX, fix);
                    } else {
                        self.asm.load_rip(RAX, fix);
                    }
                    Ok(t)
                } else {
                    Err(format!("undefined variable '{}'", name))
                }
            }
            Expr::Binary(op, l, r) => self.binary(*op, l, r),
            Expr::Unary(op, x) => self.unary(*op, x),
            Expr::Call(name, args) => self.call(name, args),
            Expr::Index(_, _) | Expr::Member(_, _) | Expr::Arrow(_, _) => {
                let t = self.addr(e)?;
                self.load_at_rax(&t)?;
                Ok(t)
            }
            Expr::Assign(name, v) => {
                if let Some((t, off)) = self.find_local(name) {
                    self.expr(v)?;
                    self.store_slot(off, &t)?;
                    Ok(t)
                } else if let Some((t, off)) = self.globals.get(name).cloned() {
                    self.expr(v)?;
                    self.store_global(off, &t)?;
                    Ok(t)
                } else {
                    Err(format!("assignment to undefined variable '{}'", name))
                }
            }
            Expr::AssignLValue(lv, v) => {
                let t = self.addr(lv)?;
                self.asm.push_r(RAX);
                self.depth += 8;
                self.expr(v)?;
                self.asm.pop_r(RCX);
                self.depth -= 8;
                match self.size_of(&t)? {
                    1 => self.asm.store8(RCX, 0, RAX),
                    8 => self.asm.store(RCX, 0, RAX),
                    n => return Err(format!("cannot assign a {}-byte aggregate", n)),
                }
                Ok(t)
            }
            Expr::Cast(t, x) => {
                self.expr(x)?;
                Ok(t.clone())
            }
            Expr::Sizeof(t) => {
                let s = self.size_of(t)?;
                self.asm.mov_imm64(RAX, s as i64);
                Ok(Type::Int)
            }
        }
    }

    fn scale(&mut self, reg: u8, t: &Type) -> Result<(), String> {
        let esz = self.size_of(&pointee(t)?)? as i64;
        if esz > 1 {
            self.asm.mov_imm64(RCX, esz);
            self.asm.imul_rr(reg, RCX);
        }
        Ok(())
    }

    fn binary(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<Type, String> {
        match op {
            BinOp::And => {
                let f = self.asm.label();
                let end = self.asm.label();
                self.expr(l)?;
                self.asm.test_rr(RAX, RAX);
                self.asm.jcc(CC_E, f);
                self.expr(r)?;
                self.asm.test_rr(RAX, RAX);
                self.asm.jcc(CC_E, f);
                self.asm.mov_imm64(RAX, 1);
                self.asm.jmp(end);
                self.asm.bind(f);
                self.asm.mov_imm64(RAX, 0);
                self.asm.bind(end);
                Ok(Type::Bool)
            }
            BinOp::Or => {
                let t = self.asm.label();
                let end = self.asm.label();
                self.expr(l)?;
                self.asm.test_rr(RAX, RAX);
                self.asm.jcc(CC_NE, t);
                self.expr(r)?;
                self.asm.test_rr(RAX, RAX);
                self.asm.jcc(CC_NE, t);
                self.asm.mov_imm64(RAX, 0);
                self.asm.jmp(end);
                self.asm.bind(t);
                self.asm.mov_imm64(RAX, 1);
                self.asm.bind(end);
                Ok(Type::Bool)
            }
            _ => self.binary_arith(op, l, r),
        }
    }

    fn binary_arith(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<Type, String> {
        let lt = self.expr(l)?;
        self.asm.push_r(RAX);
        self.depth += 8;
        let rt = self.expr(r)?;
        self.asm.mov_rr(RDX, RAX);
        self.asm.pop_r(RAX);
        self.depth -= 8;

        match op {
            BinOp::Add => {
                if is_ptr(&lt) && !is_ptr(&rt) {
                    self.scale(RDX, &lt)?;
                    self.asm.add_rr(RAX, RDX);
                    Ok(lt)
                } else if is_ptr(&rt) && !is_ptr(&lt) {
                    self.scale(RAX, &rt)?;
                    self.asm.add_rr(RAX, RDX);
                    Ok(rt)
                } else {
                    self.asm.add_rr(RAX, RDX);
                    Ok(Type::Int)
                }
            }
            BinOp::Sub => {
                if is_ptr(&lt) && !is_ptr(&rt) {
                    self.scale(RDX, &lt)?;
                }
                self.asm.sub_rr(RAX, RDX);
                Ok(if is_ptr(&lt) { lt } else { Type::Int })
            }
            BinOp::Mul => {
                self.asm.imul_rr(RAX, RDX);
                Ok(Type::Int)
            }
            BinOp::Div | BinOp::Mod => {
                let unsigned = matches!(lt, Type::UInt)
                    || matches!(rt, Type::UInt)
                    || is_ptr(&lt)
                    || is_ptr(&rt);
                self.asm.mov_rr(RCX, RDX);
                if unsigned {
                    self.asm.xor_rr(RDX, RDX);
                    self.asm.div_r(RCX);
                } else {
                    self.asm.cqo();
                    self.asm.idiv_r(RCX);
                }
                if op == BinOp::Mod {
                    self.asm.mov_rr(RAX, RDX);
                }
                Ok(Type::Int)
            }
            BinOp::BitAnd => {
                self.asm.and_rr(RAX, RDX);
                Ok(Type::Int)
            }
            BinOp::BitOr => {
                self.asm.or_rr(RAX, RDX);
                Ok(Type::Int)
            }
            BinOp::BitXor => {
                self.asm.xor_rr(RAX, RDX);
                Ok(Type::Int)
            }
            BinOp::Shl => {
                self.asm.mov_rr(RCX, RDX);
                self.asm.shl_cl(RAX);
                Ok(Type::Int)
            }
            BinOp::Shr => {
                self.asm.mov_rr(RCX, RDX);
                if matches!(lt, Type::UInt) {
                    self.asm.shr_cl(RAX);
                } else {
                    self.asm.sar_cl(RAX);
                }
                Ok(Type::Int)
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                self.asm.cmp_rr(RAX, RDX);
                let unsigned = matches!(lt, Type::UInt) || is_ptr(&lt);
                let cc = match op {
                    BinOp::Eq => CC_E,
                    BinOp::Ne => CC_NE,
                    BinOp::Lt => {
                        if unsigned {
                            CC_B
                        } else {
                            CC_L
                        }
                    }
                    BinOp::Gt => {
                        if unsigned {
                            CC_A
                        } else {
                            CC_G
                        }
                    }
                    BinOp::Le => {
                        if unsigned {
                            CC_BE
                        } else {
                            CC_LE
                        }
                    }
                    _ => {
                        if unsigned {
                            CC_AE
                        } else {
                            CC_GE
                        }
                    }
                };
                self.asm.setcc(cc);
                self.asm.movzx_rr8(RAX, RAX);
                Ok(Type::Bool)
            }
            _ => Err(format!("operator '{}' is not supported", op.c())),
        }
    }

    fn unary(&mut self, op: UnOp, x: &Expr) -> Result<Type, String> {
        match op {
            UnOp::Neg => {
                let t = self.expr(x)?;
                self.asm.neg_r(RAX);
                Ok(t)
            }
            UnOp::Not => {
                self.expr(x)?;
                self.asm.test_rr(RAX, RAX);
                self.asm.setcc(CC_E);
                self.asm.movzx_rr8(RAX, RAX);
                Ok(Type::Bool)
            }
            UnOp::BitNot => {
                self.expr(x)?;
                self.asm.not_r(RAX);
                Ok(Type::Int)
            }
            UnOp::Deref => {
                let p = pointee(&self.type_of(x)?)?;
                self.expr(x)?;
                self.load_at_rax(&p)?;
                Ok(p)
            }
            UnOp::AddrOf => self.addr(x),
        }
    }

    fn load_at_rax(&mut self, t: &Type) -> Result<(), String> {
        match self.size_of(t)? {
            1 => self.asm.load8zx(RAX, RAX, 0),
            8 => self.asm.load(RAX, RAX, 0),
            _ => {}
        }
        Ok(())
    }

    fn addr(&mut self, e: &Expr) -> Result<Type, String> {
        match e {
            Expr::Ident(name) => {
                if let Some((t, off)) = self.find_local(name) {
                    self.asm.lea(RAX, RBP, -(off as i32));
                    Ok(t)
                } else if let Some((t, off)) = self.globals.get(name).cloned() {
                    self.asm.lea_rip(RAX, Fixup::Data(off));
                    Ok(t)
                } else {
                    Err(format!("undefined variable '{}'", name))
                }
            }
            Expr::Unary(UnOp::Deref, x) => {
                let p = pointee(&self.type_of(x)?)?;
                self.expr(x)?;
                Ok(p)
            }
            Expr::Member(obj, field) => {
                let ot = self.type_of(obj)?;
                match &ot {
                    Type::Named(s) => {
                        let (fo, ft) = self.field_info(s, field)?;
                        self.addr(obj)?;
                        if fo != 0 {
                            self.asm.add_imm32(RAX, fo as i32);
                        }
                        Ok(ft)
                    }
                    Type::Ptr(inner) => {
                        let s = match &**inner {
                            Type::Named(s) => s.clone(),
                            _ => return Err(format!("type '{}' has no fields", ot.display())),
                        };
                        let (fo, ft) = self.field_info(&s, field)?;
                        self.expr(obj)?;
                        if fo != 0 {
                            self.asm.add_imm32(RAX, fo as i32);
                        }
                        Ok(ft)
                    }
                    _ => Err(format!("type '{}' has no fields", ot.display())),
                }
            }
            Expr::Arrow(p, field) => {
                let inner = pointee(&self.type_of(p)?)?;
                let s = match &inner {
                    Type::Named(s) => s.clone(),
                    _ => return Err(format!("type '{}' has no fields", inner.display())),
                };
                let (fo, ft) = self.field_info(&s, field)?;
                self.expr(p)?;
                if fo != 0 {
                    self.asm.add_imm32(RAX, fo as i32);
                }
                Ok(ft)
            }
            Expr::Index(base, idx) => {
                let bt = self.type_of(base)?;
                let (elem, is_arr) = match &bt {
                    Type::Array(e, _) => ((**e).clone(), true),
                    Type::Ptr(e) => ((**e).clone(), false),
                    Type::String => (Type::Char, false),
                    _ => return Err(format!("cannot index type '{}'", bt.display())),
                };
                if is_arr {
                    self.addr(base)?;
                } else {
                    self.expr(base)?;
                }
                self.asm.push_r(RAX);
                self.depth += 8;
                self.expr(idx)?;
                let esz = self.size_of(&elem)? as i64;
                if esz > 1 {
                    self.asm.mov_imm64(RCX, esz);
                    self.asm.imul_rr(RAX, RCX);
                }
                self.asm.pop_r(RCX);
                self.depth -= 8;
                self.asm.add_rr(RAX, RCX);
                Ok(elem)
            }
            _ => Err("expression is not assignable".into()),
        }
    }

    // ---------------- calls and printing ----------------

    fn call_rt(&mut self, name: &str) -> Result<(), String> {
        let pad = if (self.depth / 8) % 2 == 1 { 8u32 } else { 0 };
        if pad != 0 {
            self.asm.sub_rsp(8);
            self.depth += 8;
        }
        let l = *self
            .rt
            .get(name)
            .ok_or_else(|| format!("internal: missing runtime '{}'", name))?;
        self.asm.call_rel(l);
        if pad != 0 {
            self.asm.add_rsp(8);
            self.depth -= 8;
        }
        Ok(())
    }

    fn print_newline(&mut self) -> Result<(), String> {
        let o = self.intern("\n")?;
        self.asm.lea_rip(RCX, Fixup::Rdata(o));
        self.asm.mov_imm64(RDX, 1);
        self.call_rt("bs_write")
    }

    fn print_value(&mut self, t: &Type) -> Result<(), String> {
        match t {
            Type::String => {
                self.asm.mov_rr(RCX, RAX);
                self.call_rt("bs_print_str")
            }
            Type::Int => {
                self.asm.mov_rr(RCX, RAX);
                self.call_rt("bs_print_int")
            }
            Type::UInt | Type::Byte => {
                self.asm.mov_rr(RCX, RAX);
                self.call_rt("bs_print_uint")
            }
            Type::Char => {
                self.asm.store_rip8(RAX, Fixup::Data(D_CHARBUF));
                self.asm.lea_rip(RCX, Fixup::Data(D_CHARBUF));
                self.asm.mov_imm64(RDX, 1);
                self.call_rt("bs_write")
            }
            Type::Bool => {
                let is_true = self.asm.label();
                let go = self.asm.label();
                self.asm.test_rr(RAX, RAX);
                self.asm.jcc(CC_NE, is_true);
                let f = self.intern("false")?;
                self.asm.lea_rip(RCX, Fixup::Rdata(f));
                self.asm.jmp(go);
                self.asm.bind(is_true);
                let tr = self.intern("true")?;
                self.asm.lea_rip(RCX, Fixup::Rdata(tr));
                self.asm.bind(go);
                self.call_rt("bs_print_str")
            }
            Type::Ptr(_) | Type::Named(_) | Type::Array(_, _) => {
                self.asm.mov_rr(RCX, RAX);
                self.call_rt("bs_print_hex")
            }
            Type::Float => Err("printing floats is not supported by the native backend".into()),
            Type::Void => Err("cannot print a void expression".into()),
        }
    }

    fn call(&mut self, name: &str, args: &[Expr]) -> Result<Type, String> {
        match name {
            "print" | "println" => {
                for a in args {
                    let t = self.expr(a)?;
                    self.print_value(&t)?;
                }
                self.print_newline()?;
                Ok(Type::Void)
            }
            "len" | "strlen" => {
                if args.len() != 1 {
                    return Err(format!("{}() expects 1 argument", name));
                }
                self.expr(&args[0])?;
                self.asm.mov_rr(RCX, RAX);
                self.call_rt("bs_strlen")?;
                Ok(Type::Int)
            }
            "malloc" => {
                if args.len() != 1 {
                    return Err("malloc() expects 1 argument".into());
                }
                self.expr(&args[0])?;
                self.asm.mov_rr(RCX, RAX);
                self.call_rt("bs_malloc")?;
                Ok(Type::Ptr(Box::new(Type::Void)))
            }
            "free" => {
                if args.len() != 1 {
                    return Err("free() expects 1 argument".into());
                }
                self.expr(&args[0])?;
                self.asm.mov_rr(RCX, RAX);
                self.call_rt("bs_free")?;
                Ok(Type::Void)
            }
            "to_int" => {
                if args.len() != 1 {
                    return Err("to_int() expects 1 argument".into());
                }
                self.expr(&args[0])?;
                Ok(Type::Int)
            }
            "to_float" => Err("to_float() is not supported by the native backend".into()),
            _ => self.call_user(name, args),
        }
    }

    fn call_user(&mut self, name: &str, args: &[Expr]) -> Result<Type, String> {
        let f = self
            .fns
            .get(name)
            .cloned()
            .ok_or_else(|| format!("undefined function '{}'", name))?;
        if f.params.len() != args.len() {
            return Err(format!(
                "function '{}' expects {} argument(s), got {}",
                name,
                f.params.len(),
                args.len()
            ));
        }
        let lbl = *self.fn_labels.get(name).unwrap();
        let n = args.len();
        let pad = if ((self.depth / 8) + n) % 2 == 1 {
            8u32
        } else {
            0
        };
        if pad != 0 {
            self.asm.sub_rsp(8);
            self.depth += 8;
        }
        for a in args.iter().rev() {
            self.expr(a)?;
            self.asm.push_r(RAX);
            self.depth += 8;
        }
        let regs = [RCX, RDX, R8, R9];
        let nreg = n.min(4);
        for i in 0..nreg {
            self.asm.pop_r(regs[i]);
            self.depth -= 8;
        }
        self.asm.sub_rsp(32);
        self.depth += 32;
        self.asm.call_rel(lbl);
        let cleanup = 32 + ((n - nreg) as u32) * 8 + pad;
        self.asm.add_rsp(cleanup);
        self.depth -= cleanup as usize;
        Ok(f.ret)
    }
}
