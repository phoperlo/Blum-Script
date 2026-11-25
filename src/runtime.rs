//! BlumScript runtime, written directly in machine code.
//!
//! These routines are emitted by the compiler itself; there is no C file, no
//! libc and no external runtime object.  All output goes through
//! `WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), ...)`.

use crate::abi::*;
use crate::x64::*;
use std::collections::HashMap;

pub type Labels = HashMap<String, u32>;

/// Emit every runtime helper and return their labels.
///
/// `null_off` is the `.rdata` offset of the literal "null".
pub fn emit(asm: &mut Asm, null_off: u32) -> Labels {
    let mut l = Labels::new();

    // ---------------- bs_write(rcx = ptr, rdx = len) ----------------
    let w = asm.label();
    l.insert("bs_write".into(), w);
    asm.bind(w);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(48);
    asm.mov_rr(R9, RSP); // &written
    asm.mov_rr(R8, RDX); // nBytes
    asm.mov_rr(RDX, RCX); // lpBuffer
    asm.load_rip(RCX, Fixup::Data(D_STDOUT)); // hFile
    asm.store_imm32(RSP, 32, 0); // lpOverlapped = NULL
    asm.call_iat(I_WRITEFILE);
    asm.leave();
    asm.ret();

    // ---------------- bs_strlen(rcx = ptr) -> rax ----------------
    let sl = asm.label();
    l.insert("bs_strlen".into(), sl);
    asm.bind(sl);
    asm.mov_rr(RDX, RCX);
    let loop_l = asm.label();
    let done_l = asm.label();
    asm.bind(loop_l);
    asm.cmp_mem8_imm8(RDX, 0, 0);
    asm.jcc(CC_E, done_l);
    asm.inc_r(RDX);
    asm.jmp(loop_l);
    asm.bind(done_l);
    asm.mov_rr(RAX, RDX);
    asm.sub_rr(RAX, RCX);
    asm.ret();

    // ---------------- bs_print_int(rcx) ----------------
    let pi = asm.label();
    l.insert("bs_print_int".into(), pi);
    asm.bind(pi);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(64);
    asm.mov_rr(RAX, RCX);
    asm.lea(R8, RSP, 63);
    asm.mov_rr(R9, R8);
    asm.xor_rr(R10, R10);
    asm.test_rr(RAX, RAX);
    let pos = asm.label();
    asm.jcc(CC_NS, pos);
    asm.mov_imm64(R10, 1);
    asm.neg_r(RAX);
    asm.bind(pos);
    asm.mov_imm64(RCX, 10);
    let iloop = asm.label();
    let iout = asm.label();
    asm.bind(iloop);
    asm.xor_rr(RDX, RDX);
    asm.div_r(RCX);
    asm.add_imm8(RDX, b'0');
    asm.dec_r(R9);
    asm.store8(R9, 0, RDX);
    asm.test_rr(RAX, RAX);
    asm.jcc(CC_NE, iloop);
    asm.test_rr(R10, R10);
    asm.jcc(CC_E, iout);
    asm.dec_r(R9);
    asm.store8_imm8(R9, 0, b'-');
    asm.bind(iout);
    asm.mov_rr(RCX, R9);
    asm.mov_rr(RDX, R8);
    asm.sub_rr(RDX, R9);
    asm.call_rel(w);
    asm.leave();
    asm.ret();

    // ---------------- bs_print_uint(rcx) ----------------
    let pu = asm.label();
    l.insert("bs_print_uint".into(), pu);
    asm.bind(pu);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(64);
    asm.mov_rr(RAX, RCX);
    asm.lea(R8, RSP, 63);
    asm.mov_rr(R9, R8);
    asm.mov_imm64(RCX, 10);
    let uloop = asm.label();
    asm.bind(uloop);
    asm.xor_rr(RDX, RDX);
    asm.div_r(RCX);
    asm.add_imm8(RDX, b'0');
    asm.dec_r(R9);
    asm.store8(R9, 0, RDX);
    asm.test_rr(RAX, RAX);
    asm.jcc(CC_NE, uloop);
    asm.mov_rr(RCX, R9);
    asm.mov_rr(RDX, R8);
    asm.sub_rr(RDX, R9);
    asm.call_rel(w);
    asm.leave();
    asm.ret();

    // ---------------- bs_print_hex(rcx) ----------------
    let ph = asm.label();
    l.insert("bs_print_hex".into(), ph);
    asm.bind(ph);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(64);
    asm.mov_rr(RAX, RCX);
    asm.lea(R8, RSP, 63);
    asm.mov_rr(R9, R8);
    asm.mov_imm64(RCX, 16);
    let hloop = asm.label();
    let hdigit = asm.label();
    let hstore = asm.label();
    asm.bind(hloop);
    asm.xor_rr(RDX, RDX);
    asm.div_r(RCX);
    asm.cmp_imm8(RDX, 10);
    asm.jcc(CC_B, hdigit);
    asm.add_imm8(RDX, b'a' - 10);
    asm.jmp(hstore);
    asm.bind(hdigit);
    asm.add_imm8(RDX, b'0');
    asm.bind(hstore);
    asm.dec_r(R9);
    asm.store8(R9, 0, RDX);
    asm.test_rr(RAX, RAX);
    asm.jcc(CC_NE, hloop);
    asm.dec_r(R9);
    asm.store8_imm8(R9, 0, b'x');
    asm.dec_r(R9);
    asm.store8_imm8(R9, 0, b'0');
    asm.mov_rr(RCX, R9);
    asm.mov_rr(RDX, R8);
    asm.sub_rr(RDX, R9);
    asm.call_rel(w);
    asm.leave();
    asm.ret();

    // ---------------- bs_print_str(rcx = ptr) ----------------
    let ps = asm.label();
    l.insert("bs_print_str".into(), ps);
    asm.bind(ps);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(32);
    let have = asm.label();
    asm.test_rr(RCX, RCX);
    asm.jcc(CC_NE, have);
    asm.lea_rip(RCX, Fixup::Rdata(null_off));
    asm.bind(have);
    asm.store(RSP, 24, RCX);
    asm.call_rel(sl);
    asm.mov_rr(RDX, RAX);
    asm.load(RCX, RSP, 24);
    asm.call_rel(w);
    asm.leave();
    asm.ret();

    // ---------------- bs_malloc(rcx = size) -> rax ----------------
    let ma = asm.label();
    l.insert("bs_malloc".into(), ma);
    asm.bind(ma);
    asm.sub_rsp(40);
    asm.store(RSP, 32, RCX);
    asm.call_iat(I_GETPROCESSHEAP);
    asm.mov_rr(RCX, RAX);
    asm.xor_rr(RDX, RDX);
    asm.load(R8, RSP, 32);
    asm.call_iat(I_HEAPALLOC);
    asm.add_rsp(40);
    asm.ret();

    // ---------------- bs_free(rcx = ptr) ----------------
    let fr = asm.label();
    l.insert("bs_free".into(), fr);
    asm.bind(fr);
    asm.sub_rsp(40);
    asm.store(RSP, 32, RCX);
    asm.call_iat(I_GETPROCESSHEAP);
    asm.mov_rr(RCX, RAX);
    asm.xor_rr(RDX, RDX);
    asm.load(R8, RSP, 32);
    asm.call_iat(I_HEAPFREE);
    asm.add_rsp(40);
    asm.ret();

    l
}
