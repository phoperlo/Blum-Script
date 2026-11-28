//! Middle-end optimisations over [`crate::ir`].
//!
//! Passes (all conservative, semantics-preserving):
//!
//! 1. **Constant folding / propagation** вЂ” a virtual register with exactly one
//!    definition that is a literal is treated as a constant everywhere in the
//!    function; instructions whose operands are all known constants are
//!    evaluated at compile time.
//! 2. **Dead code elimination** вЂ” blocks unreachable from the entry block are
//!    emptied, and pure instructions whose result is never used are removed
//!    (side-effecting instructions are always kept).
//! 3. **Peephole / strength reduction** вЂ” `x * 2^k -> x shl k`,
//!    `u / 2^k -> u shr k`, `u % 2^k -> u and (2^k - 1)`, plus the usual
//!    algebraic identities (`x + 0`, `x * 1`, `x - x`, ...) and self-copy
//!    removal with single-use copy propagation.

use std::collections::{HashMap, HashSet};

use crate::ir::*;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Const {
    I(i64),
    F(u64),
}

impl Const {
    fn as_i(self) -> Option<i64> {
        match self {
            Const::I(v) => Some(v),
            _ => None,
        }
    }
    fn as_f(self) -> Option<f64> {
        match self {
            Const::F(b) => Some(f64::from_bits(b)),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Stats {
    pub folded: usize,
    pub removed: usize,
    pub strength: usize,
    pub unreachable: usize,
}


/// Run every pass until the function stops changing (bounded).
pub fn optimize(f: &mut Function) -> Stats {
    let mut total = Stats::default();
    for _ in 0..8 {
        let mut round = Stats::default();
        round.unreachable += reachability(f);
        round.folded += const_prop(f);
        round.removed += dce(f);
        round.strength += peephole(f);
        round.removed += dce(f);
        total.folded += round.folded;
        total.removed += round.removed;
        total.strength += round.strength;
        total.unreachable += round.unreachable;
        if round.folded == 0 && round.removed == 0 && round.strength == 0 && round.unreachable == 0 {
            break;
        }
    }
    total
}

// ---------------------------------------------------------------------------
// analysis helpers
// ---------------------------------------------------------------------------

fn def_counts(f: &Function) -> Vec<u32> {
    let mut n = vec![0u32; f.kinds.len()];
    for b in &f.blocks {
        for i in &b.insts {
            if let Some(d) = i.def() {
                n[d as usize] += 1;
            }
        }
    }
    n
}

fn use_counts(f: &Function) -> Vec<u32> {
    let mut n = vec![0u32; f.kinds.len()];
    for b in &f.blocks {
        for i in &b.insts {
            for u in i.uses() {
                n[u as usize] += 1;
            }
        }
        if let Some(t) = &b.term {
            for u in t.uses() {
                n[u as usize] += 1;
            }
        }
    }
    n
}

/// Remove blocks that cannot be reached from the entry block.
fn reachability(f: &mut Function) -> usize {
    if f.blocks.is_empty() {
        return 0;
    }
    let mut seen = vec![false; f.blocks.len()];
    let mut stack = vec![0u32];
    seen[0] = true;
    while let Some(b) = stack.pop() {
        if let Some(t) = &f.blocks[b as usize].term {
            for s in t.successors() {
                let s = s as usize;
                if s < seen.len() && !seen[s] {
                    seen[s] = true;
                    stack.push(s as u32);
                }
            }
        }
    }
    let mut n = 0;
    for (i, live) in seen.iter().enumerate() {
        if !live {
            let b = &mut f.blocks[i];
            if !b.insts.is_empty() || !matches!(b.term, Some(Term::Trap)) {
                b.insts.clear();
                b.term = Some(Term::Trap);
                n += 1;
            }
        }
    }
    n
}

// ---------------------------------------------------------------------------
// constant folding & propagation
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn clamp_to_kind(k: Kind, v: i64) -> i64 {
    match k {
        Kind::I8 => v as i8 as i64,
        Kind::I16 => v as i16 as i64,
        Kind::I32 => v as i32 as i64,
        _ => v,
    }
}

fn const_prop(f: &mut Function) -> usize {
    let defs = def_counts(f);
    // `known[v]` is only filled for single-assignment vregs, so the value is
    // valid at every program point.
    let mut known: Vec<Option<Const>> = vec![None; f.kinds.len()];
    for b in &f.blocks {
        for i in &b.insts {
            match i {
                Inst::Const(d, v) if defs[*d as usize] == 1 => {
                    known[*d as usize] = Some(Const::I(*v));
                }
                Inst::FConst(d, v) if defs[*d as usize] == 1 => {
                    known[*d as usize] = Some(Const::F(v.to_bits()));
                }
                _ => {}
            }
        }
    }

    let mut changed = 0;
    'rounds: for _ in 0..8 {
        let mut round = 0;
        for bi in 0..f.blocks.len() {
            for ii in 0..f.blocks[bi].insts.len() {
                let inst = f.blocks[bi].insts[ii].clone();
                if let Some(new) = fold_inst(&inst, &known) {
                    if !same_inst(&new, &inst) {
                        if let Some(d) = new.def() {
                            if defs[d as usize] == 1 {
                                match &new {
                                    Inst::Const(_, v) => known[d as usize] = Some(Const::I(*v)),
                                    Inst::FConst(_, v) => {
                                        known[d as usize] = Some(Const::F(v.to_bits()))
                                    }
                                    _ => {}
                                }
                            }
                        }
                        f.blocks[bi].insts[ii] = new;
                        round += 1;
                    }
                }
            }
        }
        changed += round;
        if round == 0 {
            break 'rounds;
        }
    }
    changed
}

/// Structural equality (the IR carries `f64` and `String`, so `PartialEq`
/// cannot be derived; `Debug` round-trips both exactly).
fn same_inst(a: &Inst, b: &Inst) -> bool {
    format!("{:?}", a) == format!("{:?}", b)
}

fn fold_inst(i: &Inst, known: &[Option<Const>]) -> Option<Inst> {
    let k = |v: VReg| known[v as usize];
    match i {
        Inst::Copy(d, s) => {
            let c = k(*s)?;
            Some(match c {
                Const::I(v) => Inst::Const(*d, v),
                Const::F(b) => Inst::FConst(*d, f64::from_bits(b)),
            })
        }
        Inst::Bin { dst, op, a, b } => {
            let ca = k(*a);
            let cb = k(*b);
            // algebraic identities with one constant operand
            if let Some(c) = cb.and_then(|c| c.as_i()) {
                match op {
                    IBin::Add | IBin::Or | IBin::Xor if c == 0 => {
                        return Some(Inst::Copy(*dst, *a))
                    }
                    IBin::Sub | IBin::Shl | IBin::Shr | IBin::Sar if c == 0 => {
                        return Some(Inst::Copy(*dst, *a))
                    }
                    IBin::Mul if c == 1 => return Some(Inst::Copy(*dst, *a)),
                    IBin::Mul if c == 0 => return Some(Inst::Const(*dst, 0)),
                    IBin::And if c == -1 => return Some(Inst::Copy(*dst, *a)),
                    _ => {}
                }
            }
            if let Some(c) = ca.and_then(|c| c.as_i()) {
                match op {
                    IBin::Add if c == 0 => return Some(Inst::Copy(*dst, *b)),
                    IBin::Mul if c == 1 => return Some(Inst::Copy(*dst, *b)),
                    IBin::Mul if c == 0 => return Some(Inst::Const(*dst, 0)),
                    IBin::Or | IBin::Xor if c == 0 => return Some(Inst::Copy(*dst, *b)),
                    IBin::And if c == -1 => return Some(Inst::Copy(*dst, *b)),
                    _ => {}
                }
            }
            let (x, y) = (ca?.as_i()?, cb?.as_i()?);
            let r = match op {
                IBin::Add => x.wrapping_add(y),
                IBin::Sub => x.wrapping_sub(y),
                IBin::Mul => x.wrapping_mul(y),
                IBin::And => x & y,
                IBin::Or => x | y,
                IBin::Xor => x ^ y,
                IBin::Shl => {
                    if !(0..64).contains(&y) {
                        return None;
                    }
                    ((x as u64) << y) as i64
                }
                IBin::Shr => {
                    if !(0..64).contains(&y) {
                        return None;
                    }
                    ((x as u64) >> y) as i64
                }
                IBin::Sar => {
                    if !(0..64).contains(&y) {
                        return None;
                    }
                    x >> y
                }
            };
            Some(Inst::Const(*dst, r))
        }
        Inst::IDiv {
            dst,
            a,
            b,
            unsigned,
            rem,
        } => {
            let (x, y) = (k(*a)?.as_i()?, k(*b)?.as_i()?);
            if y == 0 {
                return None;
            }
            if *unsigned {
                if x < 0 || y < 0 {
                    return None;
                }
                let (x, y) = (x as u64, y as u64);
                Some(Inst::Const(
                    *dst,
                    if *rem { (x % y) as i64 } else { (x / y) as i64 },
                ))
            } else {
                if x == i64::MIN && y == -1 {
                    return None;
                }
                Some(Inst::Const(*dst, if *rem { x % y } else { x / y }))
            }
        }
        Inst::FBin { dst, op, a, b } => {
            let (x, y) = (k(*a)?.as_f()?, k(*b)?.as_f()?);
            let r = match op {
                FBin::Add => x + y,
                FBin::Sub => x - y,
                FBin::Mul => x * y,
                FBin::Div => x / y,
            };
            // keep the runtime's exact bit pattern: do not fold NaN/inf
            if !r.is_finite() {
                return None;
            }
            Some(Inst::FConst(*dst, r))
        }
        Inst::ICmp {
            dst,
            cc,
            a,
            b,
            unsigned,
        } => {
            let (x, y) = (k(*a)?.as_i()?, k(*b)?.as_i()?);
            let r = if *unsigned {
                let (x, y) = (x as u64, y as u64);
                cmp_apply(*cc, x.cmp(&y))
            } else {
                cmp_apply(*cc, x.cmp(&y))
            };
            Some(Inst::Const(*dst, r as i64))
        }
        Inst::FCmp { dst, cc, a, b } => {
            let (x, y) = (k(*a)?.as_f()?, k(*b)?.as_f()?);
            let ord = x.partial_cmp(&y);
            // ucomisd sets ZF/CF/PF; unordered (NaN) only satisfies `ne`
            let r = match ord {
                Some(o) => cmp_apply(*cc, o),
                None => matches!(cc, Cmp::Ne),
            };
            Some(Inst::Const(*dst, r as i64))
        }
        Inst::Un { dst, op, a } => {
            let x = k(*a)?.as_i()?;
            let r = match op {
                IUn::Neg => x.wrapping_neg(),
                IUn::Not => (x == 0) as i64,
                IUn::BitNot => !x,
            };
            Some(Inst::Const(*dst, r))
        }
        Inst::FUn { dst, op, a } => {
            let x = k(*a)?.as_f()?;
            match op {
                FUn::Neg => Some(Inst::FConst(*dst, -x)),
                FUn::Not => Some(Inst::Const(*dst, (x == 0.0 && !x.is_nan()) as i64)),
            }
        }
        Inst::Cast {
            dst,
            from,
            to,
            src,
        } => {
            let c = k(*src)?;
            match (from, to) {
                (Kind::F64, Kind::F64) => Some(Inst::FConst(*dst, c.as_f()?)),
                (_, Kind::F64) => Some(Inst::FConst(*dst, c.as_i()? as f64)),
                (Kind::F64, _) => {
                    let v = c.as_f()?;
                    if !v.is_finite() {
                        return None;
                    }
                    Some(Inst::Const(*dst, v as i64))
                }
                _ => Some(Inst::Const(*dst, c.as_i()?)),
            }
        }
        _ => None,
    }
}

fn cmp_apply(cc: Cmp, o: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match cc {
        Cmp::Eq => o == Equal,
        Cmp::Ne => o != Equal,
        Cmp::Lt => o == Less,
        Cmp::Le => o != Greater,
        Cmp::Gt => o == Greater,
        Cmp::Ge => o != Less,
    }
}

// ---------------------------------------------------------------------------
// dead code elimination
// ---------------------------------------------------------------------------

fn dce(f: &mut Function) -> usize {
    let mut removed = 0;
    for _ in 0..8 {
        let mut used: HashSet<VReg> = HashSet::new();
        for b in &f.blocks {
            for i in &b.insts {
                for u in i.uses() {
                    used.insert(u);
                }
            }
            if let Some(t) = &b.term {
                for u in t.uses() {
                    used.insert(u);
                }
            }
        }
        let mut round = 0;
        for bi in 0..f.blocks.len() {
            let mut keep = Vec::with_capacity(f.blocks[bi].insts.len());
            for i in &f.blocks[bi].insts {
                let dead = match i.def() {
                    Some(d) => i.is_pure() && !used.contains(&d),
                    None => false,
                };
                if dead {
                    round += 1;
                } else {
                    keep.push(i.clone());
                }
            }
            f.blocks[bi].insts = keep;
        }
        removed += round;
        if round == 0 {
            break;
        }
    }
    removed
}

// ---------------------------------------------------------------------------
// peephole / strength reduction
// ---------------------------------------------------------------------------

fn peephole(f: &mut Function) -> usize {
    let mut changed = 0;
    changed += drop_self_copies(f);
    changed += strength_reduce(f);
    changed += algebraic(f);
    changed += propagate_single_use_copies(f);
    changed
}

fn drop_self_copies(f: &mut Function) -> usize {
    let mut n = 0;
    for b in &mut f.blocks {
        let before = b.insts.len();
        b.insts.retain(|i| !matches!(i, Inst::Copy(d, s) if d == s));
        n += before - b.insts.len();
    }
    n
}

/// Rewrite the *definition* of a vreg to a new instruction.  Returns false if
/// the vreg is not single-assignment.
fn rewrite_def(f: &mut Function, defs: &[u32], v: VReg, new: Inst) -> bool {
    if defs[v as usize] != 1 {
        return false;
    }
    for b in &mut f.blocks {
        for i in b.insts.iter_mut() {
            if i.def() == Some(v) {
                *i = new;
                return true;
            }
        }
    }
    false
}

fn is_pow2(v: i64) -> Option<u8> {
    if v > 0 && (v as u64).is_power_of_two() {
        Some((v as u64).trailing_zeros() as u8)
    } else {
        None
    }
}

fn strength_reduce(f: &mut Function) -> usize {
    let mut n = 0;
    let defs = def_counts(f);
    let uses = use_counts(f);

    for bi in 0..f.blocks.len() {
        for ii in 0..f.blocks[bi].insts.len() {
            let cur = f.blocks[bi].insts[ii].clone();
            match &cur {
                Inst::Bin {
                    dst,
                    op: IBin::Mul,
                    a,
                    b,
                } => {
                    // x * 2^k -> x shl k, when the constant is a private literal
                    if let Some(k) = const_of(f, defs.as_slice(), uses.as_slice(), *b) {
                        if let Some(s) = is_pow2(k) {
                            let new = Inst::Const(*b, s as i64);
                            if rewrite_def(f, &defs, *b, new) {
                                f.blocks[bi].insts[ii] = Inst::Bin {
                                    dst: *dst,
                                    op: IBin::Shl,
                                    a: *a,
                                    b: *b,
                                };
                                n += 1;
                            }
                        }
                    } else if let Some(k) = const_of(f, defs.as_slice(), uses.as_slice(), *a) {
                        if let Some(s) = is_pow2(k) {
                            let new = Inst::Const(*a, s as i64);
                            if rewrite_def(f, &defs, *a, new) {
                                f.blocks[bi].insts[ii] = Inst::Bin {
                                    dst: *dst,
                                    op: IBin::Shl,
                                    a: *b,
                                    b: *a,
                                };
                                n += 1;
                            }
                        }
                    }
                }
                Inst::IDiv {
                    dst,
                    a,
                    b,
                    unsigned: true,
                    rem,
                } => {
                    if let Some(k) = const_of(f, defs.as_slice(), uses.as_slice(), *b) {
                        if let Some(s) = is_pow2(k) {
                            let (op, cst) = if *rem {
                                (IBin::And, k - 1)
                            } else {
                                (IBin::Shr, s as i64)
                            };
                            if rewrite_def(f, &defs, *b, Inst::Const(*b, cst)) {
                                f.blocks[bi].insts[ii] = Inst::Bin {
                                    dst: *dst,
                                    op,
                                    a: *a,
                                    b: *b,
                                };
                                n += 1;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    n
}

/// Value of a vreg if it is defined by a private literal (single def, single use).
fn const_of(f: &Function, defs: &[u32], uses: &[u32], v: VReg) -> Option<i64> {
    if defs[v as usize] != 1 || uses[v as usize] != 1 {
        return None;
    }
    for b in &f.blocks {
        for i in &b.insts {
            if i.def() == Some(v) {
                if let Inst::Const(_, c) = i {
                    return Some(*c);
                }
                return None;
            }
        }
    }
    None
}

fn algebraic(f: &mut Function) -> usize {
    let mut n = 0;
    for b in &mut f.blocks {
        for i in b.insts.iter_mut() {
            let mut replacement: Option<Inst> = None;
            if let Inst::Bin { dst, op, a, b: bb } = i {
                if *op == IBin::Sub && a == bb {
                    replacement = Some(Inst::Const(*dst, 0));
                }
            }
            if let Some(r) = replacement {
                *i = r;
                n += 1;
            }
        }
    }
    n
}

/// `d = copy s` where `d` is used once and `s` is defined earlier in the same
/// block: rewrite the single use to `s` and drop the copy.
fn propagate_single_use_copies(f: &mut Function) -> usize {
    let mut n = 0;
    let defs = def_counts(f);
    let uses = use_counts(f);
    let mut candidates: Vec<(VReg, VReg)> = Vec::new();

    for b in &f.blocks {
        for (ii, i) in b.insts.iter().enumerate() {
            if let Inst::Copy(d, s) = i {
                if d == s || defs[*d as usize] != 1 || uses[*d as usize] != 1 {
                    continue;
                }
                // A single-assignment source never changes value, so hoisting
                // the read to the definition point is safe.
                if defs[*s as usize] != 1 {
                    continue;
                }
                // the source must be defined earlier in this same block
                let ok = b.insts[..ii].iter().rev().any(|p| p.def() == Some(*s));
                if ok {
                    candidates.push((*d, *s));
                }
            }
        }
    }
    if candidates.is_empty() {
        return 0;
    }

    for (dst, src) in candidates {
        // drop the copy
        let mut removed = false;
        for b in &mut f.blocks {
            let before = b.insts.len();
            b.insts
                .retain(|i| !matches!(i, Inst::Copy(d, s) if *d == dst && *s == src));
            if b.insts.len() != before {
                removed = true;
            }
        }
        if !removed {
            continue;
        }
        // rewrite the single use
        for b in &mut f.blocks {
            for i in b.insts.iter_mut() {
                replace_use(i, dst, src);
            }
            if let Some(t) = b.term.as_mut() {
                replace_use_term(t, dst, src);
            }
        }
        n += 1;
    }
    n
}

fn replace_use(i: &mut Inst, from: VReg, to: VReg) {
    let fix = |v: &mut VReg| {
        if *v == from {
            *v = to;
        }
    };
    match i {
        Inst::Copy(_, s) => fix(s),
        Inst::Load { base, .. } => fix(base),
        Inst::Store { base, src, .. } => {
            fix(base);
            fix(src);
        }
        Inst::MemCopy { dst, src, .. } => {
            fix(dst);
            fix(src);
        }
        Inst::Bin { a, b, .. }
        | Inst::IDiv { a, b, .. }
        | Inst::FBin { a, b, .. }
        | Inst::ICmp { a, b, .. }
        | Inst::FCmp { a, b, .. } => {
            fix(a);
            fix(b);
        }
        Inst::Un { a, .. } | Inst::FUn { a, .. } => fix(a),
        Inst::Cast { src, .. } => fix(src),
        Inst::Lea { base, index, .. } => {
            fix(base);
            fix(index);
        }
        Inst::LeaBase { base, .. } => fix(base),
        Inst::Zero { dst, .. } => fix(dst),
        Inst::Call { args, .. } => {
            for a in args.iter_mut() {
                fix(a);
            }
        }
        Inst::RtCall { gpr, xmm, .. } => {
            for g in gpr.iter_mut() {
                fix(g);
            }
            if let Some(x) = xmm {
                fix(x);
            }
        }
        _ => {}
    }
}

fn replace_use_term(t: &mut Term, from: VReg, to: VReg) {
    match t {
        Term::Branch { cond, .. } => {
            if *cond == from {
                *cond = to;
            }
        }
        Term::Ret(Some(v)) => {
            if *v == from {
                *v = to;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// debug helper: histogram of instruction kinds
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub fn histogram(f: &Function) -> HashMap<&'static str, usize> {
    let mut m = HashMap::new();
    for b in &f.blocks {
        for i in &b.insts {
            let k = match i {
                Inst::Const(..) => "const",
                Inst::FConst(..) => "fconst",
                Inst::AddrGlobal(..) => "addr.global",
                Inst::AddrRdata(..) => "addr.rdata",
                Inst::AddrLocal(..) => "addr.local",
                Inst::LeaBase { .. } => "lea.base",
                Inst::Zero { .. } => "zero",
                Inst::Copy(..) => "copy",
                Inst::Load { .. } => "load",
                Inst::Store { .. } => "store",
                Inst::MemCopy { .. } => "memcpy",
                Inst::Bin { .. } => "bin",
                Inst::IDiv { .. } => "idiv",
                Inst::FBin { .. } => "fbin",
                Inst::ICmp { .. } => "icmp",
                Inst::FCmp { .. } => "fcmp",
                Inst::Un { .. } => "un",
                Inst::FUn { .. } => "fun",
                Inst::Lea { .. } => "lea",
                Inst::Cast { .. } => "cast",
                Inst::Call { .. } => "call",
                Inst::RtCall { .. } => "rtcall",
            };
            *m.entry(k).or_insert(0) += 1;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_constants() {
        let mut b = Builder::new("t", Kind::I64, false);
        let k2 = b.vreg(Kind::I64);
        let k3 = b.vreg(Kind::I64);
        let m = b.vreg(Kind::I64);
        let s = b.vreg(Kind::I64);
        b.push(Inst::Const(k2, 2));
        b.push(Inst::Const(k3, 3));
        b.push(Inst::Bin {
            dst: m,
            op: IBin::Mul,
            a: k2,
            b: k3,
        });
        b.push(Inst::Bin {
            dst: s,
            op: IBin::Add,
            a: m,
            b: k3,
        });
        b.set_term(Term::Ret(Some(s)));
        let mut f = b.finish();
        let st = optimize(&mut f);
        assert!(st.folded >= 2, "{:?}", st);
        assert!(
            matches!(f.blocks[0].insts[0], Inst::Const(_, 9)),
            "{:?}",
            f.blocks[0].insts
        );
    }

    #[test]
    fn strength_reduction_mul_pow2() {
        let mut b = Builder::new("t", Kind::I64, false);
        let x = b.vreg(Kind::I64);
        let k = b.vreg(Kind::I64);
        let r = b.vreg(Kind::I64);
        b.push(Inst::AddrLocal(x, -8));
        b.push(Inst::Const(k, 8));
        b.push(Inst::Bin {
            dst: r,
            op: IBin::Mul,
            a: x,
            b: k,
        });
        b.set_term(Term::Ret(Some(r)));
        let mut f = b.finish();
        optimize(&mut f);
        assert!(f.blocks[0]
            .insts
            .iter()
            .any(|i| matches!(i, Inst::Bin { op: IBin::Shl, .. })));
    }

    #[test]
    fn dce_drops_unused_pure() {
        let mut b = Builder::new("t", Kind::I64, false);
        let x = b.vreg(Kind::I64);
        let y = b.vreg(Kind::I64);
        b.push(Inst::Const(x, 5));
        b.push(Inst::Bin {
            dst: y,
            op: IBin::Add,
            a: x,
            b: x,
        });
        b.set_term(Term::Ret(None));
        let mut f = b.finish();
        optimize(&mut f);
        assert!(f.blocks[0].insts.is_empty());
    }
}
