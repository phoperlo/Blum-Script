//! AST -> IR lowering.
//!
//! The lowering mirrors the legacy backend instruction for instruction so that
//! both paths produce the same observable behaviour (which is what makes the
//! `--no-ir` A/B switch usable as a regression test).  What changes is the
//! shape: values become virtual registers instead of "always RAX/XMM0", and
//! control flow becomes explicit basic blocks.
//!
//! Two things are deliberately *not* lowered here:
//!
//! * **Inline assembly** вЂ” `asm { }` blocks are tied to the legacy frame
//!   bookkeeping, so any function containing one is compiled by the legacy
//!   backend (`native::emit_fn` checks this).
//! * **Aggregates as values** вЂ” a struct or array expression always evaluates
//!   to its *address* (exactly like the legacy backend).  Promoted registers
//!   therefore only ever hold scalars.

use std::collections::{HashMap, HashSet};

use crate::abi::*;
use crate::ast::*;
use crate::ir::*;
use crate::native::StructInfo;
use crate::types::Type;

// ---------------------------------------------------------------------------
// context
// ---------------------------------------------------------------------------

pub struct Ctx<'a> {
    pub structs: &'a HashMap<String, StructInfo>,
    pub fns: &'a HashMap<String, FuncDecl>,
    pub globals: &'a HashMap<String, (Type, u32)>,
}

/// Owns the read-only pools; the compiler passes disjoint field borrows so the
/// interner can be `&mut` while the context is `&`.
#[allow(dead_code)]
pub struct Interner<'a> {
    pub strings: &'a mut HashMap<String, u32>,
    pub fconsts: &'a mut HashMap<u64, u32>,
    pub rdata: &'a mut Vec<u8>,
}

#[allow(dead_code)]
impl<'a> Interner<'a> {
    pub fn str_off(&mut self, s: &str) -> u32 {
        if let Some(o) = self.strings.get(s) {
            return *o;
        }
        let off = self.rdata.len() as u32;
        self.rdata.extend_from_slice(s.as_bytes());
        self.rdata.push(0);
        self.strings.insert(s.to_string(), off);
        off
    }

    pub fn f64_off(&mut self, v: f64) -> u32 {
        let bits = v.to_bits();
        if let Some(o) = self.fconsts.get(&bits) {
            return *o;
        }
        while self.rdata.len() % 8 != 0 {
            self.rdata.push(0);
        }
        let off = self.rdata.len() as u32;
        self.rdata.extend_from_slice(&bits.to_le_bytes());
        self.fconsts.insert(bits, off);
        off
    }
}

// ---------------------------------------------------------------------------
// type helpers
// ---------------------------------------------------------------------------

pub fn size_of(ctx: &Ctx, t: &Type) -> Result<usize, String> {
    Ok(match t {
        Type::Void => 0,
        Type::Bool | Type::Char | Type::Byte => 1,
        Type::Int | Type::UInt | Type::Float => 8,
        Type::String | Type::Ptr(_) => 8,
        Type::Array(e, n) => size_of(ctx, e)? * n,
        Type::Named(s) => ctx
            .structs
            .get(s)
            .ok_or_else(|| format!("unknown type '{}'", s))?
            .size,
    })
}

pub fn is_agg(t: &Type) -> bool {
    matches!(t, Type::Named(_) | Type::Array(_, _))
}

fn kind_of(ctx: &Ctx, t: &Type) -> Result<Kind, String> {
    Ok(match t {
        Type::Float => Kind::F64,
        Type::Bool | Type::Char | Type::Byte => Kind::I8,
        Type::UInt => Kind::U64,
        Type::String | Type::Ptr(_) => Kind::Ptr,
        Type::Named(_) | Type::Array(_, _) => Kind::Agg(size_of(ctx, t)? as u32),
        Type::Int | Type::Void => Kind::I64,
    })
}

fn is_ptr(t: &Type) -> bool {
    matches!(t, Type::Ptr(_) | Type::String)
}

fn pointee(t: &Type) -> Result<Type, String> {
    match t {
        Type::Ptr(inner) => Ok((**inner).clone()),
        Type::String => Ok(Type::Char),
        _ => Err(format!(
            "cannot dereference non-pointer type '{}'",
            t.display()
        )),
    }
}

// ---------------------------------------------------------------------------
// places / locals
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Loc {
    /// Addressable frame slot, RBP-relative displacement (negative).
    Slot(i32),
    /// Promoted scalar living in a virtual register.
    Reg(VReg),
}

struct Place {
    base: VReg,
    disp: i32,
    ty: Type,
}

// ---------------------------------------------------------------------------
// promotion analysis
// ---------------------------------------------------------------------------

fn walk_exprs<'a>(s: &'a [Stmt], f: &mut dyn FnMut(&Expr)) {
    for st in s {
        walk_stmt(st, f);
    }
}

fn walk_stmt(s: &Stmt, f: &mut dyn FnMut(&Expr)) {
    match s {
        Stmt::Let(_, _, e) | Stmt::Var(_, _, e) => walk_expr(e, f),
        Stmt::Assign(_, e) => walk_expr(e, f),
        Stmt::Expr(e) => walk_expr(e, f),
        Stmt::If(c, a, b) => {
            walk_expr(c, f);
            walk_exprs(a, f);
            walk_exprs(b, f);
        }
        Stmt::While(c, b) => {
            walk_expr(c, f);
            walk_exprs(b, f);
        }
        Stmt::For(i, c, u, b) => {
            walk_stmt(i, f);
            walk_expr(c, f);
            walk_stmt(u, f);
            walk_exprs(b, f);
        }
        Stmt::Return(Some(e)) => walk_expr(e, f),
        Stmt::Block(b) => walk_exprs(b, f),
        _ => {}
    }
}

fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    match e {
        Expr::Binary(_, a, b) | Expr::Index(a, b) | Expr::AssignLValue(a, b) => {
            walk_expr(a, f);
            walk_expr(b, f);
        }
        Expr::Unary(_, x) => walk_expr(x, f),
        Expr::Call(_, args) => {
            for a in args {
                walk_expr(a, f);
            }
        }
        Expr::Member(o, _) | Expr::Arrow(o, _) | Expr::Assign(_, o) => walk_expr(o, f),
        Expr::Cast(_, x) => walk_expr(x, f),
        _ => {}
    }
}

/// A local is promoted to a virtual register when it is declared exactly once
/// and its address is never taken.  That is enough for the common case
/// (`var i = 0` loop counters, accumulators) and is trivially sound.
fn promotable(f: &FuncDecl) -> HashSet<String> {
    let mut count: HashMap<String, usize> = HashMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    let mut declared: HashSet<String> = HashSet::new();

    fn decls(s: &[Stmt], count: &mut HashMap<String, usize>, declared: &mut HashSet<String>) {
        for st in s {
            match st {
                Stmt::Let(n, _, _) | Stmt::Var(n, _, _) => {
                    *count.entry(n.clone()).or_insert(0) += 1;
                    declared.insert(n.clone());
                }
                Stmt::If(_, a, b) => {
                    decls(a, count, declared);
                    decls(b, count, declared);
                }
                Stmt::While(_, b) => decls(b, count, declared),
                Stmt::For(i, _, _, b) => {
                    decls(std::slice::from_ref(i.as_ref()), count, declared);
                    decls(b, count, declared);
                }
                Stmt::Block(b) => decls(b, count, declared),
                _ => {}
            }
        }
    }
    decls(&f.body, &mut count, &mut declared);

    walk_exprs(&f.body, &mut |e| {
        if let Expr::Unary(UnOp::AddrOf, x) = e {
            if let Expr::Ident(n) = &**x {
                taken.insert(n.clone());
            }
        }
    });

    declared
        .into_iter()
        .filter(|n| count.get(n).copied().unwrap_or(0) == 1 && !taken.contains(n))
        .collect()
}

/// Does this function contain inline assembly?  If so the legacy backend is
/// used, because `asm { }` resolves names to concrete frame offsets.
pub fn has_inline_asm(body: &[Stmt]) -> bool {
    let mut found = false;
    fn go(s: &[Stmt], found: &mut bool) {
        for st in s {
            match st {
                Stmt::Asm(_) => *found = true,
                Stmt::If(_, a, b) => {
                    go(a, found);
                    go(b, found);
                }
                Stmt::While(_, b) => go(b, found),
                Stmt::For(_, _, _, b) => go(b, found),
                Stmt::Block(b) => go(b, found),
                _ => {}
            }
        }
    }
    go(body, &mut found);
    found
}

// ---------------------------------------------------------------------------
// lowering
// ---------------------------------------------------------------------------

struct Gen<'a, 'b, 'c> {
    ctx: &'a Ctx<'a>,
    it: &'b mut Interner<'c>,
    b: Builder,
    scopes: Vec<Vec<(String, Type, Loc)>>,
    promo: HashSet<String>,
    breaks: Vec<BlockId>,
    continues: Vec<BlockId>,
    ret_ty: Type,
    ret_slot: Option<i32>,
    variadic: bool,
}

pub fn lower(f: &FuncDecl, ctx: &Ctx, it: &mut Interner) -> Result<Function, String> {
    let ret_sz = size_of(ctx, &f.ret)?;
    let ret_by_ref = is_agg(&f.ret) && ret_sz > 8;
    let ret_kind = kind_of(ctx, &f.ret)?;

    let mut g = Gen {
        ctx,
        it,
        b: Builder::new(&f.name, ret_kind, f.variadic),
        scopes: vec![Vec::new()],
        promo: promotable(f),
        breaks: Vec::new(),
        continues: Vec::new(),
        ret_ty: f.ret.clone(),
        ret_slot: None,
        variadic: f.variadic,
    };
    g.b.label(&format!("entry of {}", f.name));

    let gpr_start = if ret_by_ref { 1 } else { 0 };
    if ret_by_ref {
        let d = g.b.alloc(ret_sz);
        g.b.func.param_homes.push(Home::Gpr(1 /*RCX*/, d, 8));
        g.b.func.ret_buf = Some((d, ret_sz as u32));
        g.ret_slot = Some(d);
    }

    // ---- parameter homing ------------------------------------------------
    let ptypes: Vec<Type> = f.params.iter().map(|(_, t)| t.clone()).collect();
    let pkinds: Vec<Kind> = ptypes
        .iter()
        .map(|t| kind_of(ctx, t))
        .collect::<Result<_, String>>()?;
    let slots = abi_slots(&pkinds, gpr_start);
    let regs = [1u8, 2, 8, 9]; // RCX, RDX, R8, R9
    let xmms = [0u8, 1, 2, 3];

    if f.variadic {
        // C varargs: every argument sits in a positional slot, and the four
        // incoming integer registers are homed into the shadow space so all
        // positional slots are contiguous at [rbp + 16 + 8*pos].  Declared
        // parameters are then copied from there into their frame slots.
        for (k, r) in regs.iter().enumerate() {
            g.b.func
                .param_homes
                .push(Home::Gpr(*r, 16 + (k as i32) * 8, 8));
        }
    }

    for (i, ((name, ty), slot)) in f.params.iter().zip(slots.iter()).enumerate() {
        let sz = size_of(ctx, ty)?;
        let k = kind_of(ctx, ty)?;
        let big = is_agg(ty) && sz > 8;
        // the value slot: params are addressable in the callee frame
        let vslot = g.b.alloc(sz.max(1));

        // where the incoming bytes can be found
        let mut indirect: Option<i32> = None;
        if f.variadic {
            // positional slot: shadow space for the first four, then the
            // caller's stack area, both contiguous at [rbp + 16 + 8*pos]
            indirect = Some(16 + (gpr_start + i) as i32 * 8);
        } else {
            match *slot {
                Slot::Gpr(idx) => {
                    if big {
                        let tmp = g.b.alloc(8);
                        g.b.func.param_homes.push(Home::Gpr(regs[idx], tmp, 8));
                        indirect = Some(tmp);
                    } else {
                        let w = if k.is_float() { 8 } else { sz.clamp(1, 8) as u8 };
                        g.b.func.param_homes.push(Home::Gpr(regs[idx], vslot, w));
                    }
                }
                Slot::Xmm(idx) => {
                    g.b.func.param_homes.push(Home::Xmm(xmms[idx], vslot));
                }
                Slot::Stack(s) => {
                    // [rbp+8] return address, [rbp+16..48) shadow space
                    indirect = Some(48 + 8 * s as i32);
                }
            }
        }

        if let Some(src) = indirect {
            if big {
                let p = g.b.vreg(Kind::Ptr);
                g.b.push(Inst::Load {
                    dst: p,
                    base: FRAME,
                    disp: src,
                    kind: Kind::Ptr,
                });
                let a = g.addr_of(&Place {
                    base: FRAME,
                    disp: vslot,
                    ty: ty.clone(),
                })?;
                g.b.push(Inst::MemCopy {
                    dst: a,
                    src: p,
                    bytes: sz as u32,
                });
            } else {
                let t = g.b.vreg(k);
                g.b.push(Inst::Load {
                    dst: t,
                    base: FRAME,
                    disp: src,
                    kind: k,
                });
                g.b.push(Inst::Store {
                    base: FRAME,
                    disp: vslot,
                    src: t,
                    kind: k,
                });
            }
        }

        let loc = if g.promo.contains(name) && !k.is_agg() {
            let r = g.b.vreg(k);
            g.b.push(Inst::Load {
                dst: r,
                base: FRAME,
                disp: vslot,
                kind: k,
            });
            Loc::Reg(r)
        } else {
            Loc::Slot(vslot)
        };
        let _ = i;
        g.scopes
            .last_mut()
            .unwrap()
            .push((name.clone(), ty.clone(), loc));
    }

    // ---- body ------------------------------------------------------------
    let body = f.body.clone();
    g.stmts(&body)?;

    // every block needs a terminator: a block that falls off the end of the
    // function returns the default value
    for bi in 0..g.b.func.blocks.len() {
        if g.b.func.blocks[bi].term.is_none() {
            g.b.switch(bi as u32);
            let z = if ret_kind.is_float() {
                let d = g.b.vreg(Kind::F64);
                g.b.push(Inst::FConst(d, 0.0));
                d
            } else {
                let d = g.b.vreg(Kind::I64);
                g.b.push(Inst::Const(d, 0));
                d
            };
            g.b.set_term(Term::Ret(Some(z)));
        }
    }

    Ok(g.b.finish())
}

impl<'a, 'b, 'c> Gen<'a, 'b, 'c> {
    // -- basics ------------------------------------------------------------

    fn size_of(&self, t: &Type) -> Result<usize, String> {
        size_of(self.ctx, t)
    }
    fn kind_of(&self, t: &Type) -> Result<Kind, String> {
        kind_of(self.ctx, t)
    }

    #[allow(dead_code)]
    fn vreg(&mut self, k: Kind) -> VReg {
        self.b.vreg(k)
    }

    fn unit(&mut self) -> VReg {
        let d = self.b.vreg(Kind::I64);
        self.b.push(Inst::Const(d, 0));
        d
    }

    fn find_local(&self, n: &str) -> Option<(Type, Loc)> {
        for s in self.scopes.iter().rev() {
            for (name, t, l) in s.iter().rev() {
                if name == n {
                    return Some((t.clone(), *l));
                }
            }
        }
        None
    }

    fn field_info(&self, s: &str, f: &str) -> Result<(usize, Type), String> {
        let si = self
            .ctx
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

    /// Only set a terminator when the current block is still open.
    fn fallthrough(&mut self, target: BlockId) {
        if self.b.term().is_none() {
            self.b.set_term(Term::Goto(target));
        }
    }

    // -- type inference ----------------------------------------------------

    fn ty(&self, e: &Expr) -> Result<Type, String> {
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
                } else if let Some((t, _)) = self.ctx.globals.get(n) {
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
                    let lt = self.ty(l)?;
                    let rt = self.ty(r)?;
                    if is_ptr(&lt) {
                        Ok(lt)
                    } else if is_ptr(&rt) && *op == BinOp::Add {
                        Ok(rt)
                    } else if matches!(lt, Type::Float) || matches!(rt, Type::Float) {
                        Ok(Type::Float)
                    } else {
                        Ok(Type::Int)
                    }
                }
            },
            Expr::Unary(op, x) => match op {
                UnOp::Neg => self.ty(x),
                UnOp::Not => Ok(Type::Bool),
                UnOp::BitNot => Ok(Type::Int),
                UnOp::Deref => pointee(&self.ty(x)?),
                UnOp::AddrOf => Ok(Type::Ptr(Box::new(self.ty(x)?))),
            },
            Expr::Call(name, args) => self.call_type(name, args),
            Expr::Index(b, _) => match self.ty(b)? {
                Type::Array(e, _) => Ok(*e),
                Type::Ptr(e) => Ok(*e),
                Type::String => Ok(Type::Char),
                t => Err(format!("cannot index type '{}'", t.display())),
            },
            Expr::Member(o, f) | Expr::Arrow(o, f) => {
                let ot = self.ty(o)?;
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
            Expr::Assign(_, v) | Expr::AssignLValue(_, v) => self.ty(v),
            Expr::Cast(t, _) => Ok(t.clone()),
            Expr::Sizeof(_) => Ok(Type::Int),
        }
    }

    fn call_type(&self, name: &str, args: &[Expr]) -> Result<Type, String> {
        match name {
            "print" | "println" => Ok(Type::Void),
            "len" | "strlen" => Ok(Type::Int),
            "malloc" => Ok(Type::Ptr(Box::new(Type::Void))),
            "free" => Ok(Type::Void),
            "to_int" => Ok(Type::Int),
            "to_float" => Ok(Type::Float),
            "read_str" => Ok(Type::String),
            "read_int" => Ok(Type::Int),
            "read_char" => Ok(Type::Char),
            "va_int" => Ok(Type::Int),
            "va_float" => Ok(Type::Float),
            "open" => Ok(Type::Ptr(Box::new(Type::Void))),
            "read" | "write" | "file_size" => Ok(Type::Int),
            "close" => Ok(Type::Void),
            _ => {
                let f = self
                    .ctx
                    .fns
                    .get(name)
                    .ok_or_else(|| format!("undefined function '{}'", name))?;
                if f.variadic {
                    if args.len() < f.params.len() {
                        return Err(format!(
                            "function '{}' expects at least {} argument(s), got {}",
                            name,
                            f.params.len(),
                            args.len()
                        ));
                    }
                } else if f.params.len() != args.len() {
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

    // -- places ------------------------------------------------------------

    fn addr_of(&mut self, p: &Place) -> Result<VReg, String> {
        let d = self.b.vreg(Kind::Ptr);
        if p.base == FRAME {
            self.b.push(Inst::AddrLocal(d, p.disp));
        } else if p.base == DATA {
            self.b.push(Inst::AddrGlobal(d, p.disp as u32));
        } else if p.base == RDATA {
            self.b.push(Inst::AddrRdata(d, p.disp as u32));
        } else if p.disp == 0 {
            return Ok(p.base);
        } else {
            self.b.push(Inst::LeaBase {
                dst: d,
                base: p.base,
                disp: p.disp,
            });
        }
        Ok(d)
    }

    fn place(&mut self, e: &Expr) -> Result<Place, String> {
        match e {
            Expr::Ident(n) => {
                if let Some((t, loc)) = self.find_local(n) {
                    match loc {
                        Loc::Slot(d) => Ok(Place {
                            base: FRAME,
                            disp: d,
                            ty: t,
                        }),
                        Loc::Reg(_) => Err(format!(
                            "cannot take the address of promoted variable '{}'",
                            n
                        )),
                    }
                } else if let Some((t, off)) = self.ctx.globals.get(n) {
                    Ok(Place {
                        base: DATA,
                        disp: *off as i32,
                        ty: t.clone(),
                    })
                } else {
                    Err(format!("undefined variable '{}'", n))
                }
            }
            Expr::Unary(UnOp::Deref, x) => {
                let xt = self.ty(x)?;
                let (v, _) = self.value(x)?;
                Ok(Place {
                    base: v,
                    disp: 0,
                    ty: pointee(&xt)?,
                })
            }
            Expr::Member(obj, field) => {
                let ot = self.ty(obj)?;
                match &ot {
                    Type::Named(s) => {
                        let s = s.clone();
                        let (fo, ft) = self.field_info(&s, field)?;
                        let p = self.place(obj)?;
                        Ok(Place {
                            base: p.base,
                            disp: p.disp + fo as i32,
                            ty: ft,
                        })
                    }
                    Type::Ptr(inner) => {
                        let s = match &**inner {
                            Type::Named(s) => s.clone(),
                            _ => return Err(format!("type '{}' has no fields", ot.display())),
                        };
                        let (fo, ft) = self.field_info(&s, field)?;
                        let (v, _) = self.value(obj)?;
                        Ok(Place {
                            base: v,
                            disp: fo as i32,
                            ty: ft,
                        })
                    }
                    _ => Err(format!("type '{}' has no fields", ot.display())),
                }
            }
            Expr::Arrow(p, field) => {
                let pt = self.ty(p)?;
                let inner = pointee(&pt)?;
                let s = match &inner {
                    Type::Named(s) => s.clone(),
                    _ => return Err(format!("type '{}' has no fields", inner.display())),
                };
                let (fo, ft) = self.field_info(&s, field)?;
                let (v, _) = self.value(p)?;
                Ok(Place {
                    base: v,
                    disp: fo as i32,
                    ty: ft,
                })
            }
            Expr::Index(base, idx) => {
                let bt = self.ty(base)?;
                let (elem, is_arr) = match &bt {
                    Type::Array(e, _) => ((**e).clone(), true),
                    Type::Ptr(e) => ((**e).clone(), false),
                    Type::String => (Type::Char, false),
                    _ => return Err(format!("cannot index type '{}'", bt.display())),
                };
                let esz = self.size_of(&elem)?;

                // base pointer: either the address of an aggregate or a value
                let (base_v, extra) = if is_arr {
                    let p = self.place(base)?;
                    (self.addr_of(&p)?, 0)
                } else {
                    let (v, _) = self.value(base)?;
                    (v, 0)
                };

                let (iv, _) = self.value(idx)?;
                let d = self.b.vreg(Kind::Ptr);
                let shift = esz.trailing_zeros();
                if esz.is_power_of_two() && esz >= 1 && shift <= 3 {
                    self.b.push(Inst::Lea {
                        dst: d,
                        base: base_v,
                        index: iv,
                        scale: shift as u8,
                        disp: extra,
                    });
                } else {
                    let scaled = if esz <= 1 {
                        iv
                    } else {
                        let c = self.b.vreg(Kind::I64);
                        self.b.push(Inst::Const(c, esz as i64));
                        let s = self.b.vreg(Kind::I64);
                        self.b.push(Inst::Bin {
                            dst: s,
                            op: IBin::Mul,
                            a: iv,
                            b: c,
                        });
                        s
                    };
                    self.b.push(Inst::Lea {
                        dst: d,
                        base: base_v,
                        index: scaled,
                        scale: 0,
                        disp: extra,
                    });
                }
                Ok(Place {
                    base: d,
                    disp: 0,
                    ty: elem,
                })
            }
            _ => Err("expression is not assignable".into()),
        }
    }

    fn load_place(&mut self, p: &Place) -> Result<VReg, String> {
        let k = self.kind_of(&p.ty)?;
        if k.is_agg() {
            return self.addr_of(p);
        }
        let d = self.b.vreg(k);
        self.b.push(Inst::Load {
            dst: d,
            base: p.base,
            disp: p.disp,
            kind: k,
        });
        Ok(d)
    }

    // -- implicit conversions ---------------------------------------------

    fn coerce(&mut self, v: VReg, from: &Type, to: &Type) -> Result<VReg, String> {
        let ff = matches!(from, Type::Float);
        let tf = matches!(to, Type::Float);
        if ff == tf {
            return Ok(v);
        }
        let fk = if ff { Kind::F64 } else { Kind::I64 };
        let tk = if tf { Kind::F64 } else { Kind::I64 };
        let d = self.b.vreg(tk);
        self.b.push(Inst::Cast {
            dst: d,
            from: fk,
            to: tk,
            src: v,
        });
        Ok(d)
    }

    // -- variables ---------------------------------------------------------

    fn read_var(&mut self, n: &str) -> Result<(VReg, Type), String> {
        if let Some((t, loc)) = self.find_local(n) {
            match loc {
                Loc::Reg(r) => {
                    let k = self.kind_of(&t)?;
                    let d = self.b.vreg(k);
                    self.b.push(Inst::Copy(d, r));
                    Ok((d, t))
                }
                Loc::Slot(d) => {
                    let p = Place {
                        base: FRAME,
                        disp: d,
                        ty: t.clone(),
                    };
                    let v = self.load_place(&p)?;
                    Ok((v, t))
                }
            }
        } else if let Some((t, off)) = self.ctx.globals.get(n) {
            let p = Place {
                base: DATA,
                disp: *off as i32,
                ty: t.clone(),
            };
            let v = self.load_place(&p)?;
            Ok((v, t.clone()))
        } else {
            Err(format!("undefined variable '{}'", n))
        }
    }

    fn assign_var(&mut self, n: &str, val: &Expr) -> Result<(VReg, Type), String> {
        if let Some((t, loc)) = self.find_local(n) {
            let (v, vt) = self.value(val)?;
            let k = self.kind_of(&t)?;
            if k.is_agg() {
                let sz = self.size_of(&t)?;
                let p = match loc {
                    Loc::Slot(d) => Place {
                        base: FRAME,
                        disp: d,
                        ty: t.clone(),
                    },
                    Loc::Reg(_) => {
                        return Err(format!("cannot assign an aggregate to '{}'", n));
                    }
                };
                let d = self.addr_of(&p)?;
                self.b.push(Inst::MemCopy {
                    dst: d,
                    src: v,
                    bytes: sz as u32,
                });
                return Ok((v, t));
            }
            let cv = self.coerce(v, &vt, &t)?;
            match loc {
                Loc::Reg(r) => {
                    self.b.push(Inst::Copy(r, cv));
                    Ok((cv, t))
                }
                Loc::Slot(d) => {
                    self.b.push(Inst::Store {
                        base: FRAME,
                        disp: d,
                        src: cv,
                        kind: k,
                    });
                    Ok((cv, t))
                }
            }
        } else if let Some((t, off)) = self.ctx.globals.get(n).cloned() {
            let (v, vt) = self.value(val)?;
            let k = self.kind_of(&t)?;
            if k.is_agg() {
                let sz = self.size_of(&t)?;
                let d = self.b.vreg(Kind::Ptr);
                self.b.push(Inst::AddrGlobal(d, off));
                self.b.push(Inst::MemCopy {
                    dst: d,
                    src: v,
                    bytes: sz as u32,
                });
                return Ok((v, t));
            }
            let cv = self.coerce(v, &vt, &t)?;
            self.b.push(Inst::Store {
                base: DATA,
                disp: off as i32,
                src: cv,
                kind: k,
            });
            Ok((cv, t))
        } else {
            Err(format!("assignment to undefined variable '{}'", n))
        }
    }

    // -- statements --------------------------------------------------------

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

    fn declare(&mut self, name: &str, ty: &Option<Type>, init: &Expr) -> Result<(), String> {
        let t = match ty {
            Some(t) => t.clone(),
            None => self.ty(init)?,
        };
        let sz = self.size_of(&t)?;
        let k = self.kind_of(&t)?;
        let promote = self.promo.contains(name) && !k.is_agg();

        if k.is_agg() {
            let slot = self.b.alloc(sz.max(1));
            if matches!(init, Expr::Int(0)) {
                let p = Place {
                    base: FRAME,
                    disp: slot,
                    ty: t.clone(),
                };
                let a = self.addr_of(&p)?;
                self.b.push(Inst::Zero {
                    dst: a,
                    bytes: sz as u32,
                });
            } else {
                let (v, _) = self.value(init)?;
                let p = Place {
                    base: FRAME,
                    disp: slot,
                    ty: t.clone(),
                };
                let a = self.addr_of(&p)?;
                self.b.push(Inst::MemCopy {
                    dst: a,
                    src: v,
                    bytes: sz as u32,
                });
            }
            self.scopes
                .last_mut()
                .unwrap()
                .push((name.to_string(), t, Loc::Slot(slot)));
            return Ok(());
        }

        if promote {
            let r = self.b.vreg(k);
            let (v, vt) = self.value(init)?;
            let cv = self.coerce(v, &vt, &t)?;
            self.b.push(Inst::Copy(r, cv));
            self.scopes
                .last_mut()
                .unwrap()
                .push((name.to_string(), t, Loc::Reg(r)));
            return Ok(());
        }

        let slot = self.b.alloc(sz.max(1));
        let (v, vt) = self.value(init)?;
        let cv = self.coerce(v, &vt, &t)?;
        self.b.push(Inst::Store {
            base: FRAME,
            disp: slot,
            src: cv,
            kind: k,
        });
        self.scopes
            .last_mut()
            .unwrap()
            .push((name.to_string(), t, Loc::Slot(slot)));
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Let(n, ty, init) | Stmt::Var(n, ty, init) => self.declare(n, ty, init),
            Stmt::Assign(n, e) => {
                self.assign_var(n, e)?;
                Ok(())
            }
            Stmt::Expr(e) => {
                self.value(e)?;
                Ok(())
            }
            Stmt::If(cond, then_b, else_b) => self.lower_if(cond, then_b, else_b),
            Stmt::While(cond, body) => self.lower_while(cond, body),
            Stmt::For(init, cond, update, body) => self.lower_for(init, cond, update, body),
            Stmt::Return(v) => self.lower_return(v),
            Stmt::Break => {
                let l = *self
                    .breaks
                    .last()
                    .ok_or_else(|| "`break` outside of a loop".to_string())?;
                self.b.set_term(Term::Goto(l));
                self.b.block();
                Ok(())
            }
            Stmt::Continue => {
                let l = *self
                    .continues
                    .last()
                    .ok_or_else(|| "`continue` outside of a loop".to_string())?;
                self.b.set_term(Term::Goto(l));
                self.b.block();
                Ok(())
            }
            Stmt::Block(b) => {
                self.scopes.push(Vec::new());
                let r = self.stmts(b);
                self.scopes.pop();
                r
            }
            Stmt::Asm(_) => Err("inline asm is not supported by the IR backend".into()),
            Stmt::Func(_) | Stmt::Struct(_) => Ok(()),
        }
    }

    fn lower_if(&mut self, cond: &Expr, then_b: &[Stmt], else_b: &[Stmt]) -> Result<(), String> {
        let (cv, _) = self.value(cond)?;
        let cblk = self.b.cur();
        let t = self.b.block();
        let f = self.b.block();
        let end = self.b.block();
        self.b.switch(cblk);
        self.b.set_term(Term::Branch { cond: cv, t, f });
        self.b.switch(t);
        self.stmts(then_b)?;
        self.fallthrough(end);
        self.b.switch(f);
        self.stmts(else_b)?;
        self.fallthrough(end);
        self.b.switch(end);
        Ok(())
    }

    fn lower_while(&mut self, cond: &Expr, body: &[Stmt]) -> Result<(), String> {
        let pre = self.b.cur();
        let head = self.b.block();
        let exit = self.b.block();
        self.b.switch(pre);
        self.fallthrough(head);
        self.b.switch(head);
        let (cv, _) = self.value(cond)?;
        let cblk = self.b.cur();
        let body_b = self.b.block();
        self.b.switch(cblk);
        self.b.set_term(Term::Branch {
            cond: cv,
            t: body_b,
            f: exit,
        });
        self.b.switch(body_b);
        self.breaks.push(exit);
        self.continues.push(head);
        let r = self.stmts(body);
        self.breaks.pop();
        self.continues.pop();
        r?;
        self.fallthrough(head);
        self.b.switch(exit);
        Ok(())
    }

    fn lower_for(
        &mut self,
        init: &Stmt,
        cond: &Expr,
        update: &Stmt,
        body: &[Stmt],
    ) -> Result<(), String> {
        self.scopes.push(Vec::new());
        let r = (|| -> Result<(), String> {
            self.stmt(init)?;
            let pre = self.b.cur();
            let head = self.b.block();
            let body_b = self.b.block();
            let upd = self.b.block();
            let exit = self.b.block();
            self.b.switch(pre);
            self.fallthrough(head);
            self.b.switch(head);
            let (cv, _) = self.value(cond)?;
            let cblk = self.b.cur();
            self.b.switch(cblk);
            self.b.set_term(Term::Branch {
                cond: cv,
                t: body_b,
                f: exit,
            });
            self.b.switch(body_b);
            self.breaks.push(exit);
            self.continues.push(upd);
            let inner = self.stmts(body);
            self.breaks.pop();
            self.continues.pop();
            inner?;
            self.fallthrough(upd);
            self.b.switch(upd);
            self.stmt(update)?;
            self.fallthrough(head);
            self.b.switch(exit);
            Ok(())
        })();
        self.scopes.pop();
        r
    }

    fn lower_return(&mut self, v: &Option<Expr>) -> Result<(), String> {
        let rt = self.ret_ty.clone();
        let by_ref = is_agg(&rt) && self.size_of(&rt)? > 8;
        match v {
            Some(e) => {
                let (val, vt) = self.value(e)?;
                if by_ref {
                    self.b.set_term(Term::Ret(Some(val)));
                } else {
                    let cv = self.coerce(val, &vt, &rt)?;
                    self.b.set_term(Term::Ret(Some(cv)));
                }
            }
            None => {
                let z = self.b.vreg(Kind::I64);
                self.b.push(Inst::Const(z, 0));
                self.b.set_term(Term::Ret(Some(z)));
            }
        }
        self.b.block();
        Ok(())
    }

    // -- expressions -------------------------------------------------------

    fn value(&mut self, e: &Expr) -> Result<(VReg, Type), String> {
        match e {
            Expr::Int(v) => {
                let d = self.b.vreg(Kind::I64);
                self.b.push(Inst::Const(d, *v));
                Ok((d, Type::Int))
            }
            Expr::Float(v) => {
                let d = self.b.vreg(Kind::F64);
                self.b.push(Inst::FConst(d, *v));
                Ok((d, Type::Float))
            }
            Expr::Str(s) => {
                let off = self.it.str_off(s);
                let d = self.b.vreg(Kind::Ptr);
                self.b.push(Inst::AddrRdata(d, off));
                Ok((d, Type::String))
            }
            Expr::Char(c) => {
                let d = self.b.vreg(Kind::I8);
                self.b.push(Inst::Const(d, *c as i64));
                Ok((d, Type::Char))
            }
            Expr::Bool(b) => {
                let d = self.b.vreg(Kind::I8);
                self.b.push(Inst::Const(d, *b as i64));
                Ok((d, Type::Bool))
            }
            Expr::Null => {
                let d = self.b.vreg(Kind::Ptr);
                self.b.push(Inst::Const(d, 0));
                Ok((d, Type::Ptr(Box::new(Type::Void))))
            }
            Expr::Ident(n) => self.read_var(n),
            Expr::Binary(op, l, r) => self.binary(*op, l, r),
            Expr::Unary(op, x) => self.unary(*op, x),
            Expr::Call(n, args) => self.call(n, args),
            Expr::Index(..) | Expr::Member(..) | Expr::Arrow(..) => {
                let p = self.place(e)?;
                let v = self.load_place(&p)?;
                Ok((v, p.ty))
            }
            Expr::Assign(n, v) => self.assign_var(n, v),
            Expr::AssignLValue(lv, v) => {
                let p = self.place(lv)?;
                let addr = self.addr_of(&p)?;
                let (vv, vt) = self.value(v)?;
                let k = self.kind_of(&p.ty)?;
                if k.is_agg() {
                    let sz = self.size_of(&p.ty)?;
                    self.b.push(Inst::MemCopy {
                        dst: addr,
                        src: vv,
                        bytes: sz as u32,
                    });
                    return Ok((vv, p.ty));
                }
                let cv = self.coerce(vv, &vt, &p.ty)?;
                self.b.push(Inst::Store {
                    base: addr,
                    disp: 0,
                    src: cv,
                    kind: k,
                });
                Ok((cv, p.ty))
            }
            Expr::Cast(t, x) => {
                let (v, vt) = self.value(x)?;
                let fk = self.kind_of(&vt)?;
                let tk = self.kind_of(t)?;
                if fk == tk {
                    return Ok((v, t.clone()));
                }
                let d = self.b.vreg(tk);
                self.b.push(Inst::Cast {
                    dst: d,
                    from: fk,
                    to: tk,
                    src: v,
                });
                Ok((d, t.clone()))
            }
            Expr::Sizeof(t) => {
                let s = self.size_of(t)?;
                let d = self.b.vreg(Kind::I64);
                self.b.push(Inst::Const(d, s as i64));
                Ok((d, Type::Int))
            }
        }
    }

    /// Multiply a pointer offset by the pointee size.
    fn scale(&mut self, v: VReg, t: &Type) -> Result<VReg, String> {
        let esz = self.size_of(&pointee(t)?)?;
        if esz <= 1 {
            return Ok(v);
        }
        let c = self.b.vreg(Kind::I64);
        self.b.push(Inst::Const(c, esz as i64));
        let d = self.b.vreg(Kind::I64);
        self.b.push(Inst::Bin {
            dst: d,
            op: IBin::Mul,
            a: v,
            b: c,
        });
        Ok(d)
    }

    fn binary(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<(VReg, Type), String> {
        match op {
            BinOp::And | BinOp::Or => self.logical(op, l, r),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                self.compare(op, l, r)
            }
            _ => self.arith(op, l, r),
        }
    }

    fn logical(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<(VReg, Type), String> {
        let dst = self.b.vreg(Kind::I8);
        let (lv, _) = self.value(l)?;
        let lb = self.b.cur();
        let rhs = self.b.block();
        let tb = self.b.block();
        let fb = self.b.block();
        let end = self.b.block();

        self.b.switch(lb);
        if op == BinOp::And {
            // false short-circuits to the 0 block
            self.b.set_term(Term::Branch {
                cond: lv,
                t: rhs,
                f: fb,
            });
        } else {
            self.b.set_term(Term::Branch {
                cond: lv,
                t: tb,
                f: rhs,
            });
        }

        self.b.switch(rhs);
        let (rv, _) = self.value(r)?;
        let rb = self.b.cur();
        self.b.set_term(Term::Branch {
            cond: rv,
            t: tb,
            f: fb,
        });
        let _ = rb;

        self.b.switch(tb);
        self.b.push(Inst::Const(dst, 1));
        self.b.set_term(Term::Goto(end));

        self.b.switch(fb);
        self.b.push(Inst::Const(dst, 0));
        self.b.set_term(Term::Goto(end));

        self.b.switch(end);
        Ok((dst, Type::Bool))
    }

    fn compare(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<(VReg, Type), String> {
        let (lv, lt) = self.value(l)?;
        let (rv, rt) = self.value(r)?;
        let isf = matches!(lt, Type::Float) || matches!(rt, Type::Float);
        let cc = match op {
            BinOp::Eq => Cmp::Eq,
            BinOp::Ne => Cmp::Ne,
            BinOp::Lt => Cmp::Lt,
            BinOp::Le => Cmp::Le,
            BinOp::Gt => Cmp::Gt,
            _ => Cmp::Ge,
        };
        let d = self.b.vreg(Kind::I8);
        if isf {
            let a = if matches!(lt, Type::Float) {
                lv
            } else {
                let t = self.b.vreg(Kind::F64);
                self.b.push(Inst::Cast {
                    dst: t,
                    from: Kind::I64,
                    to: Kind::F64,
                    src: lv,
                });
                t
            };
            let b = if matches!(rt, Type::Float) {
                rv
            } else {
                let t = self.b.vreg(Kind::F64);
                self.b.push(Inst::Cast {
                    dst: t,
                    from: Kind::I64,
                    to: Kind::F64,
                    src: rv,
                });
                t
            };
            self.b.push(Inst::FCmp {
                dst: d,
                cc,
                a,
                b,
            });
        } else {
            let unsigned = matches!(lt, Type::UInt) || matches!(rt, Type::UInt) || is_ptr(&lt);
            self.b.push(Inst::ICmp {
                dst: d,
                cc,
                a: lv,
                b: rv,
                unsigned,
            });
        }
        Ok((d, Type::Bool))
    }

    fn arith(&mut self, op: BinOp, l: &Expr, r: &Expr) -> Result<(VReg, Type), String> {
        let (lv, lt) = self.value(l)?;
        let (rv, rt) = self.value(r)?;

        let isf = matches!(lt, Type::Float) || matches!(rt, Type::Float);
        if isf {
            if matches!(
                op,
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr | BinOp::Mod
            ) {
                return Err(format!(
                    "operator '{}' is not defined for floating-point values",
                    op.c()
                ));
            }
            let a = if matches!(lt, Type::Float) {
                lv
            } else {
                let t = self.b.vreg(Kind::F64);
                self.b.push(Inst::Cast {
                    dst: t,
                    from: Kind::I64,
                    to: Kind::F64,
                    src: lv,
                });
                t
            };
            let b = if matches!(rt, Type::Float) {
                rv
            } else {
                let t = self.b.vreg(Kind::F64);
                self.b.push(Inst::Cast {
                    dst: t,
                    from: Kind::I64,
                    to: Kind::F64,
                    src: rv,
                });
                t
            };
            let fop = match op {
                BinOp::Add => FBin::Add,
                BinOp::Sub => FBin::Sub,
                BinOp::Mul => FBin::Mul,
                _ => FBin::Div,
            };
            let d = self.b.vreg(Kind::F64);
            self.b.push(Inst::FBin {
                dst: d,
                op: fop,
                a,
                b,
            });
            return Ok((d, Type::Float));
        }

        let d = self.b.vreg(Kind::I64);
        match op {
            BinOp::Add => {
                if is_ptr(&lt) && !is_ptr(&rt) {
                    let s = self.scale(rv, &lt)?;
                    self.b.push(Inst::Bin {
                        dst: d,
                        op: IBin::Add,
                        a: lv,
                        b: s,
                    });
                    Ok((d, lt))
                } else if is_ptr(&rt) && !is_ptr(&lt) {
                    let s = self.scale(lv, &rt)?;
                    self.b.push(Inst::Bin {
                        dst: d,
                        op: IBin::Add,
                        a: s,
                        b: rv,
                    });
                    Ok((d, rt))
                } else {
                    self.b.push(Inst::Bin {
                        dst: d,
                        op: IBin::Add,
                        a: lv,
                        b: rv,
                    });
                    Ok((d, Type::Int))
                }
            }
            BinOp::Sub => {
                if is_ptr(&lt) && !is_ptr(&rt) {
                    let s = self.scale(rv, &lt)?;
                    self.b.push(Inst::Bin {
                        dst: d,
                        op: IBin::Sub,
                        a: lv,
                        b: s,
                    });
                } else {
                    self.b.push(Inst::Bin {
                        dst: d,
                        op: IBin::Sub,
                        a: lv,
                        b: rv,
                    });
                }
                Ok((d, if is_ptr(&lt) { lt } else { Type::Int }))
            }
            BinOp::Mul => {
                self.b.push(Inst::Bin {
                    dst: d,
                    op: IBin::Mul,
                    a: lv,
                    b: rv,
                });
                Ok((d, Type::Int))
            }
            BinOp::Div | BinOp::Mod => {
                let unsigned =
                    matches!(lt, Type::UInt) || matches!(rt, Type::UInt) || is_ptr(&lt) || is_ptr(&rt);
                self.b.push(Inst::IDiv {
                    dst: d,
                    a: lv,
                    b: rv,
                    unsigned,
                    rem: op == BinOp::Mod,
                });
                Ok((d, Type::Int))
            }
            BinOp::BitAnd => {
                self.b.push(Inst::Bin {
                    dst: d,
                    op: IBin::And,
                    a: lv,
                    b: rv,
                });
                Ok((d, Type::Int))
            }
            BinOp::BitOr => {
                self.b.push(Inst::Bin {
                    dst: d,
                    op: IBin::Or,
                    a: lv,
                    b: rv,
                });
                Ok((d, Type::Int))
            }
            BinOp::BitXor => {
                self.b.push(Inst::Bin {
                    dst: d,
                    op: IBin::Xor,
                    a: lv,
                    b: rv,
                });
                Ok((d, Type::Int))
            }
            BinOp::Shl => {
                self.b.push(Inst::Bin {
                    dst: d,
                    op: IBin::Shl,
                    a: lv,
                    b: rv,
                });
                Ok((d, Type::Int))
            }
            BinOp::Shr => {
                let op = if matches!(lt, Type::UInt) {
                    IBin::Shr
                } else {
                    IBin::Sar
                };
                self.b.push(Inst::Bin {
                    dst: d,
                    op,
                    a: lv,
                    b: rv,
                });
                Ok((d, Type::Int))
            }
            _ => Err(format!("operator '{}' is not supported", op.c())),
        }
    }

    fn unary(&mut self, op: UnOp, x: &Expr) -> Result<(VReg, Type), String> {
        match op {
            UnOp::Neg => {
                let (v, t) = self.value(x)?;
                let d = self.b.vreg(self.kind_of(&t)?);
                if matches!(t, Type::Float) {
                    self.b.push(Inst::FUn {
                        dst: d,
                        op: FUn::Neg,
                        a: v,
                    });
                } else {
                    self.b.push(Inst::Un {
                        dst: d,
                        op: IUn::Neg,
                        a: v,
                    });
                }
                Ok((d, t))
            }
            UnOp::Not => {
                let (v, t) = self.value(x)?;
                let d = self.b.vreg(Kind::I8);
                if matches!(t, Type::Float) {
                    self.b.push(Inst::FUn {
                        dst: d,
                        op: FUn::Not,
                        a: v,
                    });
                } else {
                    self.b.push(Inst::Un {
                        dst: d,
                        op: IUn::Not,
                        a: v,
                    });
                }
                Ok((d, Type::Bool))
            }
            UnOp::BitNot => {
                let (v, _) = self.value(x)?;
                let d = self.b.vreg(Kind::I64);
                self.b.push(Inst::Un {
                    dst: d,
                    op: IUn::BitNot,
                    a: v,
                });
                Ok((d, Type::Int))
            }
            UnOp::Deref => {
                let p = self.place(x)?;
                let v = self.load_place(&p)?;
                Ok((v, p.ty))
            }
            UnOp::AddrOf => {
                let p = self.place(x)?;
                let a = self.addr_of(&p)?;
                let t = p.ty.clone();
                Ok((a, Type::Ptr(Box::new(t))))
            }
        }
    }

    // -- calls -------------------------------------------------------------

    fn rt(&mut self, name: &str, gpr: Vec<VReg>, xmm: Option<VReg>, dst: Option<Kind>) -> Option<VReg> {
        let d = dst.map(|k| self.b.vreg(k));
        self.b.push(Inst::RtCall {
            dst: d,
            name: name.to_string(),
            gpr,
            xmm,
        });
        d
    }

    fn print_newline(&mut self) -> Result<(), String> {
        let s = self.it.str_off("\n");
        let p = self.b.vreg(Kind::Ptr);
        self.b.push(Inst::AddrRdata(p, s));
        let one = self.b.vreg(Kind::I64);
        self.b.push(Inst::Const(one, 1));
        self.rt("bs_write", vec![p, one], None, None);
        Ok(())
    }

    fn print_value(&mut self, t: &Type, v: VReg) -> Result<(), String> {
        match t {
            Type::String => {
                self.rt("bs_print_str", vec![v], None, None);
            }
            Type::Int => {
                self.rt("bs_print_int", vec![v], None, None);
            }
            Type::UInt | Type::Byte => {
                self.rt("bs_print_uint", vec![v], None, None);
            }
            Type::Char => {
                self.b.push(Inst::Store {
                    base: DATA,
                    disp: D_CHARBUF as i32,
                    src: v,
                    kind: Kind::I8,
                });
                let p = self.b.vreg(Kind::Ptr);
                self.b.push(Inst::AddrGlobal(p, D_CHARBUF));
                let one = self.b.vreg(Kind::I64);
                self.b.push(Inst::Const(one, 1));
                self.rt("bs_write", vec![p, one], None, None);
            }
            Type::Bool => {
                let tb = self.it.str_off("true");
                let fb = self.it.str_off("false");
                let cur = self.b.cur();
                let t = self.b.block();
                let f = self.b.block();
                let j = self.b.block();
                self.b.switch(cur);
                self.b.set_term(Term::Branch { cond: v, t, f });
                let r = self.b.vreg(Kind::Ptr);
                self.b.switch(t);
                self.b.push(Inst::AddrRdata(r, tb));
                self.b.set_term(Term::Goto(j));
                self.b.switch(f);
                self.b.push(Inst::AddrRdata(r, fb));
                self.b.set_term(Term::Goto(j));
                self.b.switch(j);
                self.rt("bs_print_str", vec![r], None, None);
            }
            Type::Ptr(_) | Type::Named(_) | Type::Array(_, _) => {
                self.rt("bs_print_hex", vec![v], None, None);
            }
            Type::Float => {
                self.rt("bs_print_float", Vec::new(), Some(v), None);
            }
            Type::Void => return Err("cannot print a void expression".into()),
        }
        Ok(())
    }

    fn expect_nargs(&self, name: &str, args: &[Expr], n: usize) -> Result<(), String> {
        if args.len() != n {
            return Err(format!(
                "{}() expects {} argument(s), got {}",
                name,
                n,
                args.len()
            ));
        }
        Ok(())
    }

    /// Lower `args` and load them into the first `n` GP argument registers of a
    /// runtime helper.
    fn rt_args(&mut self, args: &[Expr]) -> Result<Vec<VReg>, String> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            let (v, _) = self.value(a)?;
            out.push(v);
        }
        Ok(out)
    }

    fn call(&mut self, name: &str, args: &[Expr]) -> Result<(VReg, Type), String> {
        match name {
            "print" | "println" => {
                for a in args {
                    let (v, t) = self.value(a)?;
                    self.print_value(&t, v)?;
                }
                self.print_newline()?;
                let u = self.unit();
                Ok((u, Type::Void))
            }
            "len" | "strlen" => {
                self.expect_nargs(name, args, 1)?;
                let g = self.rt_args(args)?;
                let d = self
                    .rt("bs_strlen", g, None, Some(Kind::I64))
                    .unwrap();
                Ok((d, Type::Int))
            }
            "malloc" => {
                self.expect_nargs(name, args, 1)?;
                let g = self.rt_args(args)?;
                let d = self.rt("bs_malloc", g, None, Some(Kind::Ptr)).unwrap();
                Ok((d, Type::Ptr(Box::new(Type::Void))))
            }
            "free" => {
                self.expect_nargs(name, args, 1)?;
                let g = self.rt_args(args)?;
                self.rt("bs_free", g, None, None);
                let u = self.unit();
                Ok((u, Type::Void))
            }
            "to_int" => {
                self.expect_nargs(name, args, 1)?;
                let (v, t) = self.value(&args[0])?;
                if matches!(t, Type::Float) {
                    let d = self.b.vreg(Kind::I64);
                    self.b.push(Inst::Cast {
                        dst: d,
                        from: Kind::F64,
                        to: Kind::I64,
                        src: v,
                    });
                    Ok((d, Type::Int))
                } else {
                    Ok((v, Type::Int))
                }
            }
            "to_float" => {
                self.expect_nargs(name, args, 1)?;
                let (v, t) = self.value(&args[0])?;
                if matches!(t, Type::Float) {
                    Ok((v, Type::Float))
                } else {
                    let d = self.b.vreg(Kind::F64);
                    self.b.push(Inst::Cast {
                        dst: d,
                        from: Kind::I64,
                        to: Kind::F64,
                        src: v,
                    });
                    Ok((d, Type::Float))
                }
            }
            "read_str" => {
                if !args.is_empty() {
                    return Err("read_str() takes no arguments".into());
                }
                let d = self
                    .rt("bs_read_alloc", Vec::new(), None, Some(Kind::Ptr))
                    .unwrap();
                Ok((d, Type::String))
            }
            "read_int" => {
                if !args.is_empty() {
                    return Err("read_int() takes no arguments".into());
                }
                let d = self
                    .rt("bs_read_int", Vec::new(), None, Some(Kind::I64))
                    .unwrap();
                Ok((d, Type::Int))
            }
            "read_char" => {
                if !args.is_empty() {
                    return Err("read_char() takes no arguments".into());
                }
                let buf = self.b.vreg(Kind::Ptr);
                self.b.push(Inst::AddrGlobal(buf, D_INBUF));
                let two = self.b.vreg(Kind::I64);
                self.b.push(Inst::Const(two, 2));
                self.rt("bs_read", vec![buf, two], None, None);
                let d = self.b.vreg(Kind::I8);
                self.b.push(Inst::Load {
                    dst: d,
                    base: DATA,
                    disp: D_INBUF as i32,
                    kind: Kind::I8,
                });
                Ok((d, Type::Char))
            }
            "va_int" | "va_float" => {
                if !self.variadic {
                    return Err(format!(
                        "{}() is only available inside a variadic function",
                        name
                    ));
                }
                self.expect_nargs(name, args, 1)?;
                let (iv, it) = self.value(&args[0])?;
                let idx = self.coerce(iv, &it, &Type::Int)?;
                let addr = self.b.vreg(Kind::Ptr);
                self.b.push(Inst::Lea {
                    dst: addr,
                    base: FRAME,
                    index: idx,
                    scale: 3,
                    disp: 16,
                });
                if name == "va_int" {
                    let d = self.b.vreg(Kind::I64);
                    self.b.push(Inst::Load {
                        dst: d,
                        base: addr,
                        disp: 0,
                        kind: Kind::I64,
                    });
                    Ok((d, Type::Int))
                } else {
                    let d = self.b.vreg(Kind::F64);
                    self.b.push(Inst::Load {
                        dst: d,
                        base: addr,
                        disp: 0,
                        kind: Kind::F64,
                    });
                    Ok((d, Type::Float))
                }
            }
            "open" => {
                self.expect_nargs(name, args, 2)?;
                let g = self.rt_args(args)?;
                let d = self.rt("bs_open", g, None, Some(Kind::Ptr)).unwrap();
                Ok((d, Type::Ptr(Box::new(Type::Void))))
            }
            "read" => {
                self.expect_nargs(name, args, 3)?;
                let g = self.rt_args(args)?;
                let d = self.rt("bs_file_read", g, None, Some(Kind::I64)).unwrap();
                Ok((d, Type::Int))
            }
            "write" => {
                self.expect_nargs(name, args, 3)?;
                let g = self.rt_args(args)?;
                let d = self
                    .rt("bs_file_write", g, None, Some(Kind::I64))
                    .unwrap();
                Ok((d, Type::Int))
            }
            "close" => {
                self.expect_nargs(name, args, 1)?;
                let g = self.rt_args(args)?;
                self.rt("bs_close", g, None, None);
                let u = self.unit();
                Ok((u, Type::Void))
            }
            "file_size" => {
                self.expect_nargs(name, args, 1)?;
                let g = self.rt_args(args)?;
                let d = self
                    .rt("bs_file_size", g, None, Some(Kind::I64))
                    .unwrap();
                Ok((d, Type::Int))
            }
            _ => self.call_user(name, args),
        }
    }

    fn call_user(&mut self, name: &str, args: &[Expr]) -> Result<(VReg, Type), String> {
        let f = self
            .ctx
            .fns
            .get(name)
            .cloned()
            .ok_or_else(|| format!("undefined function '{}'", name))?;
        if f.variadic {
            if args.len() < f.params.len() {
                return Err(format!(
                    "function '{}' expects at least {} argument(s), got {}",
                    name,
                    f.params.len(),
                    args.len()
                ));
            }
        } else if f.params.len() != args.len() {
            return Err(format!(
                "function '{}' expects {} argument(s), got {}",
                name,
                f.params.len(),
                args.len()
            ));
        }

        let ret_ty = f.ret.clone();
        let ret_sz = self.size_of(&ret_ty)?;
        let ret_by_ref = is_agg(&ret_ty) && ret_sz > 8;
        let ret_kind = self.kind_of(&ret_ty)?;
        let ret_slot = if ret_by_ref {
            Some(self.b.alloc(ret_sz))
        } else {
            None
        };

        // argument classes come from the declared parameter types
        let mut kinds = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let k = match f.params.get(i) {
                Some((_, pt)) => self.kind_of(pt)?,
                None => {
                    let t = self.ty(a)?;
                    self.kind_of(&t)?
                }
            };
            kinds.push(k);
        }

        let mut vals = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let k = kinds[i];
            if k.is_agg() {
                // aggregates are passed as the address of the value
                if let Ok(p) = self.place(a) {
                    let v = self.addr_of(&p)?;
                    vals.push(v);
                } else {
                    let (v, _) = self.value(a)?;
                    vals.push(v);
                }
            } else {
                let pt = match f.params.get(i) {
                    Some((_, pt)) => pt.clone(),
                    None => self.ty(a)?,
                };
                let (v, vt) = self.value(a)?;
                vals.push(self.coerce(v, &vt, &pt)?);
            }
        }

        let dst_kind = if ret_kind.is_agg() {
            Kind::Ptr
        } else {
            ret_kind
        };
        let dst = Some(self.b.vreg(dst_kind));
        self.b.push(Inst::Call {
            dst,
            name: name.to_string(),
            args: vals,
            kinds,
            variadic: f.variadic,
            ret: ret_kind,
            ret_slot,
        });

        if ret_by_ref {
            let d = self.b.vreg(Kind::Ptr);
            self.b.push(Inst::AddrLocal(d, ret_slot.unwrap()));
            return Ok((d, ret_ty));
        }
        Ok((dst.unwrap(), ret_ty))
    }
}
