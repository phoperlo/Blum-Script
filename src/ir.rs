#![allow(dead_code)]
//! Three-address intermediate representation.
//!
//! The code generator is split into `AST -> IR -> x64`.  This module defines
//! the middle layer only: a per-function list of basic blocks, each with a
//! linear sequence of instructions over virtual registers and a terminating
//! jump.
//!
//! Design notes
//! ------------
//! * Values live in *virtual registers* (`VReg`).  Every vreg has a `Kind`
//!   (integer width, `f64`, pointer, or an aggregate blob) which drives both
//!   register class selection and load/store width.
//! * Addressable locals are **not** vregs: they stay in the frame and are
//!   reached through the `FRAME` pseudo-register.  Scalars that are never
//!   address-taken get promoted to vregs by `irgen` (see `Loc::Reg`), which is
//!   what gives the register allocator something long-lived to work with.
//! * There are no phi nodes.  The IR is deliberately non-SSA: a vreg may be
//!   assigned more than once (that is how promoted locals work).  Live
//!   intervals are therefore computed conservatively as
//!   `[min position, max position]` over all defs and uses.
//! * Three pseudo-registers stand for the addressing bases that have no
//!   physical register: `FRAME` (`[rbp + disp]`), `DATA` (`[rip + disp]` into
//!   `.data`) and `RDATA` (`[rip + disp]` into `.rdata`).

use std::fmt;

pub type VReg = u32;
pub type BlockId = u32;

/// `[rbp + disp]`
pub const FRAME: VReg = 0;
/// `[rip + disp]` into `.data`
pub const DATA: VReg = 1;
/// `[rip + disp]` into `.rdata`
pub const RDATA: VReg = 2;
/// First ordinary virtual register.
pub const FIRST_VREG: VReg = 3;

// ---------------------------------------------------------------------------
// value kinds
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    I8,
    I16,
    I32,
    I64,
    /// `uint`, `byte` вЂ” same width as `I64` for arithmetic, unsigned compare.
    U64,
    F64,
    Ptr,
    /// Aggregate of `n` bytes: always manipulated through its address.
    Agg(u32),
}

impl Kind {
    pub fn size(self) -> u32 {
        match self {
            Kind::I8 => 1,
            Kind::I16 => 2,
            Kind::I32 => 4,
            Kind::I64 | Kind::U64 | Kind::F64 | Kind::Ptr => 8,
            Kind::Agg(n) => n,
        }
    }
    pub fn is_float(self) -> bool {
        matches!(self, Kind::F64)
    }
    pub fn is_agg(self) -> bool {
        matches!(self, Kind::Agg(_))
    }
    pub fn is_ptr(self) -> bool {
        matches!(self, Kind::Ptr)
    }
    pub fn is_unsigned(self) -> bool {
        matches!(self, Kind::U64 | Kind::Ptr)
    }
    /// Load/store width class used by the emitter (`0` means "aggregate").
    pub fn mem_width(self) -> u8 {
        if self.is_agg() {
            0
        } else {
            self.size().min(8) as u8
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::I8 => "i8",
            Kind::I16 => "i16",
            Kind::I32 => "i32",
            Kind::I64 => "i64",
            Kind::U64 => "u64",
            Kind::F64 => "f64",
            Kind::Ptr => "ptr",
            Kind::Agg(_) => "agg",
        }
    }
}

// ---------------------------------------------------------------------------
// operators
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IBin {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Sar,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FBin {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IUn {
    Neg,
    /// logical `!` вЂ” result is 0/1
    Not,
    BitNot,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FUn {
    Neg,
    /// logical `!` вЂ” result is 0/1 (NaN counts as true)
    Not,
}

impl fmt::Display for IBin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            IBin::Add => "add",
            IBin::Sub => "sub",
            IBin::Mul => "mul",
            IBin::And => "and",
            IBin::Or => "or",
            IBin::Xor => "xor",
            IBin::Shl => "shl",
            IBin::Shr => "shr",
            IBin::Sar => "sar",
        };
        f.write_str(s)
    }
}

impl fmt::Display for FBin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            FBin::Add => "fadd",
            FBin::Sub => "fsub",
            FBin::Mul => "fmul",
            FBin::Div => "fdiv",
        };
        f.write_str(s)
    }
}

impl fmt::Display for Cmp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Cmp::Eq => "eq",
            Cmp::Ne => "ne",
            Cmp::Lt => "lt",
            Cmp::Le => "le",
            Cmp::Gt => "gt",
            Cmp::Ge => "ge",
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// instructions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Inst {
    /// `dst = imm`
    Const(VReg, i64),
    /// `dst = <double constant>`; the emitter interns the bits in `.rdata`.
    FConst(VReg, f64),
    /// `dst = &global` вЂ” `lea rax, [rip + off]` into `.data`
    AddrGlobal(VReg, u32),
    /// `dst = &literal` вЂ” `lea rax, [rip + off]` into `.rdata`
    AddrRdata(VReg, u32),
    /// `dst = &local` вЂ” `lea rax, [rbp + disp]`
    AddrLocal(VReg, i32),
    /// `dst = base + disp` for an arbitrary pointer base
    LeaBase { dst: VReg, base: VReg, disp: i32 },
    /// Zero `bytes` at the address in `dst`.
    Zero { dst: VReg, bytes: u32 },
    /// `dst = src`
    Copy(VReg, VReg),
    Load {
        dst: VReg,
        base: VReg,
        disp: i32,
        kind: Kind,
    },
    Store {
        base: VReg,
        disp: i32,
        src: VReg,
        kind: Kind,
    },
    /// `memcpy(dst, src, bytes)` вЂ” `dst`/`src` are addresses.
    MemCopy {
        dst: VReg,
        src: VReg,
        bytes: u32,
    },
    Bin {
        dst: VReg,
        op: IBin,
        a: VReg,
        b: VReg,
    },
    /// `dst = a / b` (or `a % b` when `rem`), signedness explicit.
    IDiv {
        dst: VReg,
        a: VReg,
        b: VReg,
        unsigned: bool,
        rem: bool,
    },
    FBin {
        dst: VReg,
        op: FBin,
        a: VReg,
        b: VReg,
    },
    ICmp {
        dst: VReg,
        cc: Cmp,
        a: VReg,
        b: VReg,
        unsigned: bool,
    },
    FCmp {
        dst: VReg,
        cc: Cmp,
        a: VReg,
        b: VReg,
    },
    Un {
        dst: VReg,
        op: IUn,
        a: VReg,
    },
    FUn {
        dst: VReg,
        op: FUn,
        a: VReg,
    },
    /// `dst = base + index * (1 << scale) + disp`
    Lea {
        dst: VReg,
        base: VReg,
        index: VReg,
        scale: u8,
        disp: i32,
    },
    /// `dst = (to) src`
    Cast {
        dst: VReg,
        from: Kind,
        to: Kind,
        src: VReg,
    },
    /// Call into a BlumScript function.  ABI staging is derived from `kinds`
    /// exactly like the legacy backend does.
    Call {
        dst: Option<VReg>,
        name: String,
        args: Vec<VReg>,
        kinds: Vec<Kind>,
        variadic: bool,
        ret: Kind,
        /// Frame slot of the hidden return buffer (aggregates > 8 bytes).
        ret_slot: Option<i32>,
    },
    /// Call into the linked runtime (`bs_print_int`, ...).  `gpr` are loaded
    /// into RCX, RDX, R8 in order; `xmm` into XMM0.
    RtCall {
        dst: Option<VReg>,
        name: String,
        gpr: Vec<VReg>,
        xmm: Option<VReg>,
    },
}

impl Inst {
    pub fn def(&self) -> Option<VReg> {
        match self {
            Inst::Const(d, _)
            | Inst::FConst(d, _)
            | Inst::AddrGlobal(d, _)
            | Inst::AddrRdata(d, _)
            | Inst::AddrLocal(d, _)
            | Inst::LeaBase { dst: d, .. }
            | Inst::Copy(d, _)
            | Inst::Load { dst: d, .. }
            | Inst::Bin { dst: d, .. }
            | Inst::IDiv { dst: d, .. }
            | Inst::FBin { dst: d, .. }
            | Inst::ICmp { dst: d, .. }
            | Inst::FCmp { dst: d, .. }
            | Inst::Un { dst: d, .. }
            | Inst::FUn { dst: d, .. }
            | Inst::Lea { dst: d, .. }
            | Inst::Cast { dst: d, .. } => Some(*d),
            Inst::Call { dst, .. } | Inst::RtCall { dst, .. } => *dst,
            Inst::Store { .. } | Inst::MemCopy { .. } | Inst::Zero { .. } => None,
        }
    }

    /// Does the instruction have an observable effect other than defining
    /// `def()`?  Such instructions must survive dead-code elimination.
    pub fn has_effect(&self) -> bool {
        matches!(
            self,
            Inst::Store { .. }
                | Inst::MemCopy { .. }
                | Inst::Zero { .. }
                | Inst::Call { .. }
                | Inst::RtCall { .. }
        )
    }

    /// Can the instruction be discarded when its result is unused?
    pub fn is_pure(&self) -> bool {
        !self.has_effect()
    }

    pub fn uses(&self) -> Vec<VReg> {
        match self {
            Inst::Const(..)
            | Inst::FConst(..)
            | Inst::AddrGlobal(..)
            | Inst::AddrRdata(..)
            | Inst::AddrLocal(..) => Vec::new(),
            Inst::LeaBase { base, .. } => vec![*base],
            Inst::Zero { dst, .. } => vec![*dst],
            Inst::Copy(_, s) => vec![*s],
            Inst::Load { base, .. } => vec![*base],
            Inst::Store { base, src, .. } => vec![*base, *src],
            Inst::MemCopy { dst, src, .. } => vec![*dst, *src],
            Inst::Bin { a, b, .. }
            | Inst::IDiv { a, b, .. }
            | Inst::FBin { a, b, .. }
            | Inst::ICmp { a, b, .. }
            | Inst::FCmp { a, b, .. } => vec![*a, *b],
            Inst::Un { a, .. } | Inst::FUn { a, .. } | Inst::Cast { src: a, .. } => vec![*a],
            Inst::Lea { base, index, .. } => vec![*base, *index],
            Inst::Call { args, .. } => args.clone(),
            Inst::RtCall { gpr, xmm, .. } => {
                let mut v = gpr.clone();
                v.extend(xmm.iter().copied());
                v
            }
        }
    }

    /// Physical GP registers destroyed by this instruction (bit `n` == `reg n`).
    /// Used by the allocator to decide which registers a value may *not* live
    /// in if its interval covers this instruction.
    pub fn clobbers(&self) -> u16 {
        use gp::*;
        match self {
            Inst::IDiv { .. } => AX | DX | CX,
            Inst::Zero { .. } => AX | CX,
            Inst::Bin { op, .. } => match op {
                IBin::Shl | IBin::Shr | IBin::Sar => AX | CX,
                _ => AX,
            },
            Inst::ICmp { .. } | Inst::FCmp { .. } => AX,
            Inst::Un { op, .. } => match op {
                IUn::Neg | IUn::BitNot => AX,
                IUn::Not => AX,
            },
            Inst::FUn { .. } => AX | R11,
            Inst::Cast { .. } => AX,
            Inst::MemCopy { .. } => AX | CX | DX | SI | DI,
            Inst::Call { kinds, .. } => {
                // Staging an aggregate argument larger than 8 bytes runs a
                // REP MOVSB, which trashes RSI/RDI.
                let mut m = CALLER_SAVED_GP;
                if kinds.iter().any(|k| k.is_agg() && k.size() > 8) {
                    m |= SI | DI;
                }
                m
            }
            Inst::RtCall { .. } => CALLER_SAVED_GP,
            _ => 0,
        }
    }

    /// Destroys every XMM register (a call into foreign code).
    pub fn clobbers_xmm(&self) -> bool {
        matches!(self, Inst::Call { .. } | Inst::RtCall { .. } | Inst::MemCopy { .. })
    }
}

/// GP register bit constants (indexes match `x64::REG_*`).
pub mod gp {
    pub const AX: u16 = 1 << 0;
    pub const CX: u16 = 1 << 1;
    pub const DX: u16 = 1 << 2;
    pub const BX: u16 = 1 << 3;
    pub const SP: u16 = 1 << 4;
    pub const BP: u16 = 1 << 5;
    pub const SI: u16 = 1 << 6;
    pub const DI: u16 = 1 << 7;
    pub const R8: u16 = 1 << 8;
    pub const R9: u16 = 1 << 9;
    pub const R10: u16 = 1 << 10;
    pub const R11: u16 = 1 << 11;
    pub const R12: u16 = 1 << 12;
    pub const R13: u16 = 1 << 13;
    pub const R14: u16 = 1 << 14;
    pub const R15: u16 = 1 << 15;

    /// RBX, RSI, RDI, R12..R15 вЂ” preserved across calls.
    pub const CALLEE_SAVED_GP: u16 = BX | SI | DI | R12 | R13 | R14 | R15;
    /// RAX, RCX, RDX, R8..R11 вЂ” destroyed by a call.
    pub const CALLER_SAVED_GP: u16 = AX | CX | DX | R8 | R9 | R10 | R11;
    /// Registers the allocator may hand out (RSP/RBP are reserved).
    pub const ALLOC_GP: u16 = CALLEE_SAVED_GP | CALLER_SAVED_GP;

    pub fn name(r: u8) -> &'static str {
        const NAMES: [&str; 16] = [
            "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11",
            "r12", "r13", "r14", "r15",
        ];
        NAMES[r as usize]
    }
}

// ---------------------------------------------------------------------------
// argument classification
// ---------------------------------------------------------------------------

/// Where an argument lives under the Microsoft x64 ABI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Gpr(usize),
    Xmm(usize),
    Stack(usize),
}

/// Classify `kinds` exactly like the legacy backend: floats take `XMM0..3`
/// (separate counter), everything else `RCX/RDX/R8/R9`, overflow goes to the
/// stack in order.
pub fn abi_slots(kinds: &[Kind], gpr_start: usize) -> Vec<Slot> {
    let mut out = Vec::with_capacity(kinds.len());
    let mut ik = gpr_start;
    let mut fk = 0usize;
    let mut sk = 0usize;
    for k in kinds {
        if k.is_float() {
            if fk < 4 {
                out.push(Slot::Xmm(fk));
                fk += 1;
                continue;
            }
        } else if ik < 4 {
            out.push(Slot::Gpr(ik));
            ik += 1;
            continue;
        }
        out.push(Slot::Stack(sk));
        sk += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// terminators / blocks / function
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Term {
    Goto(BlockId),
    /// Jump to `t` when `cond` is truthy, otherwise to `f`.
    Branch { cond: VReg, t: BlockId, f: BlockId },
    /// `return` with an optional value.  For aggregates larger than 8 bytes
    /// the value is the *address* of the result.
    Ret(Option<VReg>),
    /// Unreachable (used to close a block after `ret`).
    Trap,
}

impl Term {
    pub fn uses(&self) -> Vec<VReg> {
        match self {
            Term::Branch { cond, .. } => vec![*cond],
            Term::Ret(Some(v)) => vec![*v],
            _ => Vec::new(),
        }
    }
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Term::Goto(b) => vec![*b],
            Term::Branch { t, f, .. } => vec![*t, *f],
            _ => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub term: Option<Term>,
    /// Source line hint, for `--emit-ir` readability.
    pub label: String,
}

/// Where an incoming argument register is stashed by the prologue.  The
/// emitter performs these stores *before* the first IR instruction so that the
/// register shuffling of parameter homing can never alias.
#[derive(Clone, Copy, Debug)]
pub enum Home {
    /// `mov [rbp + disp], reg` with the given store width (1, 2, 4 or 8)
    Gpr(u8, i32, u8),
    /// `movsd [rbp + disp], xmm`
    Xmm(u8, i32),
}

pub struct Function {
    pub name: String,
    /// Kind of every vreg, indexed by id (pseudo-registers included).
    pub kinds: Vec<Kind>,
    pub blocks: Vec<Block>,
    pub variadic: bool,
    /// Return kind of the function.
    pub ret: Kind,
    /// Frame bytes needed by the IR (locals + spill slots + saved regs).
    pub frame: usize,
    /// Callee-saved registers used by the allocation, with the frame slot
    /// they are homed to in the prologue: `(phys reg, disp)`.
    pub saves: Vec<(u8, i32)>,
    /// Incoming argument registers to spill into the frame in the prologue.
    pub param_homes: Vec<Home>,
    /// Hidden return buffer for aggregates larger than 8 bytes: frame slot of
    /// the caller-provided pointer and the number of bytes to copy.
    pub ret_buf: Option<(i32, u32)>,
}

impl Function {
    pub fn kind(&self, v: VReg) -> Kind {
        self.kinds[v as usize]
    }

    /// Number of instructions in layout order (used for live intervals).
    pub fn position_of(&self) -> Vec<Vec<u32>> {
        let mut out = Vec::with_capacity(self.blocks.len());
        let mut pos = 0u32;
        for b in &self.blocks {
            let mut v = Vec::with_capacity(b.insts.len() + 1);
            for _ in 0..b.insts.len() {
                v.push(pos);
                pos += 1;
            }
            // the terminator occupies one slot too, so branch-only blocks
            // still get distinct positions
            v.push(pos);
            pos += 1;
            out.push(v);
        }
        out
    }

    pub fn inst_count(&self) -> usize {
        self.blocks.iter().map(|b| b.insts.len() + 1).sum()
    }

    pub fn dump(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "func {}  ; ret {}, frame {} bytes{}\n",
            self.name,
            self.ret.name(),
            self.frame,
            if self.variadic { ", variadic" } else { "" }
        ));
        for (i, b) in self.blocks.iter().enumerate() {
            s.push_str(&format!(".B{}", i));
            if !b.label.is_empty() {
                s.push_str(&format!("  ; {}", b.label));
            }
            s.push('\n');
            for inst in &b.insts {
                s.push_str("    ");
                s.push_str(&self.fmt_inst(inst));
                s.push('\n');
            }
            if let Some(t) = &b.term {
                s.push_str("    ");
                s.push_str(&self.fmt_term(t));
                s.push('\n');
            }
        }
        s
    }

    fn v(&self, r: VReg) -> String {
        match r {
            FRAME => "rbp".to_string(),
            DATA => "data".to_string(),
            RDATA => "rdata".to_string(),
            _ => format!("v{}", r),
        }
    }

    fn fmt_inst(&self, i: &Inst) -> String {
        match i {
            Inst::Const(d, v) => format!("{} = {}", self.v(*d), v),
            Inst::FConst(d, v) => format!("{} = {}f", self.v(*d), v),
            Inst::AddrGlobal(d, o) => format!("{} = &data+{}", self.v(*d), o),
            Inst::AddrRdata(d, o) => format!("{} = &rdata+{}", self.v(*d), o),
            Inst::AddrLocal(d, o) => format!("{} = &rbp{:+}", self.v(*d), o),
            Inst::LeaBase { dst, base, disp } => {
                format!("{} = lea {} {:+}", self.v(*dst), self.v(*base), disp)
            }
            Inst::Zero { dst, bytes } => format!("zero {} @ {}", bytes, self.v(*dst)),
            Inst::Copy(d, s) => format!("{} = copy {}", self.v(*d), self.v(*s)),
            Inst::Load {
                dst,
                base,
                disp,
                kind,
            } => format!(
                "{} = load.{} [{}]{:+}",
                self.v(*dst),
                kind.name(),
                self.v(*base),
                disp
            ),
            Inst::Store {
                base,
                disp,
                src,
                kind,
            } => format!(
                "store.{} [{}]{:+}, {}",
                kind.name(),
                self.v(*base),
                disp,
                self.v(*src)
            ),
            Inst::MemCopy { dst, src, bytes } => {
                format!("memcpy {} <- {}, {}", self.v(*dst), self.v(*src), bytes)
            }
            Inst::Bin { dst, op, a, b } => {
                format!("{} = {} {}, {}", self.v(*dst), op, self.v(*a), self.v(*b))
            }
            Inst::IDiv {
                dst,
                a,
                b,
                unsigned,
                rem,
            } => format!(
                "{} = {}{} {}, {}",
                self.v(*dst),
                if *rem { "rem" } else { "div" },
                if *unsigned { "u" } else { "s" },
                self.v(*a),
                self.v(*b)
            ),
            Inst::FBin { dst, op, a, b } => {
                format!("{} = {} {}, {}", self.v(*dst), op, self.v(*a), self.v(*b))
            }
            Inst::ICmp {
                dst,
                cc,
                a,
                b,
                unsigned,
            } => format!(
                "{} = cmp{}{} {}, {}",
                self.v(*dst),
                if *unsigned { "u" } else { "s" },
                cc,
                self.v(*a),
                self.v(*b)
            ),
            Inst::FCmp { dst, cc, a, b } => {
                format!("{} = fcmp{} {}, {}", self.v(*dst), cc, self.v(*a), self.v(*b))
            }
            Inst::Un { dst, op, a } => format!(
                "{} = {:?} {}",
                self.v(*dst),
                op,
                self.v(*a)
            ),
            Inst::FUn { dst, op, a } => format!(
                "{} = f{:?} {}",
                self.v(*dst),
                op,
                self.v(*a)
            ),
            Inst::Lea {
                dst,
                base,
                index,
                scale,
                disp,
            } => format!(
                "{} = lea {} + {}*{} {:+}",
                self.v(*dst),
                self.v(*base),
                self.v(*index),
                1 << scale,
                disp
            ),
            Inst::Cast {
                dst,
                from,
                to,
                src,
            } => format!(
                "{} = cast {} -> {} {}",
                self.v(*dst),
                from.name(),
                to.name(),
                self.v(*src)
            ),
            Inst::Call {
                dst,
                name,
                args,
                ..
            } => format!(
                "{}call {}({})",
                match dst {
                    Some(d) => format!("{} = ", self.v(*d)),
                    None => String::new(),
                },
                name,
                args.iter()
                    .map(|a| self.v(*a))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Inst::RtCall {
                dst,
                name,
                gpr,
                xmm,
            } => {
                let mut all: Vec<String> = gpr.iter().map(|g| self.v(*g)).collect();
                if let Some(x) = xmm {
                    all.push(format!("xmm {}", self.v(*x)));
                }
                format!(
                    "{}call @{}({})",
                    match dst {
                        Some(d) => format!("{} = ", self.v(*d)),
                        None => String::new(),
                    },
                    name,
                    all.join(", ")
                )
            }
        }
    }

    fn fmt_term(&self, t: &Term) -> String {
        match t {
            Term::Goto(b) => format!("goto .B{}", b),
            Term::Branch { cond, t, f } => {
                format!("branch {} ? .B{} : .B{}", self.v(*cond), t, f)
            }
            Term::Ret(Some(v)) => format!("ret {}", self.v(*v)),
            Term::Ret(None) => "ret".to_string(),
            Term::Trap => "trap".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// builder
// ---------------------------------------------------------------------------

pub struct Builder {
    pub func: Function,
    cur: BlockId,
}

impl Builder {
    pub fn new(name: &str, ret: Kind, variadic: bool) -> Builder {
        Builder {
            func: Function {
                name: name.to_string(),
                kinds: vec![Kind::Ptr, Kind::Ptr, Kind::Ptr],
                blocks: vec![Block::default()],
                variadic,
                ret,
                frame: 0,
                saves: Vec::new(),
                param_homes: Vec::new(),
                ret_buf: None,
            },
            cur: 0,
        }
    }

    pub fn vreg(&mut self, k: Kind) -> VReg {
        let id = self.func.kinds.len() as VReg;
        self.func.kinds.push(k);
        id
    }

    pub fn push(&mut self, i: Inst) {
        self.func.blocks[self.cur as usize].insts.push(i);
    }

    pub fn set_term(&mut self, t: Term) {
        self.func.blocks[self.cur as usize].term = Some(t);
    }

    pub fn term(&self) -> Option<&Term> {
        self.func.blocks[self.cur as usize].term.as_ref()
    }

    /// Start a new (empty) block and return its id.  The caller is responsible
    /// for terminating the previous one.
    pub fn block(&mut self) -> BlockId {
        let id = self.func.blocks.len() as BlockId;
        self.func.blocks.push(Block::default());
        self.cur = id;
        id
    }

    pub fn switch(&mut self, b: BlockId) {
        self.cur = b;
    }

    pub fn cur(&self) -> BlockId {
        self.cur
    }

    pub fn label(&mut self, s: &str) {
        if self.func.blocks[self.cur as usize].label.is_empty() {
            self.func.blocks[self.cur as usize].label = s.to_string();
        }
    }

    /// Allocate `size` bytes in the frame and return the (negative) displacement
    /// from RBP, in the same convention as `Native::alloc`.
    pub fn alloc(&mut self, size: usize) -> i32 {
        let s = (size.max(1) + 7) & !7;
        self.func.frame += s;
        -(self.func.frame as i32)
    }

    pub fn finish(self) -> Function {
        self.func
    }
}
