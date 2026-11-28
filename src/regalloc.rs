#![allow(dead_code)]
//! Linear-scan register allocation.
//!
//! Virtual registers get a linear live interval `[lo, hi]` over the
//! instruction layout (`ir::Function::position_of`).  Intervals are computed at
//! instruction granularity inside a block, but rounded out to `[min, max]` over
//! the whole function, which makes back edges conservative and therefore safe
//! without a full loop analysis.
//!
//! Register pools
//! --------------
//! * **Callee-saved** (`RBX`, `RSI`, `RDI`, `R12`..`R15`) вЂ” preferred for values
//!   that are live across a call or across an instruction with a fixed-register
//!   sequence.
//! * **Caller-saved** (`RCX`, `RDX`, `R8`, `R9`, `R11`) вЂ” preferred for short
//!   temporaries.
//! * **Reserved** вЂ” `RAX` and `R10` are never allocated: they are the emitter's
//!   scratch registers.  `XMM14`/`XMM15` are reserved the same way.
//!
//! When a value cannot get a register it is *spilled*: a frame slot is
//! reserved, every use becomes `tmp = load [rbp-slot]` and every definition is
//! followed by `store [rbp-slot], tmp`.  Allocation then re-runs; it converges
//! because spilled vregs leave the pool permanently and the reload temporaries
//! live for a single instruction.

use std::collections::{BTreeMap, HashSet};

use crate::ir::*;

pub const SCRATCH_GP: u8 = 0; // RAX
pub const SCRATCH_GP2: u8 = 10; // R10
pub const SCRATCH_XMM: u8 = 14;
pub const SCRATCH_XMM2: u8 = 15;

/// Allocatable GP registers: `ALLOC_GP` minus the two reserved scratch regs.
pub const REG_GP: u16 = gp::CALLEE_SAVED_GP | (gp::CALLER_SAVED_GP & !(gp::AX | gp::R10));
/// Allocatable XMM registers (all XMM are volatile on Windows x64).
pub const REG_XMM: u16 = 0b0011_1111_1111_1111; // XMM0..XMM13

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Loc {
    /// Never used вЂ” writes may be skipped.
    Free,
    Gp(u8),
    Xmm(u8),
    /// Frame slot (`disp` is relative to RBP, i.e. negative).
    Slot(i32),
}

pub struct Alloc {
    pub locs: Vec<Loc>,
    pub frame: usize,
    pub saves: Vec<(u8, i32)>,
    pub spilled: Vec<VReg>,
    pub rounds: usize,
}

impl Alloc {
    pub fn loc(&self, v: VReg) -> Loc {
        self.locs[v as usize]
    }
}

struct Interval {
    v: VReg,
    lo: u32,
    hi: u32,
    kind: Kind,
    /// GP registers the value must avoid because its interval covers an
    /// instruction that clobbers them.
    forbid_gp: u16,
    /// Set when the interval covers a call: every XMM is volatile, so floats
    /// crossing a call must be spilled.
    forbid_xmm: bool,
    crosses_call: bool,
}

pub fn allocate(f: &mut Function) -> Result<Alloc, String> {
    // spill slots are permanent: once a vreg is spilled it keeps its slot
    let mut slots: BTreeMap<VReg, i32> = BTreeMap::new();
    let mut frame = f.frame;
    let mut last_spilled: Vec<VReg> = Vec::new();

    for round in 0..10 {
        let interrupts = analyse(f, &slots);
        let (locs, spilled, used_gp) = scan(f, &interrupts);
        last_spilled = spilled.clone();
        if std::env::var("BS_RA_DEBUG").is_ok() {
            eprintln!(
                "round {}: {} intervals, {} spilled, frame {}",
                round,
                interrupts.len(),
                spilled.len(),
                frame
            );
            if round == 0 {
                let pos = f.position_of();
                let (comp, st, en) = loops(f, &pos);
                let lo_ = liveness(f);
                for (b, outs) in lo_.iter().enumerate() {
                    eprintln!(
                        "  block {} pos {:?}..{:?} comp {} [{},{}] liveout {:?}",
                        b,
                        pos[b].first(),
                        pos[b].last(),
                        comp[b],
                        st[comp[b]],
                        en[comp[b]],
                        outs
                    );
                }
                for iv in interrupts.iter() {
                    eprintln!("  v{} [{},{}] {:?}", iv.v, iv.lo, iv.hi, iv.kind);
                }
            }
            for v in spilled.iter().take(6) {
                if let Some(iv) = interrupts.iter().find(|i| i.v == *v) {
                    eprintln!("  spill v{} [{},{}] {:?}", iv.v, iv.lo, iv.hi, iv.kind);
                }
            }
        }

        if spilled.is_empty() {
            // home the callee-saved registers that were actually used
            let mut saves = Vec::new();
            let used_callee = used_gp & gp::CALLEE_SAVED_GP;
            for r in 0..16u8 {
                if used_callee & (1 << r) != 0 {
                    frame += 8;
                    saves.push((r, -(frame as i32)));
                }
            }
            f.frame = frame;
            f.saves = saves.clone();
            return Ok(Alloc {
                locs,
                frame,
                saves,
                spilled: Vec::new(),
                rounds: round,
            });
        }

        // reserve a slot for every newly spilled vreg and rewrite the body
        for v in &spilled {
            if !slots.contains_key(v) {
                frame += 8;
                slots.insert(*v, -(frame as i32));
            }
        }
        insert_spill_code(f, &slots, &spilled);
    }

    Err(format!(
        "register allocation failed to converge (last spill set: {:?})",
        last_spilled
    ))
}

// ---------------------------------------------------------------------------
// intervals
// ---------------------------------------------------------------------------

fn analyse(f: &Function, slots: &BTreeMap<VReg, i32>) -> Vec<Interval> {
    let pos = f.position_of();
    let nv = f.kinds.len();
    let mut lo = vec![u32::MAX; nv];
    let mut hi = vec![0u32; nv];
    let mut touch = vec![false; nv];

    // clobber map: position -> gp mask / xmm flag / is-call
    let total = pos.iter().map(|b| b.len()).sum::<usize>();
    let mut clob = vec![0u16; total];
    let mut clobx = vec![false; total];
    let mut iscall = vec![false; total];

    let mut mark = |v: VReg, p: u32| {
        if (v as usize) < nv {
            touch[v as usize] = true;
            if p < lo[v as usize] {
                lo[v as usize] = p;
            }
            if p > hi[v as usize] {
                hi[v as usize] = p;
            }
        }
    };

    for (bi, b) in f.blocks.iter().enumerate() {
        for (ii, inst) in b.insts.iter().enumerate() {
            let p = pos[bi][ii];
            clob[p as usize] |= inst.clobbers();
            clobx[p as usize] |= inst.clobbers_xmm();
            iscall[p as usize] = matches!(inst, Inst::Call { .. } | Inst::RtCall { .. });
            if let Some(d) = inst.def() {
                mark(d, p);
            }
            for u in inst.uses() {
                mark(u, p);
            }
        }
        if let Some(t) = &b.term {
            let p = pos[bi][b.insts.len()];
            for u in t.uses() {
                mark(u, p);
            }
        }
    }

    // A value that is live across a back edge stays alive for the whole loop:
    // it is defined inside the body, used at the top of the next iteration,
    // and the layout may put the definition *after* the use.  Extend those
    // intervals over the entire strongly connected component so the linear
    // scan never reuses their register for a value that sits between the loop
    // header and the textual definition.
    let (comp, start_of_comp, end_of_comp) = loops(f, &pos);
    let live_out = liveness(f);
    for (bi, outs) in live_out.iter().enumerate() {
        let mut is_back = false;
        if let Some(t) = &f.blocks[bi].term {
            for s in t.successors() {
                let s = s as usize;
                if s <= bi && comp[s] == comp[bi] {
                    is_back = true;
                }
            }
        }
        if !is_back {
            continue;
        }
        let c = comp[bi];
        let (start, end) = (start_of_comp[c], end_of_comp[c]);
        for v in outs {
            let vi = *v as usize;
            if vi >= nv || !touch[vi] {
                continue;
            }
            if start < lo[vi] {
                lo[vi] = start;
            }
            if end > hi[vi] {
                hi[vi] = end;
            }
        }
    }

    let mut out = Vec::new();
    for v in FIRST_VREG..nv as VReg {
        if !touch[v as usize] {
            continue;
        }
        if slots.contains_key(&v) {
            // already spilled: no register needed
            continue;
        }
        let (l, h) = (lo[v as usize], hi[v as usize]);
        let mut forbid_gp = 0u16;
        let mut forbid_xmm = false;
        let mut crosses_call = false;
        for p in (l + 1)..h {
            let i = p as usize;
            if i >= total {
                break;
            }
            forbid_gp |= clob[i];
            if clobx[i] {
                forbid_xmm = true;
            }
            if iscall[i] {
                crosses_call = true;
            }
        }
        let kind = match f.kinds[v as usize] {
            // aggregates are addressed through their address, which is a
            // plain pointer value
            Kind::Agg(_) => Kind::Ptr,
            k => k,
        };
        out.push(Interval {
            v,
            lo: l,
            hi: h,
            kind,
            forbid_gp,
            forbid_xmm,
            crosses_call,
        });
    }
    out.sort_by_key(|i| (i.lo, i.v));
    out
}

// ---------------------------------------------------------------------------
// control-flow helpers
// ---------------------------------------------------------------------------

/// Strongly connected components of the block graph (Tarjan, iterative) plus
/// the first and last linear position inside each component.
fn loops(f: &Function, pos: &[Vec<u32>]) -> (Vec<usize>, Vec<u32>, Vec<u32>) {
    let n = f.blocks.len();
    let succ: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            f.blocks[i]
                .term
                .as_ref()
                .map(|t| t.successors().into_iter().map(|b| b as usize).collect())
                .unwrap_or_default()
        })
        .collect();

    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on = vec![false; n];
    let mut comp = vec![usize::MAX; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut ncomp = 0usize;

    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        index[root] = next_index;
        low[root] = next_index;
        next_index += 1;
        stack.push(root);
        on[root] = true;
        let mut call: Vec<(usize, usize)> = vec![(root, 0)];

        while let Some((v, si)) = call.pop() {
            if si < succ[v].len() {
                let w = succ[v][si];
                call.push((v, si + 1));
                if index[w] == usize::MAX {
                    index[w] = next_index;
                    low[w] = next_index;
                    next_index += 1;
                    stack.push(w);
                    on[w] = true;
                    call.push((w, 0));
                } else if on[w] && index[w] < low[v] {
                    low[v] = index[w];
                }
            } else {
                if low[v] == index[v] {
                    loop {
                        let w = stack.pop().unwrap();
                        on[w] = false;
                        comp[w] = ncomp;
                        if w == v {
                            break;
                        }
                    }
                    ncomp += 1;
                }
                if let Some(&(p, _)) = call.last() {
                    if low[v] < low[p] {
                        low[p] = low[v];
                    }
                }
            }
        }
    }

    let mut start = vec![u32::MAX; ncomp];
    let mut end = vec![0u32; ncomp];
    for b in 0..n {
        let c = comp[b];
        if let Some(first) = pos[b].first() {
            if *first < start[c] {
                start[c] = *first;
            }
        }
        if let Some(last) = pos[b].last() {
            if *last > end[c] {
                end[c] = *last;
            }
        }
    }
    for s in start.iter_mut() {
        if *s == u32::MAX {
            *s = 0;
        }
    }
    (comp, start, end)
}

/// Backward liveness over the block graph; returns, per block, the values live
/// on exit.  Sets are tiny (a handful of promoted locals), so plain vectors
/// and a fixpoint loop are plenty.
fn liveness(f: &Function) -> Vec<Vec<VReg>> {
    let n = f.blocks.len();
    let mut uses: Vec<Vec<VReg>> = vec![Vec::new(); n];
    let mut defs: Vec<Vec<VReg>> = vec![Vec::new(); n];
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, b) in f.blocks.iter().enumerate() {
        // upward-exposed uses: only a use that precedes every definition of
        // the same value inside the block makes it live on entry
        let mut defined: Vec<VReg> = Vec::new();
        for inst in &b.insts {
            for u in inst.uses() {
                if !defined.contains(&u) && !uses[i].contains(&u) {
                    uses[i].push(u);
                }
            }
            if let Some(d) = inst.def() {
                if !defs[i].contains(&d) {
                    defs[i].push(d);
                }
                if !defined.contains(&d) {
                    defined.push(d);
                }
            }
        }
        if let Some(t) = &b.term {
            for u in t.uses() {
                if !defined.contains(&u) && !uses[i].contains(&u) {
                    uses[i].push(u);
                }
            }
            succ[i] = t.successors().into_iter().map(|b| b as usize).collect();
        }
    }

    let mut live_in: Vec<Vec<VReg>> = vec![Vec::new(); n];
    let mut live_out: Vec<Vec<VReg>> = vec![Vec::new(); n];
    let mut changed = true;
    while changed {
        changed = false;
        for i in (0..n).rev() {
            let mut out: Vec<VReg> = Vec::new();
            for &s in &succ[i] {
                for &v in &live_in[s] {
                    if !out.contains(&v) {
                        out.push(v);
                    }
                }
            }
            let mut inp: Vec<VReg> = uses[i].clone();
            for &v in &out {
                if !defs[i].contains(&v) && !inp.contains(&v) {
                    inp.push(v);
                }
            }
            if out != live_out[i] || inp != live_in[i] {
                live_out[i] = out;
                live_in[i] = inp;
                changed = true;
            }
        }
    }
    live_out
}

// ---------------------------------------------------------------------------
// linear scan
// ---------------------------------------------------------------------------

fn pick_gp(used: u16, forbid: u16, prefer_callee: bool) -> Option<u8> {
    let avail = REG_GP & !used & !forbid;
    if avail == 0 {
        return None;
    }
    let order: [u16; 2] = if prefer_callee {
        [gp::CALLEE_SAVED_GP, gp::CALLER_SAVED_GP]
    } else {
        [gp::CALLER_SAVED_GP, gp::CALLEE_SAVED_GP]
    };
    for m in order {
        let cand = avail & m;
        if cand != 0 {
            return Some(cand.trailing_zeros() as u8);
        }
    }
    Some(avail.trailing_zeros() as u8)
}

fn pick_xmm(used: u16, forbid: bool) -> Option<u8> {
    if forbid {
        return None;
    }
    let avail = REG_XMM & !used;
    if avail == 0 {
        None
    } else {
        Some(avail.trailing_zeros() as u8)
    }
}

fn scan(f: &Function, ivs: &[Interval]) -> (Vec<Loc>, Vec<VReg>, u16) {
    let mut locs = vec![Loc::Free; f.kinds.len()];
    let mut spilled = Vec::new();
    let mut used_gp = 0u16;
    let mut used_xmm = 0u16;
    // active: (hi, vreg, loc)
    let mut active: Vec<(u32, VReg, Loc)> = Vec::new();

    for iv in ivs {
        // expire
        active.retain(|(h, _, loc)| {
            if *h < iv.lo {
                match loc {
                    Loc::Gp(r) => used_gp &= !(1 << r),
                    Loc::Xmm(x) => used_xmm &= !(1 << x),
                    _ => {}
                }
                false
            } else {
                true
            }
        });

        if iv.kind.is_agg() {
            // aggregates are always accessed through their address, which is
            // an ordinary pointer value
            spilled.push(iv.v);
            continue;
        }

        if iv.kind.is_float() {
            if let Some(x) = pick_xmm(used_xmm, iv.forbid_xmm) {
                used_xmm |= 1 << x;
                locs[iv.v as usize] = Loc::Xmm(x);
                active.push((iv.hi, iv.v, Loc::Xmm(x)));
            } else {
                spilled.push(iv.v);
            }
            continue;
        }

        match pick_gp(used_gp, iv.forbid_gp, iv.crosses_call) {
            Some(r) => {
                used_gp |= 1 << r;
                locs[iv.v as usize] = Loc::Gp(r);
                active.push((iv.hi, iv.v, Loc::Gp(r)));
            }
            None => {
                // spill the active interval with the farthest end, if it is
                // cheaper than spilling the current one
                let victim = active
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, (h, _, _))| *h)
                    .map(|(i, (h, _, _))| (i, *h));
                match victim {
                    Some((i, vh)) if vh > iv.hi => {
                        let (_, v, loc) = active.remove(i);
                        if let Loc::Gp(r) = loc {
                            used_gp &= !(1 << r);
                        }
                        spilled.push(v);
                        if let Some(r) = pick_gp(used_gp, iv.forbid_gp, iv.crosses_call) {
                            used_gp |= 1 << r;
                            locs[iv.v as usize] = Loc::Gp(r);
                            active.push((iv.hi, iv.v, Loc::Gp(r)));
                        } else {
                            spilled.push(iv.v);
                        }
                    }
                    _ => spilled.push(iv.v),
                }
            }
        }
    }

    (locs, spilled, used_gp)
}

// ---------------------------------------------------------------------------
// spill rewriting
// ---------------------------------------------------------------------------

fn insert_spill_code(f: &mut Function, slots: &BTreeMap<VReg, i32>, spilled: &[VReg]) {
    let spilled: HashSet<VReg> = spilled.iter().copied().collect();
    let mut fresh: Vec<(VReg, Kind)> = Vec::new();
    let mut next = f.kinds.len() as VReg;

    for bi in 0..f.blocks.len() {
        let old = std::mem::take(&mut f.blocks[bi].insts);
        let mut out: Vec<Inst> = Vec::with_capacity(old.len() + 4);
        for mut inst in old {
            // reload spilled operands
            let uses = inst.uses();
            let mut rename: BTreeMap<VReg, VReg> = BTreeMap::new();
            for u in uses {
                if spilled.contains(&u) {
                    if let Some(disp) = slots.get(&u) {
                        let k = f.kinds[u as usize];
                        let t = next;
                        next += 1;
                        fresh.push((t, k));
                        out.push(Inst::Load {
                            dst: t,
                            base: FRAME,
                            disp: *disp,
                            kind: k,
                        });
                        rename.insert(u, t);
                    }
                }
            }
            if !rename.is_empty() {
                rename_uses(&mut inst, &rename);
            }
            // write back a spilled definition
            if let Some(d) = inst.def() {
                if spilled.contains(&d) {
                    if let Some(disp) = slots.get(&d) {
                        let k = f.kinds[d as usize];
                        let t = next;
                        next += 1;
                        fresh.push((t, k));
                        rename_dst(&mut inst, t);
                        out.push(inst);
                        out.push(Inst::Store {
                            base: FRAME,
                            disp: *disp,
                            src: t,
                            kind: k,
                        });
                        continue;
                    }
                }
            }
            out.push(inst);
        }
        f.blocks[bi].insts = out;

        // terminators may also read spilled values
        if let Some(t) = f.blocks[bi].term.clone() {
            let mut rename: BTreeMap<VReg, VReg> = BTreeMap::new();
            for u in t.uses() {
                if spilled.contains(&u) {
                    if let Some(disp) = slots.get(&u) {
                        let k = f.kinds[u as usize];
                        let tmp = next;
                        next += 1;
                        fresh.push((tmp, k));
                        f.blocks[bi].insts.push(Inst::Load {
                            dst: tmp,
                            base: FRAME,
                            disp: *disp,
                            kind: k,
                        });
                        rename.insert(u, tmp);
                    }
                }
            }
            if !rename.is_empty() {
                let mut t2 = t;
                rename_term(&mut t2, &rename);
                f.blocks[bi].term = Some(t2);
            }
        }
    }

    for (v, k) in fresh {
        f.kinds.push(k);
        debug_assert_eq!(f.kinds.len() as VReg, v + 1);
    }
}

fn rename_uses(i: &mut Inst, map: &BTreeMap<VReg, VReg>) {
    let r = |v: &mut VReg| {
        if let Some(n) = map.get(v) {
            *v = *n;
        }
    };
    match i {
        Inst::Copy(_, s) => r(s),
        Inst::Load { base, .. } => r(base),
        Inst::Store { base, src, .. } => {
            r(base);
            r(src);
        }
        Inst::MemCopy { dst, src, .. } => {
            r(dst);
            r(src);
        }
        Inst::Bin { a, b, .. }
        | Inst::IDiv { a, b, .. }
        | Inst::FBin { a, b, .. }
        | Inst::ICmp { a, b, .. }
        | Inst::FCmp { a, b, .. } => {
            r(a);
            r(b);
        }
        Inst::Un { a, .. } | Inst::FUn { a, .. } => r(a),
        Inst::Cast { src, .. } => r(src),
        Inst::Lea { base, index, .. } => {
            r(base);
            r(index);
        }
        Inst::LeaBase { base, .. } => r(base),
        Inst::Zero { dst, .. } => r(dst),
        Inst::Call { args, .. } => {
            for a in args.iter_mut() {
                r(a);
            }
        }
        Inst::RtCall { gpr, xmm, .. } => {
            for g in gpr.iter_mut() {
                r(g);
            }
            if let Some(x) = xmm {
                r(x);
            }
        }
        _ => {}
    }
}

fn rename_dst(i: &mut Inst, to: VReg) {
    match i {
        Inst::Const(d, _)
        | Inst::FConst(d, _)
        | Inst::AddrGlobal(d, _)
        | Inst::AddrRdata(d, _)
        | Inst::AddrLocal(d, _)
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
        | Inst::LeaBase { dst: d, .. }
        | Inst::Cast { dst: d, .. } => *d = to,
        Inst::Call { dst, .. } | Inst::RtCall { dst, .. } => *dst = Some(to),
        _ => {}
    }
}

fn rename_term(t: &mut Term, map: &BTreeMap<VReg, VReg>) {
    match t {
        Term::Branch { cond, .. } => {
            if let Some(n) = map.get(cond) {
                *cond = *n;
            }
        }
        Term::Ret(Some(v)) => {
            if let Some(n) = map.get(v) {
                *v = *n;
            }
        }
        _ => {}
    }
}
