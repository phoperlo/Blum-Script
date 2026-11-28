//! BlumScript compiler CLI (`blumsc`).
//!
//! The compiler emits x86-64 machine code directly and links a PE32+
//! executable itself: no C backend, no assembler, no LLVM, no linker.

mod abi;
mod ast;
mod imports;
mod inline_asm;
mod ir;
mod ircg;
mod irgen;
mod lexer;
mod native;
mod opt;
mod regalloc;
mod parser;
mod pe;
mod runtime;
mod types;
mod x64;

use std::env;
use std::path::{Path, PathBuf};
use std::process;

fn usage() {
    eprintln!(
        r#"BlumScript compiler (blumsc) {}

USAGE:
  blumsc build <file.blm> [-o output.exe]
  blumsc run   <file.blm>
  blumsc check <file.blm>
  blumsc tokens <file.blm>
  blumsc ast    <file.blm>
  blumsc hex    <file.blm>

OPTIONS:
  -o <path>        output executable path (build)
  --emit-bytes     print the emitted machine code as hex
  --no-ir          compile function bodies through the legacy backend
  --emit-ir        print the optimised IR and register allocation
  -h, --help       show this help
  -V, --version    show version

The backend writes x86-64 machine code and a PE32+ image directly.
Function bodies are lowered through a three-address IR that is optimised
and register-allocated before emission; --no-ir selects the older direct
AST-to-x64 path for comparison.
No external compiler, linker or runtime is required."#,
        env!("CARGO_PKG_VERSION")
    );
}

fn parse_source(path: &Path) -> Result<Vec<ast::Stmt>, String> {
    let src = imports::expand_imports(path)?;
    let toks = lexer::lex(&src)?;
    let mut p = parser::Parser::new(toks);
    p.parse_program()
}

fn compile(input: &Path, opts: native::Options) -> Result<native::Output, String> {
    let program = parse_source(input)?;
    native::compile_with(&program, opts)
}

fn default_exe(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    p.set_extension("exe");
    p
}

fn hexdump(bytes: &[u8]) {
    for (i, chunk) in bytes.chunks(16).enumerate() {
        let mut line = format!("{:08x}  ", i * 16);
        for b in chunk {
            line.push_str(&format!("{:02x} ", b));
        }
        for _ in chunk.len()..16 {
            line.push_str("   ");
        }
        line.push(' ');
        for b in chunk {
            let c = *b;
            line.push(if (0x20..0x7f).contains(&c) { c as char } else { '.' });
        }
        println!("{}", line);
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
        process::exit(1);
    }

    let cmd = args[1].as_str();
    match cmd {
        "-h" | "--help" | "help" => {
            usage();
            return;
        }
        "-V" | "--version" | "version" => {
            println!("blumsc {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        _ => {}
    }

    if args.len() < 3 {
        eprintln!("error: missing input file");
        usage();
        process::exit(1);
    }

    let input = PathBuf::from(&args[2]);
    if !input.exists() {
        eprintln!("error: file not found: {}", input.display());
        process::exit(1);
    }

    let mut output: Option<PathBuf> = None;
    let mut emit_bytes = cmd == "hex";
    let mut opts = native::Options::default();
    let mut ir_flag: Option<bool> = None;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                if i + 1 >= args.len() {
                    eprintln!("error: -o requires a path");
                    process::exit(1);
                }
                output = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--emit-bytes" | "--emit-asm" => {
                emit_bytes = true;
                i += 1;
            }
            "--no-ir" => {
                ir_flag = Some(false);
                i += 1;
            }
            "--emit-ir" => {
                ir_flag = Some(true);
                opts.dump_ir = true;
                i += 1;
            }
            other => {
                eprintln!("error: unknown option '{}'", other);
                process::exit(1);
            }
        }
    }
    if let Some(v) = ir_flag {
        opts.use_ir = v;
    }

    match cmd {
        "tokens" => match imports::expand_imports(&input).and_then(|s| lexer::lex(&s)) {
            Ok(toks) => {
                for t in toks {
                    println!("{:?}", t);
                }
            }
            Err(e) => {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        },
        "ast" => match parse_source(&input) {
            Ok(program) => println!("{:#?}", program),
            Err(e) => {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        },
        "check" => match compile(&input, opts) {
            Ok(_) => println!("ok: {}", input.display()),
            Err(e) => {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        },
        "build" | "run" | "hex" => {
            let out = match compile(&input, opts) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("error: {}", e);
                    process::exit(1);
                }
            };

            let exe = output.unwrap_or_else(|| default_exe(&input));

            if let Err(e) = pe::write_exe(&exe, &out.asm, &out.rdata, &out.data, abi::IMPORTS, out.entry)
            {
                eprintln!("error: {}", e);
                process::exit(1);
            }

            if emit_bytes {
                if let Ok(img) = pe::link(&out.asm, &out.rdata, &out.data, abi::IMPORTS, out.entry) {
                    let n = out.asm.code.len();
                    let start = 0x200;
                    let end = (start + n).min(img.len());
                    eprintln!(
                        "; .text  {} bytes  entry +0x{:x}  rdata {}  data {}",
                        n,
                        out.entry,
                        out.rdata.len(),
                        out.data.len()
                    );
                    hexdump(&img[start..end]);
                }
            }

            println!("built: {}", exe.display());

            if cmd == "run" {
                match imports::run_exe(&exe) {
                    Ok(code) => process::exit(code),
                    Err(e) => {
                        eprintln!("error: {}", e);
                        process::exit(1);
                    }
                }
            }
        }
        other => {
            eprintln!("error: unknown command '{}'", other);
            usage();
            process::exit(1);
        }
    }
}
