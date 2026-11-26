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
    // forward declaration: bs_read_alloc uses bs_malloc, defined later
    let ma = asm.label();

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

    // ---------------- bs_read(rcx = buf, rdx = maxlen) -> rax ----------------
    // Reads one line from stdin byte-by-byte, stops at LF/EOF, drops CR,
    // NUL-terminates the buffer and returns the length.
    let rd = asm.label();
    l.insert("bs_read".into(), rd);
    asm.bind(rd);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(96);
    asm.store(RBP, -8, RCX); // buf
    asm.store(RBP, -16, RDX); // maxlen
    asm.mov_imm64(RAX, 0);
    asm.store(RBP, -24, RAX); // len = 0
    asm.mov_imm64(RCX, -10); // STD_INPUT_HANDLE
    asm.call_iat(I_GETSTDHANDLE);
    asm.store(RBP, -32, RAX); // h

    let rloop = asm.label();
    let rdrop = asm.label();
    let rdone = asm.label();
    asm.bind(rloop);
    asm.load(R10, RBP, -24); // len
    asm.load(R11, RBP, -16); // maxlen
    asm.dec_r(R11);
    asm.cmp_rr(R10, R11);
    asm.jcc(CC_BE, rdrop); // len <= maxlen-1 -> continue reading
    asm.jmp(rdone);

    asm.bind(rdrop);
    // ReadFile(h, &byte, 1, &nread, NULL)
    asm.load(RCX, RBP, -32);
    asm.lea(RDX, RBP, -48);
    asm.mov_imm64(R8, 1);
    asm.lea(R9, RBP, -56);
    asm.store_imm32(RSP, 32, 0);
    asm.store_imm32(RBP, -56, 0);
    asm.call_iat(I_READFILE);
    asm.load(R10, RBP, -56);
    asm.test_rr(R10, R10);
    asm.jcc(CC_E, rdone); // EOF

    asm.load8zx(RCX, RBP, -48);
    asm.cmp_imm8(RCX, b'\r');
    asm.jcc(CC_E, rloop); // ignore CR
    asm.cmp_imm8(RCX, b'\n');
    asm.jcc(CC_E, rdone); // LF ends the line

    asm.load(R10, RBP, -8); // buf
    asm.load(RAX, RBP, -24); // len
    asm.add_rr(R10, RAX);
    asm.store8(R10, 0, RCX);
    asm.inc_r(RAX);
    asm.store(RBP, -24, RAX);
    asm.jmp(rloop);

    asm.bind(rdone);
    asm.load(R10, RBP, -8);
    asm.load(RAX, RBP, -24);
    asm.add_rr(R10, RAX);
    asm.xor_rr(RCX, RCX);
    asm.store8(R10, 0, RCX);
    asm.leave();
    asm.ret();
    // ---------------- bs_read_alloc() -> rax (heap string) ----------------
    let rda = asm.label();
    l.insert("bs_read_alloc".into(), rda);
    asm.bind(rda);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(64);
    asm.lea_rip(RCX, Fixup::Data(D_INBUF));
    asm.mov_imm64(RDX, (INBUF_LEN - 1) as i64);
    asm.call_rel(rd);
    asm.store(RBP, -8, RAX); // len
    asm.mov_rr(RCX, RAX);
    asm.inc_r(RCX);
    asm.call_rel(ma); // -> rax = buffer
    asm.store(RBP, -16, RAX);
    asm.lea_rip(R8, Fixup::Data(D_INBUF)); // src
    asm.mov_rr(R9, RAX); // dst
    asm.load(RCX, RBP, -8); // count
    let cpy = asm.label();
    let cpy_done = asm.label();
    asm.bind(cpy);
    asm.test_rr(RCX, RCX);
    asm.jcc(CC_E, cpy_done);
    asm.load8zx(RDX, R8, 0);
    asm.store8(R9, 0, RDX);
    asm.inc_r(R8);
    asm.inc_r(R9);
    asm.dec_r(RCX);
    asm.jmp(cpy);
    asm.bind(cpy_done);
    asm.xor_rr(RDX, RDX);
    asm.store8(R9, 0, RDX);
    asm.load(RAX, RBP, -16);
    asm.leave();
    asm.ret();

    // ---------------- bs_read_int() -> rax ----------------
    let ri = asm.label();
    l.insert("bs_read_int".into(), ri);
    asm.bind(ri);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(64);
    asm.lea(RCX, RBP, -48);
    asm.mov_imm64(RDX, 31);
    asm.call_rel(rd);
    asm.lea(R8, RBP, -48); // cursor
    asm.xor_rr(RAX, RAX); // accumulator
    asm.xor_rr(R9, R9); // sign flag

    let skip = asm.label();
    let skip_adv = asm.label();
    let chk_minus = asm.label();
    let chk_plus = asm.label();
    let digits = asm.label();
    let d_end = asm.label();
    let i_done = asm.label();

    asm.bind(skip);
    asm.load8zx(RCX, R8, 0);
    asm.cmp_imm8(RCX, b' ');
    asm.jcc(CC_E, skip_adv);
    asm.cmp_imm8(RCX, b'\t');
    asm.jcc(CC_NE, chk_minus);
    asm.bind(skip_adv);
    asm.inc_r(R8);
    asm.jmp(skip);

    asm.bind(chk_minus);
    asm.cmp_imm8(RCX, b'-');
    asm.jcc(CC_NE, chk_plus);
    asm.mov_imm64(R9, 1);
    asm.inc_r(R8);
    asm.jmp(digits);

    asm.bind(chk_plus);
    asm.cmp_imm8(RCX, b'+');
    asm.jcc(CC_NE, digits);
    asm.inc_r(R8);

    asm.bind(digits);
    asm.load8zx(RCX, R8, 0);
    asm.cmp_imm8(RCX, b'0');
    asm.jcc(CC_B, d_end);
    asm.cmp_imm8(RCX, b'9');
    asm.jcc(CC_A, d_end);
    asm.add_imm32(RCX, -(b'0' as i32));
    asm.mov_imm64(RDX, 10);
    asm.imul_rr(RAX, RDX);
    asm.add_rr(RAX, RCX);
    asm.inc_r(R8);
    asm.jmp(digits);

    asm.bind(d_end);
    asm.test_rr(R9, R9);
    asm.jcc(CC_E, i_done);
    asm.neg_r(RAX);
    asm.bind(i_done);
    asm.leave();
    asm.ret();

    // ---------------- bs_open(rcx = path, rdx = mode) -> rax ----------------
    // mode: 0 = read, 1 = write (truncate/create), 2 = read+write (open/create)
    let op = asm.label();
    l.insert("bs_open".into(), op);
    asm.bind(op);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(96);
    let m_write = asm.label();
    let m_rw = asm.label();
    let op_go = asm.label();
    asm.cmp_imm32(RDX, 0);
    asm.jcc(CC_NE, m_write);
    // mode 0 (read): fallthrough
    asm.mov_imm64(RDX, 0x8000_0000i64); // GENERIC_READ
    asm.mov_imm64(R10, 3); // OPEN_EXISTING
    asm.jmp(op_go);
    asm.bind(m_write);
    asm.cmp_imm32(RDX, 1);
    asm.jcc(CC_NE, m_rw);
    asm.mov_imm64(RDX, 0x4000_0000i64); // GENERIC_WRITE
    asm.mov_imm64(R10, 2); // CREATE_ALWAYS
    asm.jmp(op_go);
    asm.bind(m_rw);
    asm.mov_imm64(RDX, 0xC000_0000i64); // GENERIC_READ|GENERIC_WRITE
    asm.mov_imm64(R10, 4); // OPEN_ALWAYS
    asm.bind(op_go);
    asm.xor_rr(R8, R8); // dwShareMode = 0
    asm.xor_rr(R9, R9); // lpSecurityAttributes = NULL
    asm.store(RSP, 32, R10); // dwCreationDisposition
    asm.store_imm32(RSP, 40, 0x80); // FILE_ATTRIBUTE_NORMAL
    asm.store_imm32(RSP, 48, 0); // hTemplateFile = NULL
    asm.call_iat(I_CREATEFILEA);
    asm.leave();
    asm.ret();

    // ---------------- bs_file_read(rcx = h, rdx = buf, r8 = len) -> rax ----
    let frd = asm.label();
    l.insert("bs_file_read".into(), frd);
    asm.bind(frd);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(64);
    asm.lea(R9, RBP, -32);
    asm.store_imm32(RSP, 32, 0);
    asm.call_iat(I_READFILE);
    asm.load(RAX, RBP, -32);
    asm.leave();
    asm.ret();

    // ---------------- bs_file_write(rcx = h, rdx = buf, r8 = len) -> rax ---
    let fwr = asm.label();
    l.insert("bs_file_write".into(), fwr);
    asm.bind(fwr);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(64);
    asm.lea(R9, RBP, -32);
    asm.store_imm32(RSP, 32, 0);
    asm.call_iat(I_WRITEFILE);
    asm.load(RAX, RBP, -32);
    asm.leave();
    asm.ret();

    // ---------------- bs_close(rcx = h) ----------------
    let cl = asm.label();
    l.insert("bs_close".into(), cl);
    asm.bind(cl);
    asm.sub_rsp(40);
    asm.call_iat(I_CLOSEHANDLE);
    asm.add_rsp(40);
    asm.ret();

    // ---------------- bs_file_size(rcx = h) -> rax ----------------
    let fsz = asm.label();
    l.insert("bs_file_size".into(), fsz);
    asm.bind(fsz);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(48);
    asm.lea(RDX, RBP, -16);
    asm.call_iat(I_GETFILESIZEEX);
    asm.load(RAX, RBP, -16);
    asm.leave();
    asm.ret();

    // ---------------- bs_print_float(xmm0 = double) ----------------
    // Prints sign, integer part, '.' and six fractional digits.
    let pf = asm.label();
    l.insert("bs_print_float".into(), pf);
    asm.bind(pf);
    asm.push_r(RBP);
    asm.mov_rr(RBP, RSP);
    asm.sub_rsp(96);
    asm.movsd_mem_xmm(RBP, -8, XMM0);

    // NaN / +-Inf are handled by dedicated blocks appended at the end.
    let f_nan = asm.label();
    let f_inf = asm.label();
    asm.movq_r64_xmm(RAX, XMM0);
    asm.mov_imm64(RCX, 0x7FFF_FFFF_FFFF_FFFFu64 as i64);
    asm.and_rr(RAX, RCX);
    asm.mov_imm64(RDX, 0x7FF0_0000_0000_0000u64 as i64);
    asm.cmp_rr(RAX, RDX);
    asm.jcc(CC_A, f_nan);
    asm.jcc(CC_E, f_inf);

    let f_pos = asm.label();
    asm.movq_r64_xmm(RAX, XMM0);
    asm.test_rr(RAX, RAX);
    asm.jcc(CC_NS, f_pos);
    // '-' to the char buffer
    asm.mov_imm64(RCX, b'-' as i64);
    asm.store_rip8(RCX, Fixup::Data(D_CHARBUF));
    asm.lea_rip(RCX, Fixup::Data(D_CHARBUF));
    asm.mov_imm64(RDX, 1);
    asm.call_rel(w);
    // WriteFile may clobber XMM0 (volatile), so reload the value
    asm.movsd_xmm_mem(XMM0, RBP, -8);
    // absolute value: clear the sign bit
    asm.movq_r64_xmm(RAX, XMM0);
    asm.shl_imm(RAX, 1);
    asm.shr_imm(RAX, 1);
    asm.movq_xmm_r64(XMM0, RAX);
    asm.movsd_mem_xmm(RBP, -8, XMM0);

    asm.bind(f_pos);
    asm.cvttsd2si(RAX, XMM0);
    asm.store(RBP, -16, RAX); // integer part
    asm.cvtsi2sd(XMM1, RAX);
    asm.subsd(XMM0, XMM1); // fractional part 0..1
    asm.mov_imm64(RAX, 1_000_000);
    asm.cvtsi2sd(XMM2, RAX);
    asm.mulsd(XMM0, XMM2);
    asm.cvttsd2si(RAX, XMM0);
    asm.store(RBP, -24, RAX); // fractional digits, 0..999999

    asm.load(RCX, RBP, -16);
    asm.call_rel(pi);

    // '.' between integer and fractional parts
    asm.mov_imm64(RCX, b'.' as i64);
    asm.store_rip8(RCX, Fixup::Data(D_CHARBUF));
    asm.lea_rip(RCX, Fixup::Data(D_CHARBUF));
    asm.mov_imm64(RDX, 1);
    asm.call_rel(w);

    // six digits, leading zeros preserved
    asm.mov_imm64(RAX, 100_000);
    asm.store(RBP, -32, RAX); // divisor
    asm.mov_imm64(RAX, 6);
    asm.store(RBP, -40, RAX); // counter

    let d_loop = asm.label();
    asm.bind(d_loop);
    asm.load(R8, RBP, -24); // remaining fraction
    asm.load(R9, RBP, -32); // divisor
    asm.mov_rr(RAX, R8);
    asm.mov_imm64(RDX, 0);
    asm.div_r(R9);
    asm.store(RBP, -24, RDX); // remainder
    asm.add_imm32(RAX, b'0' as i32);
    asm.store_rip8(RAX, Fixup::Data(D_CHARBUF));

    asm.lea_rip(RCX, Fixup::Data(D_CHARBUF));
    asm.mov_imm64(RDX, 1);
    asm.call_rel(w);

    // divisor /= 10
    asm.load(RAX, RBP, -32);
    asm.mov_imm64(RCX, 10);
    asm.mov_imm64(RDX, 0);
    asm.div_r(RCX);
    asm.store(RBP, -32, RAX);
    asm.load(RAX, RBP, -40);
    asm.dec_r(RAX);
    asm.store(RBP, -40, RAX);
    asm.test_rr(RAX, RAX);
    asm.jcc(CC_NE, d_loop);
    asm.leave();
    asm.ret();

    // ---- NaN and +-Inf blocks ----
    let nan_pos = asm.label();
    asm.bind(f_nan);
    asm.movq_r64_xmm(RAX, XMM0);
    asm.test_rr(RAX, RAX);
    asm.jcc(CC_NS, nan_pos);
    asm.mov_imm64(RCX, b'-' as i64);
    asm.store_rip8(RCX, Fixup::Data(D_CHARBUF));
    asm.lea_rip(RCX, Fixup::Data(D_CHARBUF));
    asm.mov_imm64(RDX, 1);
    asm.call_rel(w);
    asm.movsd_xmm_mem(XMM0, RBP, -8);
    asm.bind(nan_pos);
    asm.mov_imm64(RAX, (b'n' as i64) | ((b'a' as i64) << 8) | ((b'n' as i64) << 16));
    asm.store(RBP, -48, RAX);
    asm.lea(RCX, RBP, -48);
    asm.mov_imm64(RDX, 3);
    asm.call_rel(w);
    asm.leave();
    asm.ret();

    let inf_pos = asm.label();
    asm.bind(f_inf);
    asm.movq_r64_xmm(RAX, XMM0);
    asm.test_rr(RAX, RAX);
    asm.jcc(CC_NS, inf_pos);
    asm.mov_imm64(RCX, b'-' as i64);
    asm.store_rip8(RCX, Fixup::Data(D_CHARBUF));
    asm.lea_rip(RCX, Fixup::Data(D_CHARBUF));
    asm.mov_imm64(RDX, 1);
    asm.call_rel(w);
    asm.bind(inf_pos);
    asm.mov_imm64(RAX, (b'i' as i64) | ((b'n' as i64) << 8) | ((b'f' as i64) << 16));
    asm.store(RBP, -48, RAX);
    asm.lea(RCX, RBP, -48);
    asm.mov_imm64(RDX, 3);
    asm.call_rel(w);
    asm.leave();
    asm.ret();

    // ---------------- bs_malloc(rcx = size) -> rax ----------------
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
