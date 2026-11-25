#![allow(dead_code)]
//! Fixed ABI constants shared by the code generator and the runtime.

/// Offset in `.data` of the cached stdout handle.
pub const D_STDOUT: u32 = 0;
/// One-byte scratch buffer used when printing a `char`.
pub const D_CHARBUF: u32 = 8;
/// First offset available for user globals.
pub const D_GLOBALS: u32 = 64;

pub const I_GETSTDHANDLE: u32 = 0;
pub const I_WRITEFILE: u32 = 1;
pub const I_EXITPROCESS: u32 = 2;
pub const I_GETPROCESSHEAP: u32 = 3;
pub const I_HEAPALLOC: u32 = 4;
pub const I_HEAPFREE: u32 = 5;

pub const IMPORTS: &[(&str, &str)] = &[
    ("kernel32.dll", "GetStdHandle"),
    ("kernel32.dll", "WriteFile"),
    ("kernel32.dll", "ExitProcess"),
    ("kernel32.dll", "GetProcessHeap"),
    ("kernel32.dll", "HeapAlloc"),
    ("kernel32.dll", "HeapFree"),
];

/// Size of the runtime-managed prefix in `.data`.
pub const DATA_HEAD: usize = 64;
