//! IR -> x86-64 emission.
//!
//! The emitter consumes a register-allocated [`Function`]: every virtual
//! register already has a home ([`Loc`]), spill reloads and write-backs are
//! explicit `Load`/`Store` instructions, and the frame size plus the list of
//! callee-saved registers to home are fixed.  All that is left is mechanical
//! instruction selection.
//!
//! Conventions
//! -----------
//! * `RAX` and `R10` are scratch GP registers, `XMM14`/`XMM15` scratch SSE
//!   registers.  The allocator never hands them out, so they are free at every
//!   instruction boundary.
//! * `RSP` stays 16-byte aligned everywhere except inside the call-staging
//!   window, which is padded so the `call` itself sees an aligned stack.
//! * `FRAME`/`DATA`/`RDATA` become `[rbp+disp]` and `[rip+disp]` operands and
//!   never materialise in a register.

use std::collections::HashMap;

use crate::ir::*;
use crate::regalloc::{Alloc, Loc};
use crate::x64::*;

/// Crafted scratch registers (never allocated).
const SGP1: u8 = RAX;
const SGP2: u8 = R10;
const SX1: u8 = 15;
const SX2: u8 = 14;

struct Emitter<'a> {
    asm: &'a mut Asm,
    f: &'a Function,
    a: &'a Alloc,
    fns: &'a HashMap<String, u32>,
    rt: &'a HashMap<String, u32>,
    rdata: &'a mut Vec<u8>,
    fconsts: &'a mut HashMap<u64, u32>,
    labels: Vec<u32>,
}

pub fn emit_fn(
    asm: &mut Asm,
    f: &Function,
    a: &Alloc,
    label: u32,
    fns: &HashMap<String, u32>,
    rt: &HashMap<String, u32>,
    rdata: &mut Vec<u8>,
    fconsts: &mut HashMap<u64, u32>,
) -> Result<(), String> {
    let labels: Vec<u32> = (0..f.blocks.len()).map(|_| asm.label()).collect();
    let mut e = Emitter {
        asm,
        f,
        a,
        fns,
        rt,
        rdata,
        fconsts,
        labels,
    };
    e.prologue(label)?;
    e.body()?;
    Ok(())
}

impl<'a> Emitter<'a> {
    // -----------------------------------------------------------------------
    // locations
    // -----------------------------------------------------------------------

    fn loc(&self, v: VReg) -> Loc {
        self.a.locs[v as usize]
    }

    fn kind(&self, v: VReg) -> Kind {
        self.f.kinds[v as usize]
    }

    /// Physically read a GP value.  A spilled value is loaded into `scratch`.
    fn gp_src(&mut self, v: VReg, scratch: u8) -> u8 {
        match self.loc(v) {
            Loc::Gp(r) => r,
            Loc::Slot(d) => {
                let k = self.kind(v);
                self.load_mem_reg(scratch, RBP, d, k);
                scratch
            }
            _ => scratch,
        }
    }

    /// Destination register for a definition (`RAX` when spilled).
    fn dst_gp(&self, v: VReg) -> u8 {
        match self.loc(v) {
            Loc::Gp(r) => r,
            _ => SGP1,
        }
    }

    /// Write a computed GP value into the definition's home.
    fn put_gp(&mut self, v: VReg, r: u8) {
        if let Loc::Slot(d) = self.loc(v) {
            let k = self.kind(v);
            self.store_kind_reg(RBP, d, r, k);
        }
    }

    fn xmm_src(&mut self, v: VReg, scratch: u8) -> u8 {
        match self.loc(v) {
            Loc::Xmm(x) => x,
            Loc::Slot(d) => {
                self.asm.movsd_xmm_mem(scratch, RBP, d);
                scratch
            }
            _ => scratch,
        }
    }

    fn dst_xmm(&self, v: VReg) -> u8 {
        match self.loc(v) {
            Loc::Xmm(x) => x,
            _ => SX1,
        }
    }

    fn put_xmm(&mut self, v: VReg, x: u8) {
        if let Loc::Slot(d) = self.loc(v) {
            self.asm.movsd_mem_xmm(RBP, d, x);
        }
    }

    // -----------------------------------------------------------------------
    // typed memory access
    // -----------------------------------------------------------------------

    fn store_kind_reg(&mut self, base: u8, disp: i32, reg: u8, k: Kind) {
        match k.mem_width() {
            1 => self.asm.store8(base, disp, reg),
            2 => self.asm.store16(base, disp, reg),
            4 => self.asm.store32(base, disp, reg),
            _ => self.asm.store(base, disp, reg),
        }
    }

    fn load_mem_reg(&mut self, dst: u8, base: u8, disp: i32, k: Kind) {
        match k.mem_width() {
            1 => self.asm.load8zx(dst, base, disp),
            2 => self.asm.load16zx(dst, base, disp),
            4 => self.asm.load32(dst, base, disp),
            _ => self.asm.load(dst, base, disp),
        }
    }

    fn load_rip_reg(&mut self, dst: u8, fix: Fixup, k: Kind) {
        match k.mem_width() {
            1 => self.asm.load8zx_rip(dst, fix),
            2 => {
                let t = if dst == SGP2 { SGP1 } else { SGP2 };
                self.asm.lea_rip(t, fix);
                self.asm.load16zx(dst, t, 0);
            }
            4 => {
                let t = if dst == SGP2 { SGP1 } else { SGP2 };
                self.asm.lea_rip(t, fix);
                self.asm.load32(dst, t, 0);
            }
            _ => self.asm.load_rip(dst, fix),
        }
    }

    fn store_rip_reg(&mut self, fix: Fixup, src: u8, k: Kind) {
        match k.mem_width() {
            1 => self.asm.store_rip8(src, fix),
            2 | 4 => {
                let t = if src == SGP2 { SGP1 } else { SGP2 };
                self.asm.lea_rip(t, fix);
                if k.mem_width() == 2 {
                    self.asm.store16(t, 0, src);
                } else {
                    self.asm.store32(t, 0, src);
                }
            }
            _ => self.asm.store_rip(src, fix),
        }
    }

    fn load_operand(&mut self, dst: u8, base: VReg, disp: i32, k: Kind) {
        if k.is_float() {
            match base {
                FRAME => self.asm.movsd_xmm_mem(dst, RBP, disp),
                DATA => self.asm.movsd_xmm_rip(dst, Fixup::Data(disp as u32)),
                RDATA => self.asm.movsd_xmm_rip(dst, Fixup::Rdata(disp as u32)),
                _ => {
                    let b = self.gp_src(base, SGP2);
                    self.asm.movsd_xmm_mem(dst, b, disp);
                }
            }
        } else {
            match base {
                FRAME => self.load_mem_reg(dst, RBP, disp, k),
                DATA => self.load_rip_reg(dst, Fixup::Data(disp as u32), k),
                RDATA => self.load_rip_reg(dst, Fixup::Rdata(disp as u32), k),
                _ => {
                    let b = self.gp_src(base, SGP2);
                    self.load_mem_reg(dst, b, disp, k);
                }
            }
        }
    }

    fn store_operand(&mut self, base: VReg, disp: i32, src: VReg, k: Kind) {
        if k.is_float() {
            let x = self.xmm_src(src, SX1);
            match base {
                FRAME => self.asm.movsd_mem_xmm(RBP, disp, x),
                DATA => self.asm.movsd_mem_xmm_rip(Fixup::Data(disp as u32), x),
                RDATA => self.asm.movsd_mem_xmm_rip(Fixup::Rdata(disp as u32), x),
                _ => {
                    let b = self.gp_src(base, SGP2);
                    self.asm.movsd_mem_xmm(b, disp, x);
                }
            }
        } else {
            let r = self.gp_src(src, SGP1);
            match base {
                FRAME => self.store_kind_reg(RBP, disp, r, k),
                DATA => self.store_rip_reg(Fixup::Data(disp as u32), r, k),
                RDATA => self.store_rip_reg(Fixup::Rdata(disp as u32), r, k),
                _ => {
                    let b = self.gp_src(base, SGP2);
                    self.store_kind_reg(b, disp, r, k);
                }
            }
        }
    }

    fn intern_f64(&mut self, v: f64) -> u32 {
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

    // -----------------------------------------------------------------------
    // prologue / epilogue
    // -----------------------------------------------------------------------

    fn prologue(&mut self, label: u32) -> Result<(), String> {
        self.asm.bind(label);
        self.asm.push_r(RBP);
        self.asm.mov_rr(RBP, RSP);
        let frame = ((self.f.frame + 15) / 16 * 16) as u32;
        if frame != 0 {
            self.asm.sub_rsp(frame);
        }

        let homes = self.f.param_homes.clone();
        for h in homes {
            match h {
                Home::Gpr(r, disp, 1) => self.asm.store8(RBP, disp, r),
                Home::Gpr(r, disp, 2) => self.asm.store16(RBP, disp, r),
                Home::Gpr(r, disp, 4) => self.asm.store32(RBP, disp, r),
                Home::Gpr(r, disp, _) => self.asm.store(RBP, disp, r),
                Home::Xmm(x, disp) => self.asm.movsd_mem_xmm(RBP, disp, x),
            }
        }
        let saves = self.f.saves.clone();
        for (r, disp) in saves {
            self.asm.store(RBP, disp, r);
        }
        Ok(())
    }

    fn epilogue(&mut self) {
        let saves = self.f.saves.clone();
        for (r, disp) in saves {
            self.asm.load(r, RBP, disp);
        }
        self.asm.leave();
        self.asm.ret();
    }

    // -----------------------------------------------------------------------
    // body
    // -----------------------------------------------------------------------

    fn body(&mut self) -> Result<(), String> {
        for bi in 0..self.f.blocks.len() {
            let lbl = self.labels[bi];
            self.asm.bind(lbl);
            let insts = self.f.blocks[bi].insts.clone();
            for i in &insts {
                self.inst(i)?;
            }
            if let Some(t) = self.f.blocks[bi].term.clone() {
                self.term(&t)?;
            }
        }
        Ok(())
    }

    fn inst(&mut self, i: &Inst) -> Result<(), String> {
        match i {
            Inst::Const(d, v) => {
                let dr = self.dst_gp(*d);
                self.asm.mov_imm64(dr, *v);
                self.put_gp(*d, dr);
            }
            Inst::FConst(d, v) => {
                let off = self.intern_f64(*v);
                let dx = self.dst_xmm(*d);
                self.asm.movsd_xmm_rip(dx, Fixup::Rdata(off));
                self.put_xmm(*d, dx);
            }
            Inst::AddrGlobal(d, off) => {
                let dr = self.dst_gp(*d);
                self.asm.lea_rip(dr, Fixup::Data(*off));
                self.put_gp(*d, dr);
            }
            Inst::AddrRdata(d, off) => {
                let dr = self.dst_gp(*d);
                self.asm.lea_rip(dr, Fixup::Rdata(*off));
                self.put_gp(*d, dr);
            }
            Inst::AddrLocal(d, disp) => {
                let dr = self.dst_gp(*d);
                self.asm.lea(dr, RBP, *disp);
                self.put_gp(*d, dr);
            }
            Inst::LeaBase { dst, base, disp } => {
                let dr = self.dst_gp(*dst);
                match *base {
                    FRAME => self.asm.lea(dr, RBP, *disp),
                    DATA => {
                        self.asm.lea_rip(dr, Fixup::Data(0));
                        if *disp != 0 {
                            self.asm.add_imm32(dr, *disp);
                        }
                    }
                    RDATA => {
                        self.asm.lea_rip(dr, Fixup::Rdata(0));
                        if *disp != 0 {
                            self.asm.add_imm32(dr, *disp);
                        }
                    }
                    _ => {
                        let b = self.gp_src(*base, SGP2);
                        self.asm.lea(dr, b, *disp);
                    }
                }
                self.put_gp(*dst, dr);
            }
            Inst::Zero { dst, bytes } => {
                if *bytes == 0 {
                    return Ok(());
                }
                let p = self.gp_src(*dst, SGP2);
                if p != SGP1 {
                    self.asm.mov_rr(SGP1, p);
                }
                let words = (*bytes as i64 + 7) / 8;
                self.asm.mov_imm64(RCX, words);
                self.asm.mov_imm32_reg(SGP2, 0);
                let top = self.asm.label();
                self.asm.bind(top);
                self.asm.store(SGP1, 0, SGP2);
                self.asm.add_imm32(SGP1, 8);
                self.asm.dec_r(RCX);
                self.asm.jcc(CC_NE, top);
            }
            Inst::Copy(d, s) => {
                let k = self.kind(*d);
                if k.is_float() {
                    let x = self.xmm_src(*s, SX1);
                    match self.loc(*d) {
                        Loc::Xmm(dx) => {
                            if dx != x {
                                self.asm.movsd_xmm_xmm(dx, x);
                            }
                        }
                        Loc::Slot(sp) => self.asm.movsd_mem_xmm(RBP, sp, x),
                        _ => {}
                    }
                } else {
                    let r = self.gp_src(*s, SGP2);
                    let dr = self.dst_gp(*d);
                    if dr != r {
                        self.asm.mov_rr(dr, r);
                    }
                    self.put_gp(*d, dr);
                }
            }
            Inst::Load {
                dst,
                base,
                disp,
                kind,
            } => {
                if kind.is_float() {
                    let dx = self.dst_xmm(*dst);
                    self.load_operand(dx, *base, *disp, *kind);
                    self.put_xmm(*dst, dx);
                } else {
                    let dr = self.dst_gp(*dst);
                    self.load_operand(dr, *base, *disp, *kind);
                    self.put_gp(*dst, dr);
                }
            }
            Inst::Store {
                base,
                disp,
                src,
                kind,
            } => self.store_operand(*base, *disp, *src, *kind),
            Inst::MemCopy { dst, src, bytes } => {
                if *bytes == 0 {
                    return Ok(());
                }
                let d = self.gp_src(*dst, SGP1);
                if d != SGP1 {
                    self.asm.mov_rr(SGP1, d);
                }
                let s = self.gp_src(*src, SGP2);
                self.asm.mov_rr(RSI, s);
                self.asm.mov_rr(RDI, SGP1);
                self.asm.mov_imm64(RCX, *bytes as i64);
                self.asm.rep_movsb();
            }
            Inst::Bin { dst, op, a, b } => self.bin(*dst, *op, *a, *b),
            Inst::IDiv {
                dst,
                a,
                b,
                unsigned,
                rem,
            } => self.idiv(*dst, *a, *b, *unsigned, *rem),
            Inst::FBin { dst, op, a, b } => self.fbin(*dst, *op, *a, *b),
            Inst::ICmp {
                dst,
                cc,
                a,
                b,
                unsigned,
            } => {
                let ra = self.gp_src(*a, SGP1);
                let rb = self.gp_src(*b, SGP2);
                self.asm.cmp_rr(ra, rb);
                let dr = self.dst_gp(*dst);
                let code = cc_code(*cc, *unsigned);
                self.asm.setcc_r(code, dr);
                self.asm.movzx_rr8(dr, dr);
                self.put_gp(*dst, dr);
            }
            Inst::FCmp { dst, cc, a, b } => {
                let xa = self.xmm_src(*a, SX1);
                let xb = self.xmm_src(*b, SX2);
                self.asm.ucomisd(xa, xb);
                let dr = self.dst_gp(*dst);
                self.asm.setcc_r(fcc_code(*cc), dr);
                self.asm.movzx_rr8(dr, dr);
                self.put_gp(*dst, dr);
            }
            Inst::Un { dst, op, a } => {
                let dr = self.dst_gp(*dst);
                let ra = self.gp_src(*a, SGP2);
                match op {
                    IUn::Neg => {
                        if dr != ra {
                            self.asm.mov_rr(dr, ra);
                        }
                        self.asm.neg_r(dr);
                    }
                    IUn::Not => {
                        self.asm.test_rr(ra, ra);
                        self.asm.setcc_r(CC_E, dr);
                        self.asm.movzx_rr8(dr, dr);
                    }
                    IUn::BitNot => {
                        if dr != ra {
                            self.asm.mov_rr(dr, ra);
                        }
                        self.asm.not_r(dr);
                    }
                }
                self.put_gp(*dst, dr);
            }
            Inst::FUn { dst, op, a } => {
                let x = self.xmm_src(*a, SX1);
                match op {
                    FUn::Neg => {
                        self.asm.movq_r64_xmm(SGP1, x);
                        self.asm.mov_imm64(SGP2, i64::MIN);
                        self.asm.xor_rr(SGP1, SGP2);
                        let dx = self.dst_xmm(*dst);
                        self.asm.movq_xmm_r64(dx, SGP1);
                        self.put_xmm(*dst, dx);
                    }
                    FUn::Not => {
                        self.asm.xorpd(SX2, SX2);
                        self.asm.ucomisd(x, SX2);
                        self.asm.setcc_r(CC_E, SGP2);
                        let ok = self.asm.label();
                        self.asm.jcc(CC_NP, ok);
                        self.asm.mov_imm32_reg(SGP2, 0);
                        self.asm.bind(ok);
                        let dr = self.dst_gp(*dst);
                        self.asm.movzx_rr8(dr, SGP2);
                        self.put_gp(*dst, dr);
                    }
                }
            }
            Inst::Lea {
                dst,
                base,
                index,
                scale,
                disp,
            } => {
                let dr = self.dst_gp(*dst);
                let breg = match *base {
                    FRAME => RBP,
                    DATA | RDATA => {
                        let t = if dr == SGP2 { SGP1 } else { SGP2 };
                        let fix = if *base == DATA {
                            Fixup::Data(0)
                        } else {
                            Fixup::Rdata(0)
                        };
                        self.asm.lea_rip(t, fix);
                        t
                    }
                    _ => self.gp_src(*base, SGP2),
                };
                let ireg = self.gp_src(*index, SGP1);
                self.asm.emit_mem_sib(
                    &[0x8D],
                    true,
                    dr,
                    breg,
                    ireg,
                    1u8 << *scale,
                    *disp,
                );
                self.put_gp(*dst, dr);
            }
            Inst::Cast {
                dst,
                from,
                to,
                src,
            } => {
                let ff = from.is_float();
                let tf = to.is_float();
                if ff && tf {
                    let x = self.xmm_src(*src, SX1);
                    let dx = self.dst_xmm(*dst);
                    if dx != x {
                        self.asm.movsd_xmm_xmm(dx, x);
                    }
                    self.put_xmm(*dst, dx);
                } else if !ff && !tf {
                    let r = self.gp_src(*src, SGP2);
                    let dr = self.dst_gp(*dst);
                    if dr != r {
                        self.asm.mov_rr(dr, r);
                    }
                    self.put_gp(*dst, dr);
                } else if !ff && tf {
                    let r = self.gp_src(*src, SGP2);
                    let dx = self.dst_xmm(*dst);
                    self.asm.cvtsi2sd(dx, r);
                    self.put_xmm(*dst, dx);
                } else {
                    let x = self.xmm_src(*src, SX1);
                    let dr = self.dst_gp(*dst);
                    self.asm.cvttsd2si(dr, x);
                    self.put_gp(*dst, dr);
                }
            }
            Inst::Call {
                dst,
                name,
                args,
                kinds,
                variadic,
                ret,
                ret_slot,
            } => self.call(dst, name, args, kinds, *variadic, *ret, *ret_slot)?,
            Inst::RtCall {
                dst,
                name,
                gpr,
                xmm,
            } => self.rtcall(*dst, name, gpr, *xmm)?,
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // arithmetic
    // -----------------------------------------------------------------------

    fn bin(&mut self, dst: VReg, op: IBin, a: VReg, b: VReg) {
        let dr = self.dst_gp(dst);
        let ra = self.gp_src(a, SGP1);
        let rb = self.gp_src(b, SGP2);

        match op {
            IBin::Shl | IBin::Shr | IBin::Sar => {
                // count must be in CL, value anywhere but CL
                if dr == RCX {
                    if ra != SGP1 {
                        self.asm.mov_rr(SGP1, ra);
                    }
                    self.asm.mov_rr(RCX, rb);
                    self.shift(op, SGP1);
                    self.asm.mov_rr(dr, SGP1);
                } else if dr == rb {
                    self.asm.mov_rr(SGP2, rb);
                    if dr != ra {
                        self.asm.mov_rr(dr, ra);
                    }
                    self.asm.mov_rr(RCX, SGP2);
                    self.shift(op, dr);
                } else {
                    if dr != ra {
                        self.asm.mov_rr(dr, ra);
                    }
                    self.asm.mov_rr(RCX, rb);
                    self.shift(op, dr);
                }
                self.put_gp(dst, dr);
                return;
            }
            _ => {}
        }

        // two-address form with a scratch when the destination aliases the
        // right operand
        if dr == rb && dr != ra {
            // dest == b: keep b alive in the scratch, then fold
            self.asm.mov_rr(SGP2, rb);
            if dr != ra {
                self.asm.mov_rr(dr, ra);
            }
            match op {
                IBin::Add => self.asm.add_rr(dr, SGP2),
                IBin::Sub => self.asm.sub_rr(dr, SGP2),
                IBin::Mul => self.asm.imul_rr(dr, SGP2),
                IBin::And => self.asm.and_rr(dr, SGP2),
                IBin::Or => self.asm.or_rr(dr, SGP2),
                IBin::Xor => self.asm.xor_rr(dr, SGP2),
                _ => unreachable!(),
            }
        } else {
            if dr != ra {
                self.asm.mov_rr(dr, ra);
            }
            match op {
                IBin::Add => self.asm.add_rr(dr, rb),
                IBin::Sub => self.asm.sub_rr(dr, rb),
                IBin::Mul => self.asm.imul_rr(dr, rb),
                IBin::And => self.asm.and_rr(dr, rb),
                IBin::Or => self.asm.or_rr(dr, rb),
                IBin::Xor => self.asm.xor_rr(dr, rb),
                _ => unreachable!(),
            }
        }
        self.put_gp(dst, dr);
    }

    fn shift(&mut self, op: IBin, r: u8) {
        match op {
            IBin::Shl => self.asm.shl_cl(r),
            IBin::Shr => self.asm.shr_cl(r),
            _ => self.asm.sar_cl(r),
        }
    }

    fn idiv(&mut self, dst: VReg, a: VReg, b: VReg, unsigned: bool, rem: bool) {
        let rb = self.gp_src(b, SGP2);
        if rb != SGP2 {
            self.asm.mov_rr(SGP2, rb);
        }
        let ra = self.gp_src(a, SGP1);
        if ra != SGP1 {
            self.asm.mov_rr(SGP1, ra);
        }
        self.asm.mov_rr(RCX, SGP2);
        if unsigned {
            self.asm.xor_rr(RDX, RDX);
            self.asm.div_r(RCX);
        } else {
            self.asm.cqo();
            self.asm.idiv_r(RCX);
        }
        let src = if rem { RDX } else { RAX };
        let dr = self.dst_gp(dst);
        if dr != src {
            self.asm.mov_rr(dr, src);
        }
        self.put_gp(dst, dr);
    }

    fn fbin(&mut self, dst: VReg, op: FBin, a: VReg, b: VReg) {
        let dx = self.dst_xmm(dst);
        let xa = self.xmm_src(a, SX1);
        let xb = self.xmm_src(b, SX2);
        if dx == xb && dx != xa {
            // dest aliases the right operand: compute in a scratch
            self.asm.movsd_xmm_xmm(SX2, xb);
            self.asm.movsd_xmm_xmm(SX1, xa);
            self.fold(op, SX1, SX2);
            self.asm.movsd_xmm_xmm(dx, SX1);
        } else {
            if dx != xa {
                self.asm.movsd_xmm_xmm(dx, xa);
            }
            self.fold(op, dx, xb);
        }
        self.put_xmm(dst, dx);
    }

    fn fold(&mut self, op: FBin, dst: u8, src: u8) {
        match op {
            FBin::Add => self.asm.addsd(dst, src),
            FBin::Sub => self.asm.subsd(dst, src),
            FBin::Mul => self.asm.mulsd(dst, src),
            FBin::Div => self.asm.divsd(dst, src),
        }
    }

    // -----------------------------------------------------------------------
    // calls
    // -----------------------------------------------------------------------

    fn call(
        &mut self,
        dst: &Option<VReg>,
        name: &str,
        args: &[VReg],
        kinds: &[Kind],
        variadic: bool,
        ret: Kind,
        ret_slot: Option<i32>,
    ) -> Result<(), String> {
        let lbl = *self
            .fns
            .get(name)
            .ok_or_else(|| format!("internal: unknown function '{}'", name))?;

        let n = args.len();
        let pos0 = if ret_slot.is_some() { 1 } else { 0 };
        let slots = abi_slots(kinds, pos0);

        // staging layout: one 8-byte slot per argument, overflow aggregates
        // copied into a trailing area
        let mut data_off = vec![0i32; n];
        let mut cursor = (n * 8) as i32;
        for (i, k) in kinds.iter().enumerate() {
            if k.is_agg() && k.size() > 8 {
                data_off[i] = cursor;
                cursor += ((k.size() as i32 + 7) / 8) * 8;
            }
        }
        let staging = cursor as u32;
        let nstack = if variadic {
            (0..n).filter(|i| pos0 + *i >= 4).count()
        } else {
            slots
                .iter()
                .filter(|s| matches!(s, Slot::Stack(_)))
                .count()
        };
        let outgoing = 32 + (nstack as u32) * 8;
        let total = staging + outgoing;
        let pad = (16 - (total % 16)) % 16;

        if pad != 0 {
            self.asm.sub_rsp(pad);
        }
        self.asm.sub_rsp(staging);

        for i in 0..n {
            let at = (i * 8) as i32;
            let k = kinds[i];
            if k.is_float() {
                let x = self.xmm_src(args[i], SX1);
                self.asm.movsd_mem_xmm(RSP, at, x);
            } else if k.is_agg() && k.size() > 8 {
                let src = self.gp_src(args[i], SGP2);
                self.asm.mov_rr(RSI, src);
                self.asm.lea(RDI, RSP, data_off[i]);
                self.asm.mov_imm64(RCX, k.size() as i64);
                self.asm.rep_movsb();
                self.asm.lea(SGP1, RSP, data_off[i]);
                self.asm.store(RSP, at, SGP1);
            } else {
                let r = self.gp_src(args[i], SGP2);
                self.asm.store(RSP, at, r);
            }
        }

        self.asm.sub_rsp(outgoing);
        let base = outgoing as i32;
        let regs = [RCX, RDX, R8, R9];
        let xmms = [XMM0, XMM1, XMM2, XMM3];

        if variadic {
            for i in 0..n {
                let pos = pos0 + i;
                let src = base + (i * 8) as i32;
                if pos < 4 {
                    self.asm.load(regs[pos], RSP, src);
                } else {
                    self.asm.load(SGP1, RSP, src);
                    self.asm.store(RSP, 32 + (pos as i32 - 4) * 8, SGP1);
                }
                if kinds[i].is_float() && i < 4 {
                    self.asm.movsd_xmm_mem(xmms[i], RSP, src);
                }
            }
        } else {
            for (i, slot) in slots.iter().enumerate() {
                if let Slot::Stack(k) = slot {
                    self.asm.load(SGP1, RSP, base + (i * 8) as i32);
                    self.asm.store(RSP, 32 + (*k as i32) * 8, SGP1);
                }
            }
            for (i, slot) in slots.iter().enumerate() {
                match slot {
                    Slot::Gpr(k) => self.asm.load(regs[*k], RSP, base + (i * 8) as i32),
                    Slot::Xmm(k) => {
                        self.asm.movsd_xmm_mem(xmms[*k], RSP, base + (i * 8) as i32)
                    }
                    Slot::Stack(_) => {}
                }
            }
        }
        if let Some(off) = ret_slot {
            self.asm.lea(RCX, RBP, off);
        }

        self.asm.call_rel(lbl);
        let cleanup = outgoing + staging + pad;
        if cleanup != 0 {
            self.asm.add_rsp(cleanup);
        }

        if let Some(d) = dst {
            if ret.is_float() {
                let dx = self.dst_xmm(*d);
                if dx != XMM0 {
                    self.asm.movsd_xmm_xmm(dx, XMM0);
                }
                self.put_xmm(*d, dx);
            } else if !ret.is_agg() {
                let dr = self.dst_gp(*d);
                if dr != RAX {
                    self.asm.mov_rr(dr, RAX);
                }
                self.put_gp(*d, dr);
            } else {
                // aggregate return: RAX already holds the buffer pointer
                let dr = self.dst_gp(*d);
                if dr != RAX {
                    self.asm.mov_rr(dr, RAX);
                }
                self.put_gp(*d, dr);
            }
        }
        Ok(())
    }

    fn rtcall(
        &mut self,
        dst: Option<VReg>,
        name: &str,
        gpr: &[VReg],
        xmm: Option<VReg>,
    ) -> Result<(), String> {
        let lbl = *self
            .rt
            .get(name)
            .ok_or_else(|| format!("internal: unknown runtime '{}'", name))?;
        let targets = [RCX, RDX, R8];
        let mut moves: Vec<(u8, u8)> = Vec::new();
        for (i, v) in gpr.iter().enumerate() {
            let src = self.gp_src(*v, SGP2);
            if i < targets.len() {
                moves.push((targets[i], src));
            }
        }
        self.parallel_moves(&mut moves);
        if let Some(v) = xmm {
            let x = self.xmm_src(v, SX1);
            if x != XMM0 {
                self.asm.movsd_xmm_xmm(XMM0, x);
            }
        }
        self.asm.call_rel(lbl);
        if let Some(d) = dst {
            // the runtime returns an integer in RAX; float helpers are void
            let dr = self.dst_gp(d);
            if dr != RAX {
                self.asm.mov_rr(dr, RAX);
            }
            self.put_gp(d, dr);
        }
        Ok(())
    }

    /// Parallel register move with cycle breaking through `RAX`/`R10`.
    fn parallel_moves(&mut self, moves: &mut Vec<(u8, u8)>) {
        loop {
            // drop no-ops
            moves.retain(|(d, s)| d != s);
            if moves.is_empty() {
                return;
            }
            // a move whose destination is nobody's source can go now
            if let Some(i) = moves
                .iter()
                .position(|(d, _)| !moves.iter().any(|(_, s)| s == d))
            {
                let (d, s) = moves.remove(i);
                self.asm.mov_rr(d, s);
                continue;
            }
            // every remaining move is part of a cycle: break it with a temp
            let (_, s0) = moves[0];
            let tmp = if s0 == SGP2 { SGP1 } else { SGP2 };
            self.asm.mov_rr(tmp, s0);
            for m in moves.iter_mut() {
                if m.1 == s0 {
                    m.1 = tmp;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // terminators
    // -----------------------------------------------------------------------

    fn term(&mut self, t: &Term) -> Result<(), String> {
        match t {
            Term::Goto(b) => {
                let l = self.labels[*b as usize];
                self.asm.jmp(l);
            }
            Term::Branch { cond, t, f } => {
                let lt = self.labels[*t as usize];
                let lf = self.labels[*f as usize];
                let k = self.kind(*cond);
                if k.is_float() {
                    let x = self.xmm_src(*cond, SX1);
                    self.asm.xorpd(SX2, SX2);
                    self.asm.ucomisd(x, SX2);
                    let truthy = self.asm.label();
                    self.asm.jcc(CC_P, truthy); // NaN counts as true
                    self.asm.jcc(CC_E, lf);
                    self.asm.jmp(lt);
                    self.asm.bind(truthy);
                    self.asm.jmp(lt);
                } else {
                    let r = self.gp_src(*cond, SGP2);
                    self.asm.test_rr(r, r);
                    self.asm.jcc(CC_NE, lt);
                    self.asm.jmp(lf);
                }
            }
            Term::Ret(v) => {
                match v {
                    Some(v) => {
                        if self.f.ret.is_agg() {
                            if let Some((slot, bytes)) = self.f.ret_buf {
                                let src = self.gp_src(*v, SGP2);
                                self.asm.mov_rr(RSI, src);
                                self.asm.load(RDI, RBP, slot);
                                self.asm.mov_imm64(RCX, bytes as i64);
                                self.asm.rep_movsb();
                                self.asm.load(RAX, RBP, slot);
                            }
                        } else if self.f.ret.is_float() {
                            let x = self.xmm_src(*v, SX1);
                            if x != XMM0 {
                                self.asm.movsd_xmm_xmm(XMM0, x);
                            }
                        } else {
                            let r = self.gp_src(*v, SGP2);
                            if r != RAX {
                                self.asm.mov_rr(RAX, r);
                            }
                        }
                    }
                    None => self.asm.mov_imm64(RAX, 0),
                }
                self.epilogue();
            }
            Term::Trap => self.asm.ud2(),
        }
        Ok(())
    }
}

fn cc_code(cc: Cmp, unsigned: bool) -> u8 {
    match (cc, unsigned) {
        (Cmp::Eq, _) => CC_E,
        (Cmp::Ne, _) => CC_NE,
        (Cmp::Lt, false) => CC_L,
        (Cmp::Le, false) => CC_LE,
        (Cmp::Gt, false) => CC_G,
        (Cmp::Ge, false) => CC_GE,
        (Cmp::Lt, true) => CC_B,
        (Cmp::Le, true) => CC_BE,
        (Cmp::Gt, true) => CC_A,
        (Cmp::Ge, true) => CC_AE,
    }
}

/// `ucomisd` maps `CF`/`ZF` like an unsigned compare.
fn fcc_code(cc: Cmp) -> u8 {
    match cc {
        Cmp::Eq => CC_E,
        Cmp::Ne => CC_NE,
        Cmp::Lt => CC_B,
        Cmp::Le => CC_BE,
        Cmp::Gt => CC_A,
        Cmp::Ge => CC_AE,
    }
}
