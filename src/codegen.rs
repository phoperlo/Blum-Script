//! C code generator for BlumScript.

use std::collections::HashMap;

use crate::ast::*;
use crate::types::Type;

const RUNTIME: &str = r#"/* ---- BlumScript runtime ---- */
#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>

static void bs_print_int(int64_t v)   { printf("%lld", (long long)v); }
static void bs_print_uint(uint64_t v) { printf("%llu", (unsigned long long)v); }
static void bs_print_float(double v)  { printf("%g", v); }
static void bs_print_bool(bool v)     { printf("%s", v ? "true" : "false"); }
static void bs_print_char(char c)     { putchar((int)c); }
static void bs_print_str(const char* s){ printf("%s", s ? s : "null"); }
static void bs_print_ptr(const void* p){ if (p) printf("%p", p); else printf("null"); }
/* ---- end runtime ---- */
"#;

#[derive(Clone)]
struct FuncSig {
    params: Vec<Type>,
    ret: Type,
}

pub struct Codegen {
    structs: HashMap<String, Vec<(String, Type)>>,
    funcs: HashMap<String, FuncSig>,
    globals: HashMap<String, Type>,
    scopes: Vec<HashMap<String, Type>>,
    out: String,
    indent: usize,
    current_ret: Type,
    _current_fn: String,
    has_user_main: bool,
}

impl Codegen {
    pub fn new() -> Self {
        Self {
            structs: HashMap::new(),
            funcs: HashMap::new(),
            globals: HashMap::new(),
            scopes: Vec::new(),
            out: String::new(),
            indent: 0,
            current_ret: Type::Void,
            _current_fn: String::new(),
            has_user_main: false,
        }
    }

    // ---------- helpers ----------

    fn line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn c_name(name: &str) -> String {
        if name == "main" {
            "bs_main".to_string()
        } else {
            name.to_string()
        }
    }

    fn get_var(&self, name: &str) -> Option<Type> {
        for sc in self.scopes.iter().rev() {
            if let Some(t) = sc.get(name) {
                return Some(t.clone());
            }
        }
        self.globals.get(name).cloned()
    }

    fn define(&mut self, name: &str, t: Type) {
        if let Some(sc) = self.scopes.last_mut() {
            sc.insert(name.to_string(), t);
        } else {
            self.globals.insert(name.to_string(), t);
        }
    }

    fn numeric_result(a: &Type, b: &Type) -> Type {
        if *a == Type::Float || *b == Type::Float {
            Type::Float
        } else if *a == Type::UInt || *b == Type::UInt || *a == Type::Byte || *b == Type::Byte {
            Type::UInt
        } else {
            Type::Int
        }
    }

    fn cast_code(expr_code: &str, from: &Type, to: &Type) -> String {
        if from == to {
            return expr_code.to_string();
        }
        format!("(({})({}))", to.c(), expr_code)
    }

    fn decl_str(t: &Type, name: &str) -> String {
        match t {
            Type::Array(inner, n) => format!("{} {}[{}]", inner.c(), name, n),
            Type::Ptr(inner) => format!("{}* {}", inner.c(), name),
            _ => format!("{} {}", t.c(), name),
        }
    }

    // ---------- top-level ----------

    pub fn generate(&mut self, program: &[Stmt]) -> Result<String, String> {
        // 1. collect structs
        for stmt in program {
            if let Stmt::Struct(s) = stmt {
                self.structs.insert(s.name.clone(), s.fields.clone());
            }
        }
        // 2. collect function signatures
        for stmt in program {
            if let Stmt::Func(f) = stmt {
                if f.name == "main" {
                    self.has_user_main = true;
                    if !f.params.is_empty() {
                        return Err("main() must not take parameters".into());
                    }
                }
                self.funcs.insert(
                    f.name.clone(),
                    FuncSig {
                        params: f.params.iter().map(|(_, t)| t.clone()).collect(),
                        ret: f.ret.clone(),
                    },
                );
            }
        }
        // 3. collect globals (top-level let/var)
        for stmt in program {
            match stmt {
                Stmt::Let(name, ty, init) | Stmt::Var(name, ty, init) => {
                    let t = match ty {
                        Some(t) => t.clone(),
                        None => {
                            let (_, inferred) = self.infer(init)?;
                            inferred
                        }
                    };
                    self.globals.insert(name.clone(), t);
                }
                _ => {}
            }
        }

        // header + runtime
        self.out.push_str(RUNTIME);
        self.out.push('\n');

        // struct typedefs
        if !self.structs.is_empty() {
            self.out.push_str("/* ---- structs ---- */\n");
            for name in self.structs.keys() {
                self.out.push_str(&format!("typedef struct {} {};\n", name, name));
            }
            let entries: Vec<(String, Vec<(String, Type)>)> = self
                .structs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (name, fields) in entries {
                self.out.push_str(&format!("struct {} {{\n", name));
                for (fname, fty) in fields {
                    self.out.push_str(&format!("    {};\n", Self::decl_str(&fty, &fname)));
                }
                self.out.push_str("};\n");
            }
            self.out.push('\n');
        }

        // globals
        if !self.globals.is_empty() {
            self.out.push_str("/* ---- globals ---- */\n");
            let entries: Vec<(String, Type)> =
                self.globals.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            for (name, t) in entries {
                self.out.push_str(&format!("static {};\n", Self::decl_str(&t, &name)));
            }
            self.out.push('\n');
        }

        // prototypes
        self.out.push_str("/* ---- prototypes ---- */\n");
        for stmt in program {
            if let Stmt::Func(f) = stmt {
                let ret = if f.name == "main" {
                    "int".to_string()
                } else {
                    f.ret.c()
                };
                let params = if f.name == "main" {
                    "void".to_string()
                } else {
                    f.params
                        .iter()
                        .map(|(n, t)| format!("{} {}", t.c(), n))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let params = if params.is_empty() {
                    "void".to_string()
                } else {
                    params
                };
                self.out
                    .push_str(&format!("{} {}({});\n", ret, Self::c_name(&f.name), params));
            }
        }
        self.out.push('\n');

        // function definitions
        for stmt in program {
            if let Stmt::Func(f) = stmt {
                self.gen_function(f)?;
            }
        }

        // main
        self.gen_main(program)?;

        Ok(std::mem::take(&mut self.out))
    }

    fn gen_function(&mut self, f: &FuncDecl) -> Result<(), String> {
        let cname = Self::c_name(&f.name);
        let ret_c = if f.name == "main" {
            "int".to_string()
        } else {
            f.ret.c()
        };
        let params = if f.name == "main" {
            "void".to_string()
        } else {
            let s = f
                .params
                .iter()
                .map(|(n, t)| format!("{} {}", t.c(), n))
                .collect::<Vec<_>>()
                .join(", ");
            if s.is_empty() {
                "void".to_string()
            } else {
                s
            }
        };

        self.out
            .push_str(&format!("{} {}({}) {{\n", ret_c, cname, params));
        self.indent += 1;
        self.scopes.push(HashMap::new());
        for (n, t) in &f.params {
            self.define(n, t.clone());
        }
        self.current_ret = if f.name == "main" {
            Type::Int
        } else {
            f.ret.clone()
        };
        self._current_fn = f.name.clone();

        for s in &f.body {
            self.gen_stmt(s)?;
        }

        if f.name == "main" {
            // ensure return
            self.line("return 0;");
        }

        self.scopes.pop();
        self.indent -= 1;
        self.out.push_str("}\n\n");
        Ok(())
    }

    fn gen_main(&mut self, program: &[Stmt]) -> Result<(), String> {
        self.out.push_str("/* ---- program entry ---- */\n");
        self.out.push_str("int main(void) {\n");
        self.indent += 1;
        self.scopes.push(HashMap::new());

        for stmt in program {
            match stmt {
                Stmt::Func(_) | Stmt::Struct(_) => {}
                Stmt::Let(name, _, init) | Stmt::Var(name, _, init) => {
                    let (code, _) = self.infer(init)?;
                    self.line(&format!("{} = {};", name, code));
                }
                other => {
                    self.gen_stmt(other)?;
                }
            }
        }

        if self.has_user_main {
            self.line("(void)bs_main();");
        }
        self.line("return 0;");

        self.scopes.pop();
        self.indent -= 1;
        self.out.push_str("}\n");
        Ok(())
    }

    // ---------- statements ----------

    fn gen_stmts(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        for s in stmts {
            self.gen_stmt(s)?;
        }
        Ok(())
    }

    fn gen_block(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        self.scopes.push(HashMap::new());
        let r = self.gen_stmts(stmts);
        self.scopes.pop();
        r
    }

    fn gen_stmt(&mut self, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Let(name, ty, init) | Stmt::Var(name, ty, init) => {
                let (code, etype) = self.infer(init)?;
                let t = match ty {
                    Some(t) => t.clone(),
                    None => etype.clone(),
                };
                let code = if ty.is_some() && t != etype {
                    Self::cast_code(&code, &etype, &t)
                } else {
                    code
                };
                let decl = Self::decl_str(&t, name);
                if matches!(t, Type::Array(_, _)) {
                    self.line(&format!("{} = {};", decl, code));
                } else {
                    self.line(&format!("{} = {};", decl, code));
                }
                self.define(name, t);
                Ok(())
            }
            Stmt::Assign(name, e) => {
                let var_ty = self
                    .get_var(name)
                    .ok_or_else(|| format!("assignment to undefined variable '{}'", name))?;
                let (code, etype) = self.infer(e)?;
                let code = Self::cast_code(&code, &etype, &var_ty);
                self.line(&format!("{} = {};", name, code));
                Ok(())
            }
            Stmt::Func(_) | Stmt::Struct(_) => Ok(()),
            Stmt::If(cond, then_b, else_b) => {
                let (c, _) = self.infer(cond)?;
                self.line(&format!("if ({}) {{", c));
                self.indent += 1;
                self.gen_block(then_b)?;
                self.indent -= 1;
                if !else_b.is_empty() {
                    self.line("} else {");
                    self.indent += 1;
                    self.gen_block(else_b)?;
                    self.indent -= 1;
                    self.line("}");
                } else {
                    self.line("}");
                }
                Ok(())
            }
            Stmt::While(cond, body) => {
                let (c, _) = self.infer(cond)?;
                self.line(&format!("while ({}) {{", c));
                self.indent += 1;
                self.gen_block(body)?;
                self.indent -= 1;
                self.line("}");
                Ok(())
            }
            Stmt::For(init, cond, update, body) => {
                self.scopes.push(HashMap::new());
                let init_s = self.gen_simple(init)?;
                let (cond_c, _) = self.infer(cond)?;
                let upd_s = self.gen_simple(update)?;
                self.line(&format!("for ({}; {}; {}) {{", init_s, cond_c, upd_s));
                self.indent += 1;
                self.gen_stmts(body)?;
                self.indent -= 1;
                self.line("}");
                self.scopes.pop();
                Ok(())
            }
            Stmt::Return(e) => {
                match e {
                    Some(expr) => {
                        let (code, etype) = self.infer(expr)?;
                        let code =
                            Self::cast_code(&code, &etype, &self.current_ret.clone());
                        self.line(&format!("return {};", code));
                    }
                    None => {
                        self.line("return;");
                    }
                }
                Ok(())
            }
            Stmt::Break => {
                self.line("break;");
                Ok(())
            }
            Stmt::Continue => {
                self.line("continue;");
                Ok(())
            }
            Stmt::Expr(e) => {
                let (code, _) = self.infer(e)?;
                self.line(&format!("{};", code));
                Ok(())
            }
            Stmt::Block(b) => self.gen_block(b),
            Stmt::Asm(raw) => {
                for ln in raw.lines() {
                    self.line(ln);
                }
                Ok(())
            }
        }
    }

    /// Generate a for-loop init/update clause (no trailing semicolon).
    fn gen_simple(&mut self, s: &Stmt) -> Result<String, String> {
        match s {
            Stmt::Let(name, ty, init) | Stmt::Var(name, ty, init) => {
                let (code, etype) = self.infer(init)?;
                let t = match ty {
                    Some(t) => t.clone(),
                    None => etype.clone(),
                };
                let code = if ty.is_some() && t != etype {
                    Self::cast_code(&code, &etype, &t)
                } else {
                    code
                };
                self.define(name, t.clone());
                Ok(format!("{} = {}", Self::decl_str(&t, name), code))
            }
            Stmt::Assign(name, e) => {
                let var_ty = self
                    .get_var(name)
                    .ok_or_else(|| format!("assignment to undefined variable '{}'", name))?;
                let (code, etype) = self.infer(e)?;
                let code = Self::cast_code(&code, &etype, &var_ty);
                Ok(format!("{} = {}", name, code))
            }
            Stmt::Expr(e) => {
                let (code, _) = self.infer(e)?;
                Ok(code)
            }
            other => Err(format!("invalid for-loop clause: {:?}", other)),
        }
    }

    // ---------- expressions ----------

    fn infer(&mut self, e: &Expr) -> Result<(String, Type), String> {
        match e {
            Expr::Int(v) => Ok((format!("((int64_t){})", v), Type::Int)),
            Expr::Float(v) => {
                let s = if v.fract() == 0.0 {
                    format!("{:.1}", v)
                } else {
                    format!("{}", v)
                };
                Ok((format!("((double){})", s), Type::Float))
            }
            Expr::Str(s) => Ok((c_string(s), Type::String)),
            Expr::Char(c) => Ok((format!("((char){})", *c as u32), Type::Char)),
            Expr::Bool(b) => Ok((
                if *b { "true".to_string() } else { "false".to_string() },
                Type::Bool,
            )),
            Expr::Null => Ok(("((void*)0)".to_string(), Type::Ptr(Box::new(Type::Void)))),

            Expr::Ident(name) => {
                let t = self
                    .get_var(name)
                    .ok_or_else(|| format!("undefined variable '{}'", name))?;
                Ok((name.clone(), t))
            }

            Expr::Assign(name, rhs) => {
                let var_ty = self
                    .get_var(name)
                    .ok_or_else(|| format!("assignment to undefined variable '{}'", name))?;
                let (code, etype) = self.infer(rhs)?;
                let code = Self::cast_code(&code, &etype, &var_ty);
                Ok((format!("({} = {})", name, code), var_ty))
            }

            Expr::AssignLValue(lhs, rhs) => {
                let (lc, lt) = self.infer(lhs)?;
                let (rc, rt) = self.infer(rhs)?;
                let rc = Self::cast_code(&rc, &rt, &lt);
                Ok((format!("({} = {})", lc, rc), lt))
            }

            Expr::Unary(op, inner) => match op {
                UnOp::Neg => {
                    let (code, t) = self.infer(inner)?;
                    if !t.is_numeric() {
                        return Err("unary '-' requires a numeric operand".into());
                    }
                    Ok((format!("(-({}))", code), t))
                }
                UnOp::Not => {
                    let (code, _) = self.infer(inner)?;
                    Ok((format!("(!({}))", code), Type::Bool))
                }
                UnOp::BitNot => {
                    let (code, t) = self.infer(inner)?;
                    if !t.is_intlike() {
                        return Err("bitwise '~' requires an integer operand".into());
                    }
                    Ok((format!("(~({}))", code), Type::Int))
                }
                UnOp::Deref => {
                    let (code, t) = self.infer(inner)?;
                    match t.pointee() {
                        Some(p) => Ok((format!("(*({}))", code), p)),
                        None => Err("cannot dereference a non-pointer value".into()),
                    }
                }
                UnOp::AddrOf => {
                    let (code, t) = self.infer(inner)?;
                    match inner.as_ref() {
                        Expr::Ident(_) | Expr::Member(_, _) | Expr::Arrow(_, _) | Expr::Index(_, _) => {
                            Ok((format!("(&({}))", code), Type::Ptr(Box::new(t))))
                        }
                        _ => Err("cannot take the address of a non-lvalue".into()),
                    }
                }
            },

            Expr::Binary(op, l, r) => {
                let (lc, lt) = self.infer(l)?;
                let (rc, rt) = self.infer(r)?;
                let code = format!("(({}) {} ({}))", lc, op.c(), rc);
                let t = match op {
                    BinOp::Add | BinOp::Sub => {
                        if let Some(p) = lt.pointee() {
                            if rt.is_intlike() {
                                Type::Ptr(Box::new(p))
                            } else {
                                return Err("pointer arithmetic requires an integer offset".into());
                            }
                        } else if let Some(p) = rt.pointee() {
                            if lt.is_intlike() {
                                Type::Ptr(Box::new(p))
                            } else {
                                return Err("pointer arithmetic requires an integer offset".into());
                            }
                        } else if lt.is_numeric() && rt.is_numeric() {
                            Self::numeric_result(&lt, &rt)
                        } else if lt == Type::String && rt == Type::String && matches!(op, BinOp::Add) {
                            // string concatenation not supported at runtime; treat as pointer
                            Type::String
                        } else {
                            return Err(format!(
                                "invalid operands for '{}': {} and {}",
                                op.c(),
                                lt.display(),
                                rt.display()
                            ));
                        }
                    }
                    BinOp::Mul | BinOp::Div | BinOp::Mod => {
                        if !lt.is_numeric() || !rt.is_numeric() {
                            return Err(format!(
                                "invalid operands for '{}': {} and {}",
                                op.c(),
                                lt.display(),
                                rt.display()
                            ));
                        }
                        Self::numeric_result(&lt, &rt)
                    }
                    BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                        if !(lt.is_numeric() && rt.is_numeric())
                            && !(lt.is_pointer() && rt.is_pointer())
                        {
                            return Err(format!(
                                "invalid operands for '{}': {} and {}",
                                op.c(),
                                lt.display(),
                                rt.display()
                            ));
                        }
                        Type::Bool
                    }
                    BinOp::Eq | BinOp::Ne => Type::Bool,
                    BinOp::And | BinOp::Or => Type::Bool,
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                        if !lt.is_intlike() || !rt.is_intlike() {
                            return Err(format!(
                                "invalid operands for '{}': {} and {}",
                                op.c(),
                                lt.display(),
                                rt.display()
                            ));
                        }
                        Type::Int
                    }
                };
                Ok((code, t))
            }

            Expr::Cast(ty, inner) => {
                let (code, _) = self.infer(inner)?;
                Ok((format!("(({})({}))", ty.c(), code), ty.clone()))
            }

            Expr::Sizeof(ty) => Ok((format!("((int64_t)sizeof({}))", ty.c()), Type::Int)),

            Expr::Call(name, args) => self.infer_call(name, args),

            Expr::Index(base, idx) => {
                let (bc, bt) = self.infer(base)?;
                let (ic, it) = self.infer(idx)?;
                if !it.is_intlike() {
                    return Err("array index must be an integer".into());
                }
                let elem = match bt {
                    Type::Ptr(inner) => *inner,
                    Type::Array(inner, _) => *inner,
                    Type::String => Type::Char,
                    _ => return Err(format!("cannot index a value of type {}", bt.display())),
                };
                Ok((format!("({}[{}])", bc, ic), elem))
            }

            Expr::Member(base, field) | Expr::Arrow(base, field) => {
                let (bc, bt) = self.infer(base)?;
                let (struct_name, via_ptr) = match &bt {
                    Type::Named(n) => (n.clone(), false),
                    Type::Ptr(inner) => match inner.as_ref() {
                        Type::Named(n) => (n.clone(), true),
                        _ => return Err("member access on a non-struct pointer".into()),
                    },
                    _ => return Err("member access on a non-struct value".into()),
                };
                let fields = self
                    .structs
                    .get(&struct_name)
                    .ok_or_else(|| format!("unknown struct '{}'", struct_name))?;
                let fty = fields
                    .iter()
                    .find(|(f, _)| f == field)
                    .map(|(_, t)| t.clone())
                    .ok_or_else(|| {
                        format!("struct '{}' has no field '{}'", struct_name, field)
                    })?;
                if via_ptr {
                    Ok((format!("({}->{})", bc, field), fty))
                } else {
                    Ok((format!("({}.{})", bc, field), fty))
                }
            }
        }
    }

    fn infer_call(&mut self, name: &str, args: &[Expr]) -> Result<(String, Type), String> {
        match name {
            "print" | "println" => {
                let mut parts: Vec<String> = Vec::new();
                for a in args {
                    let (code, t) = self.infer(a)?;
                    parts.push(format!("bs_print_{}({})", t.print_suffix(), code));
                }
                if parts.is_empty() {
                    parts.push("printf(\"\\n\")".to_string());
                } else {
                    parts.push("printf(\"\\n\")".to_string());
                }
                Ok((format!("({})", parts.join(", ")), Type::Void))
            }
            "len" => {
                if args.len() != 1 {
                    return Err("len() expects 1 argument".into());
                }
                let (code, t) = self.infer(&args[0])?;
                match t {
                    Type::String | Type::Ptr(_) | Type::Array(_, _) => {
                        Ok((format!("((int64_t)strlen((const char*)({})))", code), Type::Int))
                    }
                    _ => Err("len() expects a string or pointer".into()),
                }
            }
            "malloc" => {
                if args.len() != 1 {
                    return Err("malloc() expects 1 argument".into());
                }
                let (code, t) = self.infer(&args[0])?;
                if !t.is_intlike() {
                    return Err("malloc() size must be an integer".into());
                }
                Ok((
                    format!("((void*)malloc((size_t)({})))", code),
                    Type::Ptr(Box::new(Type::Void)),
                ))
            }
            "free" => {
                if args.len() != 1 {
                    return Err("free() expects 1 argument".into());
                }
                let (code, _) = self.infer(&args[0])?;
                Ok((format!("free((void*)({}))", code), Type::Void))
            }
            "to_int" => {
                if args.len() != 1 {
                    return Err("to_int() expects 1 argument".into());
                }
                let (code, _) = self.infer(&args[0])?;
                Ok((format!("((int64_t)({}))", code), Type::Int))
            }
            "to_float" => {
                if args.len() != 1 {
                    return Err("to_float() expects 1 argument".into());
                }
                let (code, _) = self.infer(&args[0])?;
                Ok((format!("((double)({}))", code), Type::Float))
            }
            "strlen" => {
                if args.len() != 1 {
                    return Err("strlen() expects 1 argument".into());
                }
                let (code, _) = self.infer(&args[0])?;
                Ok((format!("((int64_t)strlen((const char*)({})))", code), Type::Int))
            }
            _ => {
                let sig = self
                    .funcs
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("undefined function '{}'", name))?;
                if sig.params.len() != args.len() {
                    return Err(format!(
                        "function '{}' expects {} argument(s), got {}",
                        name,
                        sig.params.len(),
                        args.len()
                    ));
                }
                let mut parts = Vec::new();
                for (a, pt) in args.iter().zip(sig.params.iter()) {
                    let (code, at) = self.infer(a)?;
                    parts.push(Self::cast_code(&code, &at, pt));
                }
                let call = format!("{}({})", Self::c_name(name), parts.join(", "));
                if name == "main" {
                    Ok((call, Type::Int))
                } else {
                    Ok((call, sig.ret.clone()))
                }
            }
        }
    }
}

/// Convert a Rust string into a C string literal (escaped).
fn c_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}
