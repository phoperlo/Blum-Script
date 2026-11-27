#![allow(dead_code)] // encoder / type-surface: not every opcode is used by the current backend
//! Minimal x86-64 (Windows ABI) machine-code encoder.
//!
//! No assembler library, no LLVM: every byte this module emits is written by
//! hand.  Labels and relocations are resolved by `crate::pe` at link time.

pub const RAX: u8 = 0;
pub const RCX: u8 = 1;
pub const RDX: u8 = 2;
pub const RBX: u8 = 3;
pub const RSP: u8 = 4;
pub const RBP: u8 = 5;
pub const RSI: u8 = 6;
pub const RDI: u8 = 7;
pub const R8: u8 = 8;
pub const R9: u8 = 9;
pub const R10: u8 = 10;
pub const R11: u8 = 11;
pub const R12: u8 = 12;
pub const R13: u8 = 13;
pub const R14: u8 = 14;
pub const R15: u8 = 15;

// XMM registers are numbered like their GPR counterparts (0..15).
pub const XMM0: u8 = 0;
pub const XMM1: u8 = 1;
pub const XMM2: u8 = 2;
pub const XMM3: u8 = 3;

// condition codes
pub const CC_O: u8 = 0;
pub const CC_NO: u8 = 1;
pub const CC_B: u8 = 2;
pub const CC_AE: u8 = 3;
pub const CC_E: u8 = 4;
pub const CC_NE: u8 = 5;
pub const CC_BE: u8 = 6;
pub const CC_A: u8 = 7;
pub const CC_S: u8 = 8;
pub const CC_NS: u8 = 9;
pub const CC_P: u8 = 10;
pub const CC_NP: u8 = 11;
pub const CC_L: u8 = 12;
pub const CC_GE: u8 = 13;
pub const CC_LE: u8 = 14;
pub const CC_G: u8 = 15;

/// A relocation slot inside `.text`.
#[derive(Clone, Copy, Debug)]
pub enum Fixup {
    /// Rel32 to a label bound in this assembler.
    Local(u32),
    /// Rel32 to `IAT[index]` in the import section.
    Iat(u32),
    /// Rel32 to `.rdata + off`.
    Rdata(u32),
    /// Rel32 to `.data + off`.
    Data(u32),
    /// Rel32 to `.data + off` when `n` immediate bytes follow the
    /// displacement (the instruction does not end at `disp32`).
    DataAfter(u32, u8),
}

pub struct Asm {
    pub code: Vec<u8>,
    pub labels: Vec<Option<u32>>,
    pub fixups: Vec<(u32, Fixup)>,
    next: u32,
}

impl Asm {
    pub fn new() -> Self {
        Asm {
            code: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
            next: 0,
        }
    }

    pub fn len(&self) -> u32 {
        self.code.len() as u32
    }

    pub fn label(&mut self) -> u32 {
        let l = self.next;
        self.next += 1;
        self.labels.push(None);
        l
    }

    pub fn bind(&mut self, l: u32) {
        self.labels[l as usize] = Some(self.code.len() as u32);
    }

    pub fn raw(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    pub fn patch_u32(&mut self, at: u32, v: u32) {
        let a = at as usize;
        self.code[a..a + 4].copy_from_slice(&v.to_le_bytes());
    }

    // ---- encoding primitives ----

    fn rex(&mut self, w: bool, r: u8, b: u8) {
        let mut v = 0x40;
        if w {
            v |= 8;
        }
        if r >= 8 {
            v |= 4;
        }
        if b >= 8 {
            v |= 1;
        }
        if v != 0x40 {
            self.code.push(v);
        }
    }

    /// `op reg, rm` (register-register form, mod = 11).
    fn rr(&mut self, op: &[u8], w: bool, reg: u8, rm: u8) {
        self.rex(w, reg, rm);
        self.code.extend_from_slice(op);
        self.code.push(0xC0 | ((reg & 7) << 3) | (rm & 7));
    }

    /// `op reg, [base + disp32]`.
    fn mem(&mut self, op: &[u8], w: bool, reg: u8, base: u8, disp: i32) {
        self.rex(w, reg, base);
        self.code.extend_from_slice(op);
        let b = base & 7;
        self.code.push(0x80 | ((reg & 7) << 3) | b);
        if b == 4 {
            self.code.push(0x24); // SIB, no index, base = rsp/r12
        }
        self.code.extend_from_slice(&disp.to_le_bytes());
    }

    /// `op reg, [rip + disp32]` with a relocation.
    fn rip(&mut self, op: &[u8], w: bool, reg: u8, fix: Fixup) {
        self.rex(w, reg, 0);
        self.code.extend_from_slice(op);
        self.code.push(((reg & 7) << 3) | 0x05);
        let at = self.code.len() as u32;
        self.code.extend_from_slice(&[0, 0, 0, 0]);
        self.fixups.push((at, fix));
    }

    // ---- stack ----

    pub fn push_r(&mut self, r: u8) {
        if r >= 8 {
            self.code.push(0x41);
        }
        self.code.push(0x50 + (r & 7));
    }

    pub fn pop_r(&mut self, r: u8) {
        if r >= 8 {
            self.code.push(0x41);
        }
        self.code.push(0x58 + (r & 7));
    }

    pub fn sub_rsp(&mut self, v: u32) {
        self.code.extend_from_slice(&[0x48, 0x81, 0xEC]);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    pub fn add_rsp(&mut self, v: u32) {
        self.code.extend_from_slice(&[0x48, 0x81, 0xC4]);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    /// `sub rsp, imm32` with a placeholder that can be patched later.
    pub fn sub_rsp_placeholder(&mut self) -> u32 {
        self.code.extend_from_slice(&[0x48, 0x81, 0xEC, 0, 0, 0, 0]);
        self.code.len() as u32 - 4
    }

    pub fn leave(&mut self) {
        self.code.push(0xC9);
    }

    pub fn ret(&mut self) {
        self.code.push(0xC3);
    }

    // ---- data movement ----

    pub fn mov_rr(&mut self, dst: u8, src: u8) {
        self.rr(&[0x89], true, src, dst);
    }

    pub fn mov_rr8(&mut self, dst: u8, src: u8) {
        self.rr(&[0x88], false, src, dst);
    }

    pub fn movzx_rr8(&mut self, dst: u8, src: u8) {
        self.rr(&[0x0F, 0xB6], true, dst, src);
    }

    pub fn mov_imm64(&mut self, r: u8, v: i64) {
        if v >= i32::MIN as i64 && v <= i32::MAX as i64 {
            self.rex(true, 0, r);
            self.code.push(0xC7);
            self.code.push(0xC0 | (r & 7));
            self.code.extend_from_slice(&(v as i32).to_le_bytes());
        } else {
            self.rex(true, 0, r);
            self.code.push(0xB8 + (r & 7));
            self.code.extend_from_slice(&v.to_le_bytes());
        }
    }

    pub fn load(&mut self, dst: u8, base: u8, disp: i32) {
        self.mem(&[0x8B], true, dst, base, disp);
    }

    pub fn store(&mut self, base: u8, disp: i32, src: u8) {
        self.mem(&[0x89], true, src, base, disp);
    }

    pub fn store8(&mut self, base: u8, disp: i32, src: u8) {
        self.mem(&[0x88], false, src, base, disp);
    }

    pub fn load8zx(&mut self, dst: u8, base: u8, disp: i32) {
        self.mem(&[0x0F, 0xB6], true, dst, base, disp);
    }

    pub fn load16zx(&mut self, dst: u8, base: u8, disp: i32) {
        self.mem(&[0x0F, 0xB7], true, dst, base, disp);
    }

    /// `mov r32, [base+disp]` (zero-extends into the full register)
    pub fn load32(&mut self, dst: u8, base: u8, disp: i32) {
        self.mem(&[0x8B], false, dst, base, disp);
    }

    pub fn store16(&mut self, base: u8, disp: i32, src: u8) {
        self.code.push(0x66);
        self.mem(&[0x89], false, src, base, disp);
    }

    pub fn store32(&mut self, base: u8, disp: i32, src: u8) {
        self.mem(&[0x89], false, src, base, disp);
    }

    /// `rep movsb` — copies RCX bytes from [RSI] to [RDI].
    pub fn rep_movsb(&mut self) {
        self.code.extend_from_slice(&[0xF3, 0xA4]);
    }

    pub fn lea(&mut self, dst: u8, base: u8, disp: i32) {
        self.mem(&[0x8D], true, dst, base, disp);
    }

    pub fn store_imm32(&mut self, base: u8, disp: i32, v: i32) {
        self.mem(&[0xC7], true, 0, base, disp);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    pub fn store8_imm8(&mut self, base: u8, disp: i32, v: u8) {
        self.mem(&[0xC6], false, 0, base, disp);
        self.code.push(v);
    }

    pub fn cmp_mem8_imm8(&mut self, base: u8, disp: i32, v: u8) {
        self.mem(&[0x80], false, 7, base, disp);
        self.code.push(v);
    }

    // ---- rip-relative ----

    pub fn lea_rip(&mut self, dst: u8, fix: Fixup) {
        self.rip(&[0x8D], true, dst, fix);
    }

    pub fn load_rip(&mut self, dst: u8, fix: Fixup) {
        self.rip(&[0x8B], true, dst, fix);
    }

    pub fn load8zx_rip(&mut self, dst: u8, fix: Fixup) {
        self.rip(&[0x0F, 0xB6], true, dst, fix);
    }

    pub fn store_rip(&mut self, src: u8, fix: Fixup) {
        self.rip(&[0x89], true, src, fix);
    }

    pub fn store_rip8(&mut self, src: u8, fix: Fixup) {
        self.rip(&[0x88], false, src, fix);
    }

    // ---- arithmetic ----

    pub fn add_rr(&mut self, dst: u8, src: u8) {
        self.rr(&[0x01], true, src, dst);
    }

    pub fn sub_rr(&mut self, dst: u8, src: u8) {
        self.rr(&[0x29], true, src, dst);
    }

    pub fn and_rr(&mut self, dst: u8, src: u8) {
        self.rr(&[0x21], true, src, dst);
    }

    pub fn or_rr(&mut self, dst: u8, src: u8) {
        self.rr(&[0x09], true, src, dst);
    }

    pub fn xor_rr(&mut self, dst: u8, src: u8) {
        self.rr(&[0x31], true, src, dst);
    }

    pub fn cmp_rr(&mut self, a: u8, b: u8) {
        self.rr(&[0x39], true, b, a);
    }

    pub fn test_rr(&mut self, a: u8, b: u8) {
        self.rr(&[0x85], true, b, a);
    }

    pub fn imul_rr(&mut self, dst: u8, src: u8) {
        self.rr(&[0x0F, 0xAF], true, dst, src);
    }

    pub fn neg_r(&mut self, r: u8) {
        self.rr(&[0xF7], true, 3, r);
    }

    pub fn not_r(&mut self, r: u8) {
        self.rr(&[0xF7], true, 2, r);
    }

    pub fn idiv_r(&mut self, r: u8) {
        self.rr(&[0xF7], true, 7, r);
    }

    pub fn div_r(&mut self, r: u8) {
        self.rr(&[0xF7], true, 6, r);
    }

    pub fn inc_r(&mut self, r: u8) {
        self.rr(&[0xFF], true, 0, r);
    }

    pub fn dec_r(&mut self, r: u8) {
        self.rr(&[0xFF], true, 1, r);
    }

    pub fn cqo(&mut self) {
        self.code.extend_from_slice(&[0x48, 0x99]);
    }

    pub fn add_imm32(&mut self, r: u8, v: i32) {
        if v >= i8::MIN as i32 && v <= i8::MAX as i32 {
            self.rex(true, 0, r);
            self.code.push(0x83);
            self.code.push(0xC0 | (r & 7));
            self.code.push(v as u8);
        } else {
            self.rex(true, 0, r);
            self.code.push(0x81);
            self.code.push(0xC0 | (r & 7));
            self.code.extend_from_slice(&v.to_le_bytes());
        }
    }

    pub fn add_imm8(&mut self, r: u8, v: u8) {
        self.rex(false, 0, r);
        self.code.push(0x80);
        self.code.push(0xC0 | (r & 7));
        self.code.push(v);
    }

    pub fn cmp_imm8(&mut self, r: u8, v: u8) {
        self.rex(false, 0, r);
        self.code.push(0x80);
        self.code.push(0xF8 | (r & 7));
        self.code.push(v);
    }

    pub fn cmp_imm32(&mut self, r: u8, v: i32) {
        if v >= i8::MIN as i32 && v <= i8::MAX as i32 {
            self.rex(true, 0, r);
            self.code.push(0x83);
            self.code.push(0xF8 | (r & 7));
            self.code.push(v as u8);
        } else {
            self.rex(true, 0, r);
            self.code.push(0x81);
            self.code.push(0xF8 | (r & 7));
            self.code.extend_from_slice(&v.to_le_bytes());
        }
    }

    pub fn shl_cl(&mut self, r: u8) {
        self.rr(&[0xD3], true, 4, r);
    }

    pub fn shl_imm(&mut self, r: u8, imm: u8) {
        self.rex(true, 0, r);
        self.code.push(0xC1);
        self.code.push(0xC0 | (4 << 3) | (r & 7));
        self.code.push(imm);
    }

    pub fn shr_imm(&mut self, r: u8, imm: u8) {
        self.rex(true, 0, r);
        self.code.push(0xC1);
        self.code.push(0xC0 | (5 << 3) | (r & 7));
        self.code.push(imm);
    }

    pub fn shr_cl(&mut self, r: u8) {
        self.rr(&[0xD3], true, 5, r);
    }

    pub fn sar_cl(&mut self, r: u8) {
        self.rr(&[0xD3], true, 7, r);
    }

    pub fn setcc(&mut self, cc: u8) {
        self.code.extend_from_slice(&[0x0F, 0x90 + cc, 0xC0]);
    }

    // ---- SSE2 (scalar double) ----

    fn sse_rr(&mut self, prefix: Option<u8>, w: bool, reg: u8, rm: u8, op: &[u8]) {
        if let Some(p) = prefix {
            self.code.push(p);
        }
        self.rex(w, reg, rm);
        self.code.extend_from_slice(op);
        self.code.push(0xC0 | ((reg & 7) << 3) | (rm & 7));
    }

    fn sse_m(&mut self, prefix: Option<u8>, w: bool, reg: u8, base: u8, disp: i32, op: &[u8]) {
        if let Some(p) = prefix {
            self.code.push(p);
        }
        self.rex(w, reg, base);
        self.code.extend_from_slice(op);
        let b = base & 7;
        self.code.push(0x80 | ((reg & 7) << 3) | b);
        if b == 4 {
            self.code.push(0x24);
        }
        self.code.extend_from_slice(&disp.to_le_bytes());
    }

    fn sse_rip(&mut self, prefix: Option<u8>, w: bool, reg: u8, fix: Fixup, op: &[u8]) {
        if let Some(p) = prefix {
            self.code.push(p);
        }
        self.rex(w, reg, 0);
        self.code.extend_from_slice(op);
        self.code.push(((reg & 7) << 3) | 0x05);
        let at = self.code.len() as u32;
        self.code.extend_from_slice(&[0, 0, 0, 0]);
        self.fixups.push((at, fix));
    }

    /// `movsd xmm, [base + disp]`
    pub fn movsd_xmm_mem(&mut self, dst: u8, base: u8, disp: i32) {
        self.sse_m(Some(0xF2), false, dst, base, disp, &[0x0F, 0x10]);
    }

    /// `movsd [base + disp], xmm`
    pub fn movsd_mem_xmm(&mut self, base: u8, disp: i32, src: u8) {
        self.sse_m(Some(0xF2), false, src, base, disp, &[0x0F, 0x11]);
    }

    /// `movsd xmm, [rip + disp32]`
    pub fn movsd_xmm_rip(&mut self, dst: u8, fix: Fixup) {
        self.sse_rip(Some(0xF2), false, dst, fix, &[0x0F, 0x10]);
    }

    /// `movsd [rip + disp32], xmm`
    pub fn movsd_mem_xmm_rip(&mut self, fix: Fixup, src: u8) {
        self.sse_rip(Some(0xF2), false, src, fix, &[0x0F, 0x11]);
    }

    pub fn movsd_xmm_xmm(&mut self, dst: u8, src: u8) {
        self.sse_rr(Some(0xF2), false, dst, src, &[0x0F, 0x10]);
    }

    pub fn addsd(&mut self, dst: u8, src: u8) {
        self.sse_rr(Some(0xF2), false, dst, src, &[0x0F, 0x58]);
    }

    pub fn subsd(&mut self, dst: u8, src: u8) {
        self.sse_rr(Some(0xF2), false, dst, src, &[0x0F, 0x5C]);
    }

    pub fn mulsd(&mut self, dst: u8, src: u8) {
        self.sse_rr(Some(0xF2), false, dst, src, &[0x0F, 0x59]);
    }

    pub fn divsd(&mut self, dst: u8, src: u8) {
        self.sse_rr(Some(0xF2), false, dst, src, &[0x0F, 0x5E]);
    }

    pub fn ucomisd(&mut self, a: u8, b: u8) {
        self.sse_rr(Some(0x66), false, a, b, &[0x0F, 0x2E]);
    }

    /// `cvtsi2sd xmm, r64`
    pub fn cvtsi2sd(&mut self, xmm: u8, r: u8) {
        self.sse_rr(Some(0xF2), true, xmm, r, &[0x0F, 0x2A]);
    }

    /// `cvttsd2si r64, xmm`
    pub fn cvttsd2si(&mut self, r: u8, xmm: u8) {
        self.sse_rr(Some(0xF2), true, r, xmm, &[0x0F, 0x2C]);
    }

    /// `movq xmm, r64`
    pub fn movq_xmm_r64(&mut self, xmm: u8, r: u8) {
        self.sse_rr(Some(0x66), true, xmm, r, &[0x0F, 0x6E]);
    }

    /// `movq r64, xmm`
    pub fn movq_r64_xmm(&mut self, r: u8, xmm: u8) {
        // 66 REX.W 0F 7E /r — reg holds the XMM source, r/m the GPR target.
        self.sse_rr(Some(0x66), true, xmm, r, &[0x0F, 0x7E]);
    }

    pub fn xorpd(&mut self, dst: u8, src: u8) {
        self.sse_rr(Some(0x66), false, dst, src, &[0x0F, 0x57]);
    }

    // ---- control flow ----

    // ---- generic forms for the inline assembler ----

    /// `op reg, rm` with an explicit REX.W, bypassing the private `rr`.
    pub fn emit_rr(&mut self, op: &[u8], w: bool, reg: u8, rm: u8) {
        self.rr(op, w, reg, rm);
    }

    /// `op reg, [base + disp32]`
    pub fn emit_mem(&mut self, op: &[u8], w: bool, reg: u8, base: u8, disp: i32) {
        self.mem(op, w, reg, base, disp);
    }

    /// `op reg, [base + index*scale + disp32]`
    pub fn emit_mem_sib(
        &mut self,
        op: &[u8],
        w: bool,
        reg: u8,
        base: u8,
        index: u8,
        scale: u8,
        disp: i32,
    ) {
        let mut v = 0x40;
        if w {
            v |= 8;
        }
        if reg >= 8 {
            v |= 4;
        }
        if index >= 8 {
            v |= 2;
        }
        if base >= 8 {
            v |= 1;
        }
        if v != 0x40 {
            self.code.push(v);
        }
        self.code.extend_from_slice(op);
        self.code.push(0x80 | ((reg & 7) << 3) | 0x04);
        let sc = match scale {
            1 => 0u8,
            2 => 1,
            4 => 2,
            _ => 3,
        };
        self.code
            .push((sc << 6) | ((index & 7) << 3) | (base & 7));
        self.code.extend_from_slice(&disp.to_le_bytes());
    }

    /// Group-1 ALU with an immediate: add/or/adc/sbb/and/sub/xor/cmp
    /// (`digit` 0..7). 64-bit when `w`, 32-bit otherwise.
    pub fn alu_imm(&mut self, w: bool, digit: u8, r: u8, imm: i64) {
        if imm >= i8::MIN as i64 && imm <= i8::MAX as i64 {
            self.rex(w, 0, r);
            self.code.push(0x83);
            self.code.push(0xC0 | (digit << 3) | (r & 7));
            self.code.push(imm as u8);
        } else {
            self.rex(w, 0, r);
            self.code.push(0x81);
            self.code.push(0xC0 | (digit << 3) | (r & 7));
            self.code.extend_from_slice(&(imm as i32).to_le_bytes());
        }
    }

    /// 8-bit group-1 ALU with an immediate (opcode 0x80).
    pub fn alu_imm8(&mut self, digit: u8, r: u8, imm: i64) {
        self.rex(false, 0, r);
        self.code.push(0x80);
        self.code.push(0xC0 | (digit << 3) | (r & 7));
        self.code.push(imm as u8);
    }

    /// `mov r32, imm32`
    pub fn mov_imm32_reg(&mut self, r: u8, v: i64) {
        self.rex(false, 0, r);
        self.code.push(0xB8 + (r & 7));
        self.code.extend_from_slice(&(v as i32).to_le_bytes());
    }

    /// `push imm32`
    pub fn push_imm32(&mut self, v: i32) {
        self.code.push(0x68);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    /// `test r, imm`
    pub fn test_imm(&mut self, w: bool, r: u8, imm: i64) {
        self.rex(w, 0, r);
        self.code.push(0xF7);
        self.code.push(0xC0 | (r & 7));
        self.code.extend_from_slice(&(imm as i32).to_le_bytes());
    }

    /// `imul dst, src, imm` (`dst` and `src` are the same register here).
    pub fn imul_imm(&mut self, w: bool, dst: u8, imm: i64) {
        if imm >= i8::MIN as i64 && imm <= i8::MAX as i64 {
            self.rex(w, dst, dst);
            self.code.push(0x6B);
            self.code.push(0xC0 | ((dst & 7) << 3) | (dst & 7));
            self.code.push(imm as u8);
        } else {
            self.rex(w, dst, dst);
            self.code.push(0x69);
            self.code.push(0xC0 | ((dst & 7) << 3) | (dst & 7));
            self.code.extend_from_slice(&(imm as i32).to_le_bytes());
        }
    }

    /// `shl/shr/sar r, imm8` (`digit` 4/5/7).
    pub fn shift_imm(&mut self, w: bool, digit: u8, r: u8, imm: u8) {
        self.rex(w, 0, r);
        self.code.push(0xC1);
        self.code.push(0xC0 | (digit << 3) | (r & 7));
        self.code.push(imm);
    }

    /// `shl/shr/sar r, cl`
    pub fn shift_cl(&mut self, w: bool, digit: u8, r: u8) {
        self.rex(w, 0, r);
        self.code.push(0xD3);
        self.code.push(0xC0 | (digit << 3) | (r & 7));
    }

    /// `setcc r8` (`cc` is the 0..15 condition code).
    pub fn setcc_r(&mut self, cc: u8, r: u8) {
        self.rex(false, 0, r);
        self.code.extend_from_slice(&[0x0F, 0x90 + cc]);
        self.code.push(0xC0 | (r & 7));
    }

    /// `call r`
    pub fn call_r(&mut self, r: u8) {
        self.rex(false, 0, r);
        self.code.push(0xFF);
        self.code.push(0xD0 | (r & 7));
    }

    pub fn syscall(&mut self) {
        self.code.extend_from_slice(&[0x0F, 0x05]);
    }

    pub fn nop(&mut self) {
        self.code.push(0x90);
    }

    pub fn int3(&mut self) {
        self.code.push(0xCC);
    }

    pub fn ud2(&mut self) {
        self.code.extend_from_slice(&[0x0F, 0x0B]);
    }

    pub fn cpuid(&mut self) {
        self.code.extend_from_slice(&[0x0F, 0xA2]);
    }

    pub fn rdtsc(&mut self) {
        self.code.extend_from_slice(&[0x0F, 0x31]);
    }

    /// `op reg, [rip + disp32]` with a `.data` relocation.
    pub fn emit_rip(&mut self, op: &[u8], w: bool, reg: u8, off: u32) {
        self.rip(op, w, reg, Fixup::Data(off));
    }

    /// 8-bit-operand form of `mem`: forces a REX prefix for spl/bpl/sil/dil.
    pub fn emit_mem8(&mut self, op: &[u8], reg: u8, base: u8, disp: i32) {
        if reg >= 4 && reg < 8 && base < 8 {
            self.code.push(0x40);
        } else {
            self.rex(false, reg, base);
        }
        self.code.extend_from_slice(op);
        let b = base & 7;
        self.code.push(0x80 | ((reg & 7) << 3) | b);
        if b == 4 {
            self.code.push(0x24);
        }
        self.code.extend_from_slice(&disp.to_le_bytes());
    }

    /// 8-bit-operand SIB form of `mem`.
    pub fn emit_mem8_sib(&mut self, op: &[u8], reg: u8, base: u8, index: u8, scale: u8, disp: i32) {
        let mut v = 0x40u8;
        if reg >= 8 {
            v |= 4;
        }
        if index >= 8 {
            v |= 2;
        }
        if base >= 8 {
            v |= 1;
        }
        let force = reg >= 4 && reg < 8;
        if v != 0x40 || force {
            self.code.push(v);
        }
        self.code.extend_from_slice(op);
        self.code.push(0x80 | ((reg & 7) << 3) | 0x04);
        let sc = match scale {
            1 => 0u8,
            2 => 1,
            4 => 2,
            _ => 3,
        };
        self.code.push((sc << 6) | ((index & 7) << 3) | (base & 7));
        self.code.extend_from_slice(&disp.to_le_bytes());
    }

    /// `mov r8, imm8`
    pub fn mov_imm8_reg(&mut self, r: u8, v: u8) {
        let mut rex = 0x40u8;
        if r >= 8 {
            rex |= 1;
        }
        if rex != 0x40 || r >= 4 {
            self.code.push(rex);
        }
        self.code.push(0xB0 + (r & 7));
        self.code.push(v);
    }

    /// `op [base + disp32], imm32` for group-1 ALU operations (`digit` 0..7).
    pub fn alu_mem_imm(&mut self, w: bool, digit: u8, base: u8, disp: i32, imm: i32) {
        self.mem(&[0x81], w, digit, base, disp);
        self.code.extend_from_slice(&imm.to_le_bytes());
    }

    /// Generic SSE memory form: `06 [REX] 0F xx /r` with the prefix emitted first.
    pub fn emit_sse_m(
        &mut self,
        p: Option<u8>,
        w: bool,
        reg: u8,
        base: u8,
        disp: i32,
        op: &[u8],
    ) {
        self.sse_m(p, w, reg, base, disp, op);
    }

    /// Generic SSE rip-relative form with a `.data` relocation.
    pub fn emit_sse_rip(&mut self, p: Option<u8>, w: bool, reg: u8, off: u32, op: &[u8]) {
        self.sse_rip(p, w, reg, Fixup::Data(off), op);
    }

    /// Rip-relative `op /digit [rip+disp32]` where `trailing` immediate bytes
    /// follow the displacement (so the relocation must skip them).
    pub fn emit_rip_after(
        &mut self,
        p: Option<u8>,
        w: bool,
        reg: u8,
        off: u32,
        op: &[u8],
        trailing: u8,
    ) {
        if let Some(p) = p {
            self.code.push(p);
        }
        self.rex(w, reg, 0);
        self.code.extend_from_slice(op);
        self.code.push(((reg & 7) << 3) | 0x05);
        let at = self.code.len() as u32;
        self.code.extend_from_slice(&[0, 0, 0, 0]);
        self.fixups.push((at, Fixup::DataAfter(off, trailing)));
    }

    pub fn jmp(&mut self, l: u32) {        self.code.push(0xE9);
        let at = self.code.len() as u32;
        self.code.extend_from_slice(&[0, 0, 0, 0]);
        self.fixups.push((at, Fixup::Local(l)));
    }

    pub fn jcc(&mut self, cc: u8, l: u32) {
        self.code.extend_from_slice(&[0x0F, 0x80 + cc]);
        let at = self.code.len() as u32;
        self.code.extend_from_slice(&[0, 0, 0, 0]);
        self.fixups.push((at, Fixup::Local(l)));
    }

    pub fn call_rel(&mut self, l: u32) {
        self.code.push(0xE8);
        let at = self.code.len() as u32;
        self.code.extend_from_slice(&[0, 0, 0, 0]);
        self.fixups.push((at, Fixup::Local(l)));
    }

    pub fn call_iat(&mut self, i: u32) {
        self.code.extend_from_slice(&[0xFF, 0x15]);
        let at = self.code.len() as u32;
        self.code.extend_from_slice(&[0, 0, 0, 0]);
        self.fixups.push((at, Fixup::Iat(i)));
    }
}
