#![allow(dead_code)]
//! AST definitions for BlumScript.

use crate::types::Type;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    pub fn c(&self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    Deref,
    AddrOf,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
    Bool(bool),
    Null,
    Ident(String),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Call(String, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Member(Box<Expr>, String),      // dot: value.field
    Arrow(Box<Expr>, String),       // ptr->field
    Assign(String, Box<Expr>),      // name = expr
    AssignLValue(Box<Expr>, Box<Expr>), // lvalue = expr (member/index/deref)
    Cast(Type, Box<Expr>),
    Sizeof(Type),
}

#[derive(Clone, Debug)]
pub struct FuncDecl {
    pub name: String,
    pub params: Vec<(String, Type)>,
    pub ret: Type,
    /// `func f(a: int, ...)` — extra arguments are passed by the C varargs
    /// convention (positional slots, floats duplicated into the GP registers).
    pub variadic: bool,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let(String, Option<Type>, Expr),
    Var(String, Option<Type>, Expr),
    Assign(String, Expr),
    Func(FuncDecl),
    Struct(StructDecl),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    For(Box<Stmt>, Expr, Box<Stmt>, Vec<Stmt>),
    Return(Option<Expr>),
    Break,
    Continue,
    Expr(Expr),
    Block(Vec<Stmt>),
    Asm(String),
}
