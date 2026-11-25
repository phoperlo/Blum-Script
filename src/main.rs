//! BlumScript compiler CLI (`blumsc`).

mod ast;
mod codegen;
mod driver;
mod lexer;
mod parser;
mod types;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

fn usage() {
    eprintln!(
        r#"BlumScript compiler (blumsc) v0.2.0

USAGE:
  blumsc build <file.blm> [-o output.exe] [--emit-c]
  blumsc run   <file.blm> [--emit-c]
  blumsc check <file.blm>
  blumsc tokens <file.blm>
  blumsc ast    <file.blm>

OPTIONS:
  -o <path>     output executable path (build)
  --emit-c      print generated C code to stdout
  -h, --help    show this help
  -V, --version show version
"#
    );
}

fn parse_source(path: &Path) -> Result<Vec<ast::Stmt>, String> {
    let src = driver::expand_imports(path)?;
    let toks = lexer::lex(&src)?;
    let mut p = parser::Parser::new(toks);
    p.parse_program()
}

fn compile_to_c(path: &Path) -> Result<(String, Vec<ast::Stmt>), String> {
    let program = parse_source(path)?;
    let mut cg = codegen::Codegen::new();
    let c = cg.generate(&program)?;
    Ok((c, program))
}

fn default_exe(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    p.set_extension("exe");
    p
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
            println!("blumsc 0.2.0");
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
    let mut emit_c = false;
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
            "--emit-c" => {
                emit_c = true;
                i += 1;
            }
            other => {
                eprintln!("error: unknown option '{}'", other);
                process::exit(1);
            }
        }
    }

    match cmd {
        "tokens" => match driver::expand_imports(&input).and_then(|s| lexer::lex(&s)) {
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
        "check" => match compile_to_c(&input) {
            Ok(_) => println!("ok: {}", input.display()),
            Err(e) => {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        },
        "build" | "run" => {
            let (c, _program) = match compile_to_c(&input) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("error: {}", e);
                    process::exit(1);
                }
            };

            if emit_c {
                print!("{}", c);
            }

            let exe = output.unwrap_or_else(|| default_exe(&input));
            let c_path = exe.with_extension("c");

            if let Err(e) = fs::write(&c_path, &c) {
                eprintln!("error: cannot write '{}': {}", c_path.display(), e);
                process::exit(1);
            }

            if let Err(e) = driver::compile_c(&c_path, &exe) {
                eprintln!("error: {}", e);
                process::exit(1);
            }

            println!("built: {}", exe.display());

            if cmd == "run" {
                match driver::run_exe(&exe) {
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
