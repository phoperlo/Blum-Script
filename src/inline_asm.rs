//! Textual inline assembler for `asm { ... }` blocks.
//!
//! A block is a sequence of lines, each one of:
//!   * a label definition   `name:`
//!   * an instruction       `mov rax, 42`
//!   * raw hex bytes        `48 c7 c0 2a 00 00 00`  (backward-compatible form)
//!   * a comment            `; ...` or `// ...`
//!
//! Supported mnemonics: mov, movzx, lea, add, sub, and, or, xor, cmp, test,
//! imul, idiv, div, neg, not, inc, dec, shl, shr, sar, jmp, je/jz, jne/jnz,
//! jg/jge/jl/jle, ja/jae/jb/jbe, js/jns, jo/jno, jp/jnp, setcc, call, ret,
//! push, pop, cqo/cdq, leave, syscall, nop, int3, ud2, cpuid, rdtsc,
//! movsd/addsd/subsd/mulsd/divsd/ucomisd/cvtsi2sd/cvttsd2si/movq.
//!
//! Operands: `rax`/`eax`/`al` (and the rest of the GPR file, `r8`..`r15`),
//! `xmm0`..`xmm15`, immediates (`42`, `0x2a`, `0b1010`, `-1`), memory
//! (`[rax+8]`, `[rax+rbx*4+16]`, `[my_local]`, `[my_local+4]`, `[my_global]`,
//! `[rip+...]` is not needed: `[my_global]` already encodes rip-relative) and
//! labels.
//!
//! Optional size prefixes are accepted on memory operands: `byte`, `dword`,
//! `qword` (optionally followed by `ptr`).  `word` (16-bit) is rejected.
//! Registers default to 64-bit; `eax` selects the 32-bit form, `al` the 8-bit
//! one.

use std::collections::HashMap;

use crate::x64::*;

pub struct Ctx<'a> {
    /// Visible locals: name -> `rbp` displacement (a negative number).
    pub locals: &'a HashMap<String, i32>,
    /// Globals: name -> `.data` offset.
    pub globals: &'a HashMap<String, u32>,
    /// Function labels: name -> assembler label id.
    pub fns: &'a HashMap<String, u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sz {
    B1,
    B4,
    B8,
}

fn gpr(name: &str) -> Option<(u8, Sz)> {
    let (id, sz) = match name {
        "rax" => (0, Sz::B8),
        "rcx" => (1, Sz::B8),
        "rdx" => (2, Sz::B8),
        "rbx" => (3, Sz::B8),
        "rsp" => (4, Sz::B8),
        "rbp" => (5, Sz::B8),
        "rsi" => (6, Sz::B8),
        "rdi" => (7, Sz::B8),
        "eax" => (0, Sz::B4),
        "ecx" => (1, Sz::B4),
        "edx" => (2, Sz::B4),
        "ebx" => (3, Sz::B4),
        "esp" => (4, Sz::B4),
        "ebp" => (5, Sz::B4),
        "esi" => (6, Sz::B4),
        "edi" => (7, Sz::B4),
        "al" => (0, Sz::B1),
        "cl" => (1, Sz::B1),
        "dl" => (2, Sz::B1),
        "bl" => (3, Sz::B1),
        "sil" => (6, Sz::B1),
        "dil" => (7, Sz::B1),
        _ => {
            let base = name.strip_prefix('r')?;
            let (num, sz) = if let Some(d) = base.strip_suffix('d') {
                (d, Sz::B4)
            } else if let Some(b) = base.strip_suffix('b') {
                (b, Sz::B1)
            } else if let Some(w) = base.strip_suffix('w') {
                (w, Sz::B8) // r8w..r15w are 16-bit; rejected by the caller
            } else {
                (base, Sz::B8)
            };
            if sz == Sz::B8 && base.ends_with('w') {
                return None;
            }
            let n: u8 = num.parse().ok()?;
            if !(8..=15).contains(&n) {
                return None;
            }
            (n, sz)
        }
    };
    Some((id, sz))
}

fn xmm(name: &str) -> Option<u8> {
    name.strip_prefix("xmm")?.parse::<u8>().ok().filter(|n| *n <= 15)
}

/// 16-bit registers are recognised only to produce a clear error.
fn is_16bit_reg(name: &str) -> bool {
    matches!(name, "ax" | "bx" | "cx" | "dx" | "si" | "di" | "bp" | "sp")
}

fn cond(name: &str) -> Option<u8> {
    Some(match name {
        "je" | "jz" => CC_E,
        "jne" | "jnz" => CC_NE,
        "jg" => CC_G,
        "jge" => CC_GE,
        "jl" => CC_L,
        "jle" => CC_LE,
        "ja" => CC_A,
        "jae" | "jnb" => CC_AE,
        "jb" => CC_B,
        "jbe" => CC_BE,
        "js" => CC_S,
        "jns" => CC_NS,
        "jo" => CC_O,
        "jno" => CC_NO,
        "jp" => CC_P,
        "jnp" => CC_NP,
        _ => return None,
    })
}

fn cc_of(name: &str) -> Option<u8> {
    cond(&format!("j{}", name))
}

#[derive(Clone)]
enum Mem {
    /// `[rip + disp]` where `disp` is a `.data` offset.
    Data(u32),
    Base {
        base: u8,
        index: Option<(u8, u8)>,
        disp: i32,
    },
}

#[derive(Clone)]
enum Op {
    R(u8, Sz),
    X(u8),
    I(i64),
    M(Mem, Sz),
    L(String),
}

/// Split on top-level commas (ignoring commas inside `[...]`).
fn split_ops(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '[' => {
                depth += 1;
                cur.push(c);
            }
            ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn parse_imm(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, body) = if let Some(r) = s.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = s.strip_prefix('+') {
        (false, r)
    } else {
        (false, s)
    };
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()?
    } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        i64::from_str_radix(b, 2).ok()?
    } else {
        body.parse::<i64>().ok()?
    };
    Some(if neg { -v } else { v })
}

fn parse_mem(inner: &str, ctx: &Ctx<'_>) -> Result<Mem, String> {
    let inner = inner.trim();
    if inner.is_empty() {
        return Err("asm: empty memory operand".into());
    }
    let mut terms: Vec<(bool, String)> = Vec::new();
    let mut cur = String::new();
    let mut sign = false;
    for c in inner.chars() {
        match c {
            '+' => {
                if !cur.trim().is_empty() {
                    terms.push((sign, cur.trim().to_string()));
                }
                cur.clear();
                sign = false;
            }
            '-' => {
                if !cur.trim().is_empty() {
                    terms.push((sign, cur.trim().to_string()));
                }
                cur.clear();
                sign = true;
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        terms.push((sign, cur.trim().to_string()));
    }

    let mut base: Option<u8> = None;
    let mut index: Option<(u8, u8)> = None;
    let mut disp: i32 = 0;
    let mut have_reg = false;

    for (neg, raw) in terms {
        let t = raw.trim();
        if let Some((r, sc)) = t.split_once('*') {
            let (id, sz) = gpr(r.trim()).ok_or_else(|| format!("asm: bad index '{}'", r))?;
            if sz != Sz::B8 {
                return Err(format!("asm: index '{}' must be 64-bit", r));
            }
            let scale = parse_imm(sc).ok_or_else(|| format!("asm: bad scale '{}'", sc))?;
            if ![1, 2, 4, 8].contains(&scale) {
                return Err(format!("asm: scale {} is not 1/2/4/8", scale));
            }
            index = Some((id, scale as u8));
            have_reg = true;
            continue;
        }
        if let Some((id, sz)) = gpr(t) {
            if sz != Sz::B8 {
                return Err(format!(
                    "asm: '{}' cannot be used as an address register",
                    t
                ));
            }
            if base.is_none() {
                base = Some(id);
            } else if index.is_none() {
                index = Some((id, 1));
            } else {
                return Err("asm: memory operand has too many registers".into());
            }
            have_reg = true;
            continue;
        }
        if let Some(v) = parse_imm(t) {
            let v = v as i32;
            disp = disp.wrapping_add(if neg { -v } else { v });
            continue;
        }
        if let Some(off) = ctx.locals.get(t) {
            if base.is_some() && base != Some(RBP) {
                return Err(format!("asm: local '{}' needs rbp addressing", t));
            }
            base = Some(RBP);
            let v = if neg { -(*off) } else { *off };
            // `off` is already the (negative) rbp displacement.
            disp = disp.wrapping_add(v);
            have_reg = true;
            continue;
        }
        if let Some(off) = ctx.globals.get(t) {
            if have_reg || disp != 0 {
                return Err(format!(
                    "asm: global '{}' cannot be combined with other address terms",
                    t
                ));
            }
            return Ok(Mem::Data(*off));
        }
        return Err(format!("asm: unknown symbol '{}'", t));
    }

    match base {
        Some(b) => Ok(Mem::Base { base: b, index, disp }),
        None => Err("asm: memory operand needs a base register or a global".into()),
    }
}

/// Peel an optional `byte` / `dword` / `qword` (with optional `ptr`) prefix.
fn strip_size(s: &str) -> Result<(&str, Option<Sz>), String> {
    let low = s.trim_start();
    for (kw, sz) in [("byte", Sz::B1), ("dword", Sz::B4), ("qword", Sz::B8)] {
        if let Some(rest) = low.strip_prefix(kw) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                let mut r = rest.trim_start();
                if let Some(x) = r.strip_prefix("ptr") {
                    if x.is_empty() || x.starts_with(char::is_whitespace) {
                        r = x.trim_start();
                    }
                }
                return Ok((r, Some(sz)));
            }
        }
    }
    if let Some(rest) = low.strip_prefix("word") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Err("asm: 16-bit (word) memory operands are not supported".into());
        }
    }
    Ok((low, None))
}

fn parse_op(s: &str, ctx: &Ctx<'_>) -> Result<Op, String> {
    let (s, hint) = strip_size(s)?;
    let s = s.trim();
    if s.is_empty() {
        return Err("asm: empty operand".into());
    }
    if let Some(inner) = s.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or_else(|| format!("asm: missing ']' in '{}'", s))?;
        return Ok(Op::M(parse_mem(inner, ctx)?, hint.unwrap_or(Sz::B8)));
    }
    if is_16bit_reg(s) {
        return Err(format!("asm: 16-bit register '{}' is not supported", s));
    }
    if let Some((id, sz)) = gpr(s) {
        return Ok(Op::R(id, sz));
    }
    if let Some(x) = xmm(s) {
        return Ok(Op::X(x));
    }
    if let Some(v) = parse_imm(s) {
        return Ok(Op::I(v));
    }
    Ok(Op::L(s.to_string()))
}

fn each_line(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(|l| l.trim()).filter(|l| !l.is_empty())
}

fn is_hex_bytes(line: &str) -> bool {
    let mut any = false;
    for tok in line.split(|c: char| c.is_whitespace() || c == ',') {
        let t = tok.trim();
        if t.is_empty() {
            continue;
        }
        let t = t
            .strip_prefix("0x")
            .or_else(|| t.strip_prefix("0X"))
            .unwrap_or(t);
        if t.len() > 2 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        any = true;
    }
    any
}

fn strip_comment(line: &str) -> &str {
    let mut cut = line.len();
    if let Some(p) = line.find(';') {
        cut = cut.min(p);
    }
    if let Some(p) = line.find("//") {
        cut = cut.min(p);
    }
    line[..cut].trim()
}

fn emit_mov_reg_reg(asm: &mut Asm, d: u8, s: u8, sz: Sz) {
    match sz {
        Sz::B8 => asm.mov_rr(d, s),
        Sz::B4 => asm.emit_rr(&[0x89], false, s, d),
        Sz::B1 => asm.mov_rr8(d, s),
    }
}

fn emit_mov_reg_imm(asm: &mut Asm, d: u8, v: i64, sz: Sz) {
    match sz {
        Sz::B8 => asm.mov_imm64(d, v),
        Sz::B4 => asm.mov_imm32_reg(d, v),
        Sz::B1 => asm.mov_imm8_reg(d, v as u8),
    }
}

fn emit_load(asm: &mut Asm, d: u8, m: &Mem, sz: Sz) {
    match m {
        Mem::Data(off) => match sz {
            Sz::B8 => asm.load_rip(d, Fixup::Data(*off)),
            Sz::B4 => asm.emit_rip(&[0x8B], false, d, *off),
            Sz::B1 => asm.load8zx_rip(d, Fixup::Data(*off)),
        },
        Mem::Base { base, index, disp } => {
            let w = sz == Sz::B8;
            match (sz, index) {
                (Sz::B1, None) => asm.emit_mem8(&[0x0F, 0xB6], d, *base, *disp),
                (Sz::B1, Some((i, sc))) => {
                    asm.emit_mem8_sib(&[0x0F, 0xB6], d, *base, *i, *sc, *disp)
                }
                (_, None) => asm.emit_mem(&[0x8B], w, d, *base, *disp),
                (_, Some((i, sc))) => asm.emit_mem_sib(&[0x8B], w, d, *base, *i, *sc, *disp),
            }
        }
    }
}

fn emit_store(asm: &mut Asm, m: &Mem, s: u8, sz: Sz) {
    match m {
        Mem::Data(off) => match sz {
            Sz::B8 => asm.store_rip(s, Fixup::Data(*off)),
            Sz::B4 => asm.emit_rip(&[0x89], false, s, *off),
            Sz::B1 => asm.store_rip8(s, Fixup::Data(*off)),
        },
        Mem::Base { base, index, disp } => {
            let w = sz == Sz::B8;
            match (sz, index) {
                (Sz::B1, None) => asm.emit_mem8(&[0x88], s, *base, *disp),
                (Sz::B1, Some((i, sc))) => asm.emit_mem8_sib(&[0x88], s, *base, *i, *sc, *disp),
                (_, None) => asm.emit_mem(&[0x89], w, s, *base, *disp),
                (_, Some((i, sc))) => asm.emit_mem_sib(&[0x89], w, s, *base, *i, *sc, *disp),
            }
        }
    }
}

fn emit_imm_to_mem(asm: &mut Asm, m: &Mem, sz: Sz, v: i64) {
    match m {
        Mem::Data(off) => match sz {
            Sz::B8 => {
                asm.emit_rip_after(None, true, 0, *off, &[0xC7], 4);
                asm.raw(&(v as i32).to_le_bytes());
            }
            Sz::B4 => {
                asm.emit_rip_after(None, false, 0, *off, &[0xC7], 4);
                asm.raw(&(v as i32).to_le_bytes());
            }
            Sz::B1 => {
                asm.emit_rip_after(None, false, 0, *off, &[0xC6], 1);
                asm.raw(&[v as u8]);
            }
        },
        Mem::Base { base, index, disp } => match sz {
            Sz::B1 => {
                match index {
                    Some((i, sc)) => asm.emit_mem8_sib(&[0xC6], 0, *base, *i, *sc, *disp),
                    None => asm.emit_mem8(&[0xC6], 0, *base, *disp),
                }
                asm.raw(&[v as u8]);
            }
            sz => {
                let w = sz == Sz::B8;
                match index {
                    Some((i, sc)) => asm.emit_mem_sib(&[0xC7], w, 0, *base, *i, *sc, *disp),
                    None => asm.emit_mem(&[0xC7], w, 0, *base, *disp),
                }
                asm.raw(&(v as i32).to_le_bytes());
            }
        },
    }
}

fn emit_alu_mem_imm(asm: &mut Asm, sz: Sz, digit: u8, m: &Mem, v: i64) {
    match m {
        Mem::Base { base, index, disp } => match sz {
            Sz::B1 => {
                match index {
                    Some((i, sc)) => asm.emit_mem8_sib(&[0x80], digit, *base, *i, *sc, *disp),
                    None => asm.emit_mem8(&[0x80], digit, *base, *disp),
                }
                asm.raw(&[v as u8]);
            }
            _ => {
                let w = sz == Sz::B8;
                match index {
                    Some((i, sc)) => {
                        asm.emit_mem_sib(&[0x81], w, digit, *base, *i, *sc, *disp)
                    }
                    None => asm.emit_mem(&[0x81], w, digit, *base, *disp),
                }
                asm.raw(&(v as i32).to_le_bytes());
            }
        },
        Mem::Data(_) => {
            // rip-relative ALU with an immediate: encode the modrm/disp first.
            if let Mem::Data(off) = m {
                match sz {
                    Sz::B1 => {
                        asm.emit_rip_after(None, false, digit, *off, &[0x80], 1);
                        asm.raw(&[v as u8]);
                    }
                    _ => {
                        asm.emit_rip_after(None, sz == Sz::B8, digit, *off, &[0x81], 4);
                        asm.raw(&(v as i32).to_le_bytes());
                    }
                }
            }
        }
    }
}

pub fn assemble(text: &str, asm: &mut Asm, ctx: &Ctx<'_>) -> Result<(), String> {
    // Pass 1: pre-create labels so forward jumps work.
    let mut labels: HashMap<String, u32> = HashMap::new();
    for raw in each_line(text) {
        let line = strip_comment(raw);
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_suffix(':') {
            let name = name.trim();
            if name.is_empty() {
                return Err("asm: empty label name".into());
            }
            if labels.contains_key(name) {
                return Err(format!("asm: duplicate label '{}'", name));
            }
            let id = asm.label();
            labels.insert(name.to_string(), id);
        }
    }

    // Pass 2: emit.
    for raw in each_line(text) {
        let line = strip_comment(raw);
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_suffix(':') {
            let id = labels[name.trim()];
            asm.bind(id);
            continue;
        }
        if is_hex_bytes(line) {
            for tok in line.split(|c: char| c.is_whitespace() || c == ',') {
                let t = tok.trim();
                if t.is_empty() {
                    continue;
                }
                let t = t
                    .strip_prefix("0x")
                    .or_else(|| t.strip_prefix("0X"))
                    .unwrap_or(t);
                let b = u8::from_str_radix(t, 16)
                    .map_err(|_| format!("asm: '{}' is not a hex byte", tok))?;
                asm.raw(&[b]);
            }
            continue;
        }

        let (mn, rest) = match line.split_once(char::is_whitespace) {
            Some((m, r)) => (m.to_ascii_lowercase(), r.trim()),
            None => (line.to_ascii_lowercase(), ""),
        };
        let args = if rest.is_empty() {
            Vec::new()
        } else {
            split_ops(rest)
        };
        let ops: Vec<Op> = args
            .iter()
            .map(|a| parse_op(a, ctx))
            .collect::<Result<_, String>>()?;
        emit(asm, &mn, &ops, &labels, ctx)?;
    }
    Ok(())
}

fn label_target(name: &str, labels: &HashMap<String, u32>, ctx: &Ctx<'_>) -> Result<u32, String> {
    if let Some(l) = labels.get(name) {
        return Ok(*l);
    }
    if let Some(l) = ctx.fns.get(name) {
        return Ok(*l);
    }
    Err(format!("asm: undefined label '{}'", name))
}

fn emit(
    asm: &mut Asm,
    mn: &str,
    ops: &[Op],
    labels: &HashMap<String, u32>,
    ctx: &Ctx<'_>,
) -> Result<(), String> {
    // ---- no-operand instructions ----
    match mn {
        "ret" => {
            asm.ret();
            return Ok(());
        }
        "leave" => {
            asm.leave();
            return Ok(());
        }
        "syscall" => {
            asm.syscall();
            return Ok(());
        }
        "nop" => {
            asm.nop();
            return Ok(());
        }
        "int3" => {
            asm.int3();
            return Ok(());
        }
        "ud2" => {
            asm.ud2();
            return Ok(());
        }
        "cpuid" => {
            asm.cpuid();
            return Ok(());
        }
        "rdtsc" => {
            asm.rdtsc();
            return Ok(());
        }
        "cqo" | "cdq" => {
            asm.cqo();
            return Ok(());
        }
        _ => {}
    }

    if mn == "jmp" {
        match ops.first() {
            Some(Op::L(n)) => {
                let t = label_target(n, labels, ctx)?;
                asm.jmp(t);
                return Ok(());
            }
            Some(Op::R(r, _)) => {
                asm.emit_rr(&[0xFF], false, 4, *r);
                return Ok(());
            }
            _ => return Err("asm: jmp needs a label or register".into()),
        }
    }

    if let Some(cc) = cond(mn) {
        match ops.first() {
            Some(Op::L(n)) => {
                let t = label_target(n, labels, ctx)?;
                asm.jcc(cc, t);
                return Ok(());
            }
            _ => return Err(format!("asm: {} needs a label", mn)),
        }
    }

    if let Some(ccraw) = mn.strip_prefix("set") {
        if let Some(cc) = cc_of(ccraw) {
            if let Some(Op::R(r, Sz::B1)) = ops.first() {
                asm.setcc_r(cc, *r);
                return Ok(());
            }
            return Err("asm: setcc needs an 8-bit register".into());
        }
    }

    if mn == "call" {
        match ops.first() {
            Some(Op::L(n)) => {
                let t = label_target(n, labels, ctx)?;
                asm.call_rel(t);
                return Ok(());
            }
            Some(Op::R(r, _)) => {
                asm.call_r(*r);
                return Ok(());
            }
            _ => return Err("asm: call needs a label or register".into()),
        }
    }

    if mn == "push" {
        match ops.first() {
            Some(Op::R(r, _)) => {
                asm.push_r(*r);
                return Ok(());
            }
            Some(Op::I(v)) => {
                asm.push_imm32(*v as i32);
                return Ok(());
            }
            _ => return Err("asm: push needs a register or immediate".into()),
        }
    }

    if mn == "pop" {
        if let Some(Op::R(r, _)) = ops.first() {
            asm.pop_r(*r);
            return Ok(());
        }
        return Err("asm: pop needs a register".into());
    }

    // ---- SSE ----
    let sse_reg = match mn {
        "movsd" => Some((Some(0xF2), &[0x0F, 0x10u8][..])),
        "addsd" => Some((Some(0xF2), &[0x0F, 0x58][..])),
        "subsd" => Some((Some(0xF2), &[0x0F, 0x5C][..])),
        "mulsd" => Some((Some(0xF2), &[0x0F, 0x59][..])),
        "divsd" => Some((Some(0xF2), &[0x0F, 0x5E][..])),
        "ucomisd" => Some((Some(0x66), &[0x0F, 0x2E][..])),
        _ => None,
    };
    if let Some((p, op)) = sse_reg {
        let (a, b) = match (ops.first(), ops.get(1)) {
            (Some(a), Some(b)) => (a, b),
            _ => return Err(format!("asm: '{}' needs two operands", mn)),
        };
        match (a, b) {
            (Op::X(d), Op::X(s)) => match mn {
                "movsd" => asm.movsd_xmm_xmm(*d, *s),
                "addsd" => asm.addsd(*d, *s),
                "subsd" => asm.subsd(*d, *s),
                "mulsd" => asm.mulsd(*d, *s),
                "divsd" => asm.divsd(*d, *s),
                _ => asm.ucomisd(*d, *s),
            },
            (Op::X(d), Op::M(Mem::Base { base, index, disp }, _)) => {
                if index.is_some() {
                    return Err(format!("asm: '{}' does not support SIB operands", mn));
                }
                asm.emit_sse_m(p, false, *d, *base, *disp, op);
            }
            (Op::X(d), Op::M(Mem::Data(off), _)) => {
                let p = p.unwrap_or(0x66);
                if mn == "movsd" {
                    asm.movsd_xmm_rip(*d, Fixup::Data(*off));
                } else {
                    asm.emit_sse_rip(Some(p), false, *d, *off, op);
                }
            }
            (Op::M(Mem::Base { base, index, disp }, _), Op::X(s)) => {
                if mn != "movsd" {
                    return Err(format!("asm: '{}' cannot store its destination", mn));
                }
                if index.is_some() {
                    return Err("asm: movsd does not support SIB operands".into());
                }
                asm.movsd_mem_xmm(*base, *disp, *s);
            }
            (Op::M(Mem::Data(off), _), Op::X(s)) => {
                if mn != "movsd" {
                    return Err(format!("asm: '{}' cannot store its destination", mn));
                }
                asm.movsd_mem_xmm_rip(Fixup::Data(*off), *s);
            }
            _ => return Err(format!("asm: bad operands for '{}'", mn)),
        }
        return Ok(());
    }

    if mn == "cvtsi2sd" {
        if let (Some(Op::X(d)), Some(Op::R(s, _))) = (ops.first(), ops.get(1)) {
            asm.cvtsi2sd(*d, *s);
            return Ok(());
        }
        return Err("asm: cvtsi2sd needs xmm, reg64".into());
    }
    if mn == "cvttsd2si" {
        if let (Some(Op::R(d, _)), Some(Op::X(s))) = (ops.first(), ops.get(1)) {
            asm.cvttsd2si(*d, *s);
            return Ok(());
        }
        return Err("asm: cvttsd2si needs reg64, xmm".into());
    }
    if mn == "movq" {
        if let (Some(Op::X(d)), Some(Op::R(s, _))) = (ops.first(), ops.get(1)) {
            asm.movq_xmm_r64(*d, *s);
            return Ok(());
        }
        if let (Some(Op::R(d, _)), Some(Op::X(s))) = (ops.first(), ops.get(1)) {
            asm.movq_r64_xmm(*d, *s);
            return Ok(());
        }
        return Err("asm: movq needs xmm, reg64 or reg64, xmm".into());
    }

    // ---- single-operand instructions ----
    if mn == "idiv" || mn == "div" || mn == "neg" || mn == "not" || mn == "inc" || mn == "dec"
    {
        let r = match ops.first() {
            Some(Op::R(r, _)) => *r,
            _ => return Err(format!("asm: {} needs a register", mn)),
        };
        match mn {
            "idiv" => asm.idiv_r(r),
            "div" => asm.div_r(r),
            "neg" => asm.neg_r(r),
            "not" => asm.not_r(r),
            "inc" => asm.inc_r(r),
            _ => asm.dec_r(r),
        }
        return Ok(());
    }

    // ---- two-operand instructions ----
    const TWO_OP: &[&str] = &[
        "mov", "movzx", "lea", "add", "sub", "and", "or", "xor", "cmp", "test", "imul", "shl",
        "shr", "sar",
    ];
    if !TWO_OP.contains(&mn) {
        return Err(format!("asm: unknown instruction '{}'", mn));
    }

    let (a, b) = match (ops.first(), ops.get(1)) {
        (Some(a), Some(b)) => (a.clone(), b.clone()),
        _ => return Err(format!("asm: '{}' needs two operands", mn)),
    };

    match mn {
        "mov" => {
            match (&a, &b) {
                (Op::R(d, sz), Op::R(s, _)) => emit_mov_reg_reg(asm, *d, *s, *sz),
                (Op::R(d, sz), Op::I(v)) => emit_mov_reg_imm(asm, *d, *v, *sz),
                (Op::R(d, sz), Op::M(m, _)) => emit_load(asm, *d, m, *sz),
                (Op::M(m, sz), Op::R(s, _)) => emit_store(asm, m, *s, *sz),
                (Op::M(m, sz), Op::I(v)) => emit_imm_to_mem(asm, m, *sz, *v),
                _ => return Err("asm: unsupported mov operands".into()),
            }
            return Ok(());
        }
        "movzx" => {
            match (&a, &b) {
                (Op::R(d, _), Op::R(s, Sz::B1)) => asm.movzx_rr8(*d, *s),
                (Op::R(d, _), Op::M(m, _)) => emit_load(asm, *d, m, Sz::B1),
                _ => return Err("asm: movzx needs reg, reg8/mem8".into()),
            }
            return Ok(());
        }
        "lea" => {
            match (&a, &b) {
                (Op::R(d, sz), Op::M(Mem::Base { base, index, disp }, _)) => match index {
                    Some((i, sc)) => {
                        asm.emit_mem_sib(&[0x8D], *sz == Sz::B8, *d, *base, *i, *sc, *disp)
                    }
                    None => asm.emit_mem(&[0x8D], *sz == Sz::B8, *d, *base, *disp),
                },
                (Op::R(d, sz), Op::M(Mem::Data(off), _)) => {
                    asm.emit_rip(&[0x8D], *sz == Sz::B8, *d, *off)
                }
                _ => return Err("asm: lea needs reg, [mem]".into()),
            }
            return Ok(());
        }
        "add" | "sub" | "and" | "or" | "xor" | "cmp" => {
            let (digit, opcode): (u8, u8) = match mn {
                "add" => (0, 0x01),
                "sub" => (5, 0x29),
                "and" => (4, 0x21),
                "or" => (1, 0x09),
                "xor" => (6, 0x31),
                _ => (7, 0x39),
            };
            match (&a, &b) {
                (Op::R(d, sz), Op::R(s, _)) => {
                    let op = if *sz == Sz::B1 { opcode - 1 } else { opcode };
                    asm.emit_rr(&[op], *sz == Sz::B8, *s, *d);
                }
                (Op::R(d, sz), Op::I(v)) => {
                    if *sz == Sz::B1 {
                        asm.alu_imm8(digit, *d, *v);
                    } else {
                        asm.alu_imm(*sz == Sz::B8, digit, *d, *v);
                    }
                }
                (Op::R(d, sz), Op::M(m, _)) => {
                    let op = if *sz == Sz::B1 { opcode - 1 } else { opcode };
                    match m {
                        Mem::Data(off) => {
                            asm.emit_rip(&[op], *sz == Sz::B8, *d, *off);
                        }
                        Mem::Base { base, index, disp } => match index {
                            Some((i, sc)) => asm.emit_mem_sib(
                                &[op],
                                *sz == Sz::B8,
                                *d,
                                *base,
                                *i,
                                *sc,
                                *disp,
                            ),
                            None => asm.emit_mem(&[op], *sz == Sz::B8, *d, *base, *disp),
                        },
                    }
                }
                (Op::M(m, sz), Op::R(s, _)) => {
                    let op = if *sz == Sz::B1 { opcode - 1 } else { opcode };
                    match m {
                        Mem::Data(off) => {
                            asm.emit_rip(&[op], *sz == Sz::B8, *s, *off);
                        }
                        Mem::Base { base, index, disp } => match index {
                            Some((i, sc)) => {
                                asm.emit_mem_sib(&[op], *sz == Sz::B8, *s, *base, *i, *sc, *disp)
                            }
                            None => asm.emit_mem(&[op], *sz == Sz::B8, *s, *base, *disp),
                        },
                    }
                }
                (Op::M(m, sz), Op::I(v)) => emit_alu_mem_imm(asm, *sz, digit, m, *v),
                _ => return Err(format!("asm: unsupported {} operands", mn)),
            }
            return Ok(());
        }
        "test" => {
            match (&a, &b) {
                (Op::R(d, sz), Op::R(s, _)) => {
                    asm.emit_rr(&[0x85], *sz == Sz::B8, *s, *d);
                }
                (Op::R(d, sz), Op::I(v)) => asm.test_imm(*sz == Sz::B8, *d, *v),
                (Op::M(Mem::Base { base, index, disp }, sz), Op::I(v)) => match index {
                    Some((i, sc)) => {
                        asm.emit_mem_sib(&[0xF7], *sz == Sz::B8, 0, *base, *i, *sc, *disp);
                        asm.raw(&(*v as i32).to_le_bytes());
                    }
                    None => {
                        asm.emit_mem(&[0xF7], *sz == Sz::B8, 0, *base, *disp);
                        asm.raw(&(*v as i32).to_le_bytes());
                    }
                },
                (Op::M(Mem::Data(off), sz), Op::I(v)) => {
                    asm.emit_rip_after(None, *sz == Sz::B8, 0, *off, &[0xF7], 4);
                    asm.raw(&(*v as i32).to_le_bytes());
                }
                _ => return Err("asm: test needs reg/mem, reg/imm".into()),
            }
            return Ok(());
        }
        "imul" => {
            match (&a, &b) {
                (Op::R(d, _), Op::R(s, _)) => asm.imul_rr(*d, *s),
                (Op::R(d, sz), Op::I(v)) => asm.imul_imm(*sz == Sz::B8, *d, *v),
                (Op::R(d, _), Op::M(Mem::Base { base, index, disp }, _)) => match index {
                    Some((i, sc)) => {
                        asm.emit_mem_sib(&[0x0F, 0xAF], true, *d, *base, *i, *sc, *disp)
                    }
                    None => asm.emit_mem(&[0x0F, 0xAF], true, *d, *base, *disp),
                },
                _ => return Err("asm: imul needs reg, reg/imm/mem".into()),
            }
            return Ok(());
        }
        "shl" | "shr" | "sar" => {
            let digit = match mn {
                "shl" => 4,
                "shr" => 5,
                _ => 7,
            };
            match (&a, &b) {
                (Op::R(d, sz), Op::I(v)) => asm.shift_imm(*sz == Sz::B8, digit, *d, *v as u8),
                (Op::R(d, sz), Op::R(1, Sz::B1)) => asm.shift_cl(*sz == Sz::B8, digit, *d),
                _ => return Err(format!("asm: {} needs reg, imm/cl", mn)),
            }
            return Ok(());
        }
        _ => {}
    }

    Err(format!("asm: unknown instruction '{}'", mn))
}
