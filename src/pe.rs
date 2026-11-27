//! Minimal PE32+ (Windows x64 console executable) writer.
//!
//! Lays out `.text`, `.rdata`, `.data` and `.idata`, resolves every relocation
//! produced by `crate::x64::Asm`, builds the kernel32 import table and writes a
//! ready-to-run `.exe`.

use crate::x64::{Asm, Fixup};
use std::fs;
use std::path::Path;

const SECT_ALIGN: u32 = 0x1000;
const FILE_ALIGN: u32 = 0x200;
const IMAGE_BASE: u64 = 0x1_4000_0000;
const HEADERS: u32 = 0x200;

fn align_up(v: u32, a: u32) -> u32 {
    if v == 0 {
        0
    } else {
        (v + a - 1) / a * a
    }
}

fn put_u16(buf: &mut [u8], at: usize, v: u16) {
    buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(buf: &mut [u8], at: usize, v: u64) {
    buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn section_header(
    name: &str,
    vsize: u32,
    rva: u32,
    raw_size: u32,
    raw_ptr: u32,
    chars: u32,
) -> [u8; 40] {
    let mut h = [0u8; 40];
    let nb = name.as_bytes();
    h[..nb.len().min(8)].copy_from_slice(&nb[..nb.len().min(8)]);
    put_u32(&mut h, 8, vsize);
    put_u32(&mut h, 12, rva);
    put_u32(&mut h, 16, raw_size);
    put_u32(&mut h, 20, raw_ptr);
    put_u32(&mut h, 36, chars);
    h
}

/// Write a complete executable to disk.
pub fn write_exe(
    path: &Path,
    asm: &Asm,
    rdata: &[u8],
    data: &[u8],
    imports: &[(&str, &str)],
    entry: u32,
) -> Result<(), String> {
    let image = link(asm, rdata, data, imports, entry)?;
    fs::write(path, &image).map_err(|e| format!("cannot write '{}': {}", path.display(), e))
}

/// Build the complete PE image in memory.
pub fn link(
    asm: &Asm,
    rdata: &[u8],
    data: &[u8],
    imports: &[(&str, &str)],
    entry: u32,
) -> Result<Vec<u8>, String> {
    if imports.is_empty() {
        return Err("internal: no imports".into());
    }
    let dll = imports[0].0;
    if imports.iter().any(|(d, _)| *d != dll) {
        return Err("internal: multiple DLLs not supported".into());
    }

    let n = imports.len() as u32;

    // ---- import section layout ----
    let mut off: u32 = 40; // descriptor + terminator
    let ilt_off = off;
    off += (n + 1) * 8;
    let iat_off = off;
    off += (n + 1) * 8;
    let mut hint_offs: Vec<u32> = Vec::new();
    for (_, name) in imports {
        hint_offs.push(off);
        off += 2 + name.len() as u32 + 1;
        if off % 2 == 1 {
            off += 1;
        }
    }
    let dll_off = off;
    off += dll.len() as u32 + 1;
    let idata_size = off;

    // ---- section RVAs ----
    let text_len = asm.code.len() as u32;
    let rdata_len = rdata.len() as u32;
    let data_len = data.len() as u32;

    let text_rva = SECT_ALIGN;
    let rdata_rva = text_rva + align_up(text_len, SECT_ALIGN);
    let data_rva = rdata_rva + align_up(rdata_len, SECT_ALIGN);
    let idata_rva = data_rva + align_up(data_len, SECT_ALIGN);

    // ---- build import table ----
    let mut idata = vec![0u8; idata_size as usize];
    put_u32(&mut idata, 0, idata_rva + ilt_off); // OriginalFirstThunk
    put_u32(&mut idata, 12, idata_rva + dll_off); // Name
    put_u32(&mut idata, 16, idata_rva + iat_off); // FirstThunk
    idata[dll_off as usize..dll_off as usize + dll.len()].copy_from_slice(dll.as_bytes());

    for (i, (_, name)) in imports.iter().enumerate() {
        let h = hint_offs[i];
        idata[h as usize..h as usize + 2].copy_from_slice(&0u16.to_le_bytes());
        let ns = h as usize + 2;
        idata[ns..ns + name.len()].copy_from_slice(name.as_bytes());
        let rva = (idata_rva + h) as u64;
        put_u64(&mut idata, (ilt_off + 8 * i as u32) as usize, rva);
        put_u64(&mut idata, (iat_off + 8 * i as u32) as usize, rva);
    }

    // ---- relocate code ----
    let mut code = asm.code.clone();
    for (at, fix) in &asm.fixups {
        let a = *at as usize;
        let here = text_rva as i64 + *at as i64 + 4;
        let disp: i64 = match fix {
            Fixup::Local(l) => {
                let target = asm.labels[*l as usize]
                    .ok_or_else(|| format!("internal: unbound label #{}", l))?;
                target as i64 - (*at as i64 + 4)
            }
            Fixup::Iat(i) => (idata_rva + iat_off + 8 * *i) as i64 - here,
            Fixup::Rdata(o) => (rdata_rva + *o) as i64 - here,
            Fixup::Data(o) => (data_rva + *o) as i64 - here,
            Fixup::DataAfter(o, n) => {
                (data_rva + *o) as i64 - (text_rva as i64 + *at as i64 + 4 + *n as i64)
            }
        };
        if disp < i32::MIN as i64 || disp > i32::MAX as i64 {
            return Err("internal: relocation out of range, image too large".into());
        }
        code[a..a + 4].copy_from_slice(&(disp as i32).to_le_bytes());
    }

    // ---- file layout ----
    let text_raw = align_up(text_len, FILE_ALIGN);
    let rdata_raw = align_up(rdata_len, FILE_ALIGN);
    let data_raw = align_up(data_len, FILE_ALIGN);
    let idata_raw = align_up(idata_size, FILE_ALIGN);

    let text_ptr = HEADERS;
    let rdata_ptr = text_ptr + text_raw;
    let data_ptr = rdata_ptr + rdata_raw;
    let idata_ptr = data_ptr + data_raw;

    let size_of_image = align_up(idata_rva + idata_size, SECT_ALIGN);
    let entry_rva = text_rva + entry;

    // ---- DOS + PE headers ----
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"MZ");
    out.resize(0x3C, 0);
    out.extend_from_slice(&0x40u32.to_le_bytes());
    out.resize(0x40, 0);
    out.extend_from_slice(b"PE\0\0");

    // COFF
    out.extend_from_slice(&0x8664u16.to_le_bytes()); // machine amd64
    out.extend_from_slice(&4u16.to_le_bytes()); // number of sections
    out.extend_from_slice(&0u32.to_le_bytes()); // timestamp
    out.extend_from_slice(&0u32.to_le_bytes()); // symbol table
    out.extend_from_slice(&0u32.to_le_bytes()); // symbol count
    out.extend_from_slice(&240u16.to_le_bytes()); // optional header size
    out.extend_from_slice(&0x0022u16.to_le_bytes()); // exe | large address aware

    // optional header (PE32+)
    let mut opt = vec![0u8; 240];
    put_u16(&mut opt, 0, 0x20B);
    opt[2] = 1; // linker major
    put_u32(&mut opt, 4, text_raw); // SizeOfCode
    put_u32(
        &mut opt,
        8,
        rdata_raw + data_raw + idata_raw, // SizeOfInitializedData
    );
    put_u32(&mut opt, 16, entry_rva);
    put_u32(&mut opt, 20, text_rva); // BaseOfCode
    put_u64(&mut opt, 24, IMAGE_BASE);
    put_u32(&mut opt, 32, SECT_ALIGN);
    put_u32(&mut opt, 36, FILE_ALIGN);
    put_u16(&mut opt, 40, 6); // OS major
    put_u16(&mut opt, 48, 6); // subsystem major
    put_u32(&mut opt, 56, size_of_image);
    put_u32(&mut opt, 60, HEADERS);
    put_u16(&mut opt, 68, 3); // console
    put_u16(&mut opt, 70, 0x0100); // NX compatible
    put_u64(&mut opt, 72, 0x10_0000); // stack reserve
    put_u64(&mut opt, 80, 0x1000); // stack commit
    put_u64(&mut opt, 88, 0x10_0000); // heap reserve
    put_u64(&mut opt, 96, 0x1000); // heap commit
    put_u32(&mut opt, 108, 16); // number of directories
    // directory[1] = import table
    put_u32(&mut opt, 112 + 8, idata_rva);
    put_u32(&mut opt, 112 + 12, 40);
    out.extend_from_slice(&opt);

    // section table
    out.extend_from_slice(&section_header(
        ".text",
        text_len,
        text_rva,
        text_raw,
        text_ptr,
        0x6000_0020,
    ));
    out.extend_from_slice(&section_header(
        ".rdata",
        rdata_len,
        rdata_rva,
        rdata_raw,
        rdata_ptr,
        0x4000_0040,
    ));
    out.extend_from_slice(&section_header(
        ".data",
        data_len,
        data_rva,
        data_raw,
        data_ptr,
        0xC000_0040,
    ));
    out.extend_from_slice(&section_header(
        ".idata",
        idata_size,
        idata_rva,
        idata_raw,
        idata_ptr,
        0xC000_0040,
    ));

    // ---- section bodies ----
    out.resize(HEADERS as usize, 0);
    out.extend_from_slice(&code);
    out.resize(text_ptr as usize + text_raw as usize, 0);
    out.extend_from_slice(rdata);
    out.resize(rdata_ptr as usize + rdata_raw as usize, 0);
    out.extend_from_slice(data);
    out.resize(data_ptr as usize + data_raw as usize, 0);
    out.extend_from_slice(&idata);
    out.resize(idata_ptr as usize + idata_raw as usize, 0);

    Ok(out)
}
