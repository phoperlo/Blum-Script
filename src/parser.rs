//! Recursive-descent parser for BlumScript.

use crate::ast::*;
use crate::lexer::Tok;
use crate::types::Type;

pub struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    pub fn new(toks: Vec<Tok>) -> Self {
        Self { toks, pos: 0 }
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }

    fn peek_at(&self, off: usize) -> &Tok {
        let idx = (self.pos + off).min(self.toks.len() - 1);
        &self.toks[idx]
    }

    fn advance(&mut self) -> Tok {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn same(a: &Tok, b: &Tok) -> bool {
        std::mem::discriminant(a) == std::mem::discriminant(b)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if Self::same(self.peek(), t) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &Tok) -> Result<(), String> {
        if Self::same(self.peek(), t) {
            self.advance();
            Ok(())
        } else {
            Err(format!("expected {:?}, got {:?}", t, self.peek()))
        }
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.advance() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("expected identifier, got {:?}", other)),
        }
    }

    // ---------------- program ----------------

    pub fn parse_program(&mut self) -> Result<Vec<Stmt>, String> {
        let mut stmts = Vec::new();
        while *self.peek() != Tok::Eof {
            stmts.push(self.parse_stmt()?);
        }
        Ok(stmts)
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(&Tok::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        match self.peek().clone() {
            Tok::Let | Tok::Var => self.parse_var_decl(),
            Tok::Func => self.parse_func_decl(),
            Tok::Struct => self.parse_struct_decl(),
            Tok::If => self.parse_if(),
            Tok::While => self.parse_while(),
            Tok::For => self.parse_for(),
            Tok::Return => {
                self.advance();
                if self.eat(&Tok::Semi) {
                    Ok(Stmt::Return(None))
                } else {
                    let e = self.parse_expr()?;
                    self.expect(&Tok::Semi)?;
                    Ok(Stmt::Return(Some(e)))
                }
            }
            Tok::Break => {
                self.advance();
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Break)
            }
            Tok::Continue => {
                self.advance();
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Continue)
            }
            Tok::AsmRaw(raw) => {
                self.advance();
                Ok(Stmt::Asm(raw))
            }
            Tok::Asm => {
                // asm without braces is an error
                Err("asm must be followed by a brace block".into())
            }
            Tok::LBrace => {
                let b = self.parse_block()?;
                Ok(Stmt::Block(b))
            }
            Tok::Ident(name) if Self::same(self.peek_at(1), &Tok::Assign) => {
                // assignment statement
                self.advance();
                self.advance();
                let e = self.parse_expr()?;
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Assign(name, e))
            }
            _ => {
                let e = self.parse_expr()?;
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn parse_var_decl(&mut self) -> Result<Stmt, String> {
        let is_let = matches!(self.peek(), Tok::Let);
        self.advance();
        let name = self.expect_ident()?;
        let ty = if self.eat(&Tok::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let init = if self.eat(&Tok::Assign) {
            self.parse_expr()?
        } else {
            Expr::Int(0)
        };
        self.expect(&Tok::Semi)?;
        if is_let {
            Ok(Stmt::Let(name, ty, init))
        } else {
            Ok(Stmt::Var(name, ty, init))
        }
    }

    fn parse_func_decl(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::Func)?;
        let name = self.expect_ident()?;
        self.expect(&Tok::LParen)?;
        let mut params = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                let p = self.expect_ident()?;
                self.expect(&Tok::Colon)?;
                let t = self.parse_type()?;
                params.push((p, t));
                if self.eat(&Tok::Comma) {
                    continue;
                }
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        let ret = if self.eat(&Tok::Arrow) {
            self.parse_type()?
        } else {
            Type::Void
        };
        let body = self.parse_block()?;
        Ok(Stmt::Func(FuncDecl {
            name,
            params,
            ret,
            body,
        }))
    }

    fn parse_struct_decl(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::Struct)?;
        let name = self.expect_ident()?;
        self.expect(&Tok::LBrace)?;
        let mut fields = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            let f = self.expect_ident()?;
            self.expect(&Tok::Colon)?;
            let t = self.parse_type()?;
            self.expect(&Tok::Semi)?;
            fields.push((f, t));
        }
        self.expect(&Tok::RBrace)?;
        self.eat(&Tok::Semi);
        Ok(Stmt::Struct(StructDecl { name, fields }))
    }

    fn parse_if(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::If)?;
        self.expect(&Tok::LParen)?;
        let cond = self.parse_expr()?;
        self.expect(&Tok::RParen)?;
        let then_b = self.parse_block()?;
        let else_b = if self.eat(&Tok::Else) {
            if *self.peek() == Tok::If {
                vec![self.parse_stmt()?]
            } else {
                self.parse_block()?
            }
        } else {
            Vec::new()
        };
        Ok(Stmt::If(cond, then_b, else_b))
    }

    fn parse_while(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::While)?;
        self.expect(&Tok::LParen)?;
        let cond = self.parse_expr()?;
        self.expect(&Tok::RParen)?;
        let body = self.parse_block()?;
        Ok(Stmt::While(cond, body))
    }

    fn parse_simple_stmt(&mut self) -> Result<Stmt, String> {
        // for-loop init/update: no trailing semicolon
        match self.peek().clone() {
            Tok::Let | Tok::Var => {
                let is_let = matches!(self.peek(), Tok::Let);
                self.advance();
                let name = self.expect_ident()?;
                let ty = if self.eat(&Tok::Colon) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                let init = if self.eat(&Tok::Assign) {
                    self.parse_expr()?
                } else {
                    Expr::Int(0)
                };
                if is_let {
                    Ok(Stmt::Let(name, ty, init))
                } else {
                    Ok(Stmt::Var(name, ty, init))
                }
            }
            Tok::Ident(name) if Self::same(self.peek_at(1), &Tok::Assign) => {
                self.advance();
                self.advance();
                let e = self.parse_expr()?;
                Ok(Stmt::Assign(name, e))
            }
            _ => {
                let e = self.parse_expr()?;
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn parse_for(&mut self) -> Result<Stmt, String> {
        self.expect(&Tok::For)?;
        self.expect(&Tok::LParen)?;
        let init = self.parse_simple_stmt()?;
        self.expect(&Tok::Semi)?;
        let cond = self.parse_expr()?;
        self.expect(&Tok::Semi)?;
        let update = self.parse_simple_stmt()?;
        self.expect(&Tok::RParen)?;
        let body = self.parse_block()?;
        Ok(Stmt::For(Box::new(init), cond, Box::new(update), body))
    }

    // ---------------- types ----------------

    pub fn parse_type(&mut self) -> Result<Type, String> {
        if self.eat(&Tok::Star) {
            let inner = self.parse_type()?;
            return Ok(Type::Ptr(Box::new(inner)));
        }
        let name = match self.peek().clone() {
            Tok::Ident(s) => {
                self.advance();
                s
            }
            other => return Err(format!("expected type name, got {:?}", other)),
        };
        let mut ty = match name.as_str() {
            "int" => Type::Int,
            "uint" => Type::UInt,
            "float" | "double" => Type::Float,
            "bool" => Type::Bool,
            "char" => Type::Char,
            "byte" => Type::Byte,
            "void" => Type::Void,
            "string" => Type::String,
            _ => Type::Named(name),
        };
        if self.eat(&Tok::LBracket) {
            let n = match self.peek() {
                Tok::Int(v) => *v,
                _ => return Err("array size must be an integer literal".into()),
            };
            self.advance();
            self.expect(&Tok::RBracket)?;
            ty = Type::Array(Box::new(ty), n as usize);
        }
        Ok(ty)
    }

    // ---------------- expressions ----------------

    pub fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, String> {
        let lhs = self.parse_or()?;
        if self.eat(&Tok::Assign) {
            let rhs = self.parse_assignment()?;
            return Ok(match lhs {
                Expr::Ident(name) => Expr::Assign(name, Box::new(rhs)),
                other => Expr::AssignLValue(Box::new(other), Box::new(rhs)),
            });
        }
        Ok(lhs)
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        loop {
            if self.eat(&Tok::Or) || self.eat(&Tok::PipePipe) {
                let right = self.parse_and()?;
                left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_bitor()?;
        loop {
            if self.eat(&Tok::And) || self.eat(&Tok::AmpAmp) {
                let right = self.parse_bitor()?;
                left = Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_bitor(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_bitxor()?;
        while self.eat(&Tok::Pipe) {
            let right = self.parse_bitxor()?;
            left = Expr::Binary(BinOp::BitOr, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_bitxor(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_bitand()?;
        while self.eat(&Tok::Caret) {
            let right = self.parse_bitand()?;
            left = Expr::Binary(BinOp::BitXor, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_bitand(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_equality()?;
        while self.eat(&Tok::Amp) {
            let right = self.parse_equality()?;
            left = Expr::Binary(BinOp::BitAnd, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_relational()?;
        loop {
            if self.eat(&Tok::EqEq) {
                let right = self.parse_relational()?;
                left = Expr::Binary(BinOp::Eq, Box::new(left), Box::new(right));
            } else if self.eat(&Tok::NotEq) {
                let right = self.parse_relational()?;
                left = Expr::Binary(BinOp::Ne, Box::new(left), Box::new(right));
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_relational(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_shift()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => BinOp::Lt,
                Tok::Gt => BinOp::Gt,
                Tok::Le => BinOp::Le,
                Tok::Ge => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.parse_shift()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_shift(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                Tok::Shl => BinOp::Shl,
                Tok::Shr => BinOp::Shr,
                _ => break,
            };
            self.advance();
            let right = self.parse_additive()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Tok::Minus => {
                self.advance();
                Ok(Expr::Unary(UnOp::Neg, Box::new(self.parse_unary()?)))
            }
            Tok::Not => {
                self.advance();
                Ok(Expr::Unary(UnOp::Not, Box::new(self.parse_unary()?)))
            }
            Tok::Tilde => {
                self.advance();
                Ok(Expr::Unary(UnOp::BitNot, Box::new(self.parse_unary()?)))
            }
            Tok::Star => {
                self.advance();
                Ok(Expr::Unary(UnOp::Deref, Box::new(self.parse_unary()?)))
            }
            Tok::Amp => {
                self.advance();
                Ok(Expr::Unary(UnOp::AddrOf, Box::new(self.parse_unary()?)))
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_primary()?;
        loop {
            match self.peek().clone() {
                Tok::LParen => {
                    // call only makes sense on a plain identifier
                    if let Expr::Ident(name) = &e {
                        self.advance();
                        let mut args = Vec::new();
                        if *self.peek() != Tok::RParen {
                            loop {
                                args.push(self.parse_expr()?);
                                if self.eat(&Tok::Comma) {
                                    continue;
                                }
                                break;
                            }
                        }
                        self.expect(&Tok::RParen)?;
                        e = Expr::Call(name.clone(), args);
                    } else {
                        return Err("only named functions can be called".into());
                    }
                }
                Tok::LBracket => {
                    self.advance();
                    let idx = self.parse_expr()?;
                    self.expect(&Tok::RBracket)?;
                    e = Expr::Index(Box::new(e), Box::new(idx));
                }
                Tok::Dot => {
                    self.advance();
                    let field = self.expect_ident()?;
                    e = Expr::Member(Box::new(e), field);
                }
                Tok::Arrow => {
                    self.advance();
                    let field = self.expect_ident()?;
                    e = Expr::Arrow(Box::new(e), field);
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.advance() {
            Tok::Int(v) => Ok(Expr::Int(v)),
            Tok::Float(v) => Ok(Expr::Float(v)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Char(c) => Ok(Expr::Char(c)),
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            Tok::Null => Ok(Expr::Null),
            Tok::Ident(name) => Ok(Expr::Ident(name)),
            Tok::LParen => {
                let e = self.parse_expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::Cast => {
                self.expect(&Tok::Lt)?;
                let ty = self.parse_type()?;
                self.expect(&Tok::Gt)?;
                self.expect(&Tok::LParen)?;
                let e = self.parse_expr()?;
                self.expect(&Tok::RParen)?;
                Ok(Expr::Cast(ty, Box::new(e)))
            }
            Tok::Sizeof => {
                self.expect(&Tok::LParen)?;
                let ty = self.parse_type()?;
                self.expect(&Tok::RParen)?;
                Ok(Expr::Sizeof(ty))
            }
            other => Err(format!("unexpected token in expression: {:?}", other)),
        }
    }
}
