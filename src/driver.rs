//! Driver: import expansion and native compilation via MSVC/gcc/clang.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Recursively expand `import "file.blm";` lines.
pub fn expand_imports(path: &Path) -> Result<String, String> {
    let mut seen = HashSet::new();
    let mut out = String::new();
    expand_into(path, &mut seen, &mut out)?;
    Ok(out)
}

fn expand_into(path: &Path, seen: &mut HashSet<PathBuf>, out: &mut String) -> Result<(), String> {
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.contains(&canon) {
        return Ok(());
    }
    seen.insert(canon);

    let src = fs::read_to_string(path)
        .map_err(|e| format!("cannot read '{}': {}", path.display(), e))?;

    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("import") {
            // import "path";
            let rest = trimmed[6..].trim_start();
            if let Some(start) = rest.find('"') {
                if let Some(end) = rest[start + 1..].find('"') {
                    let rel = &rest[start + 1..start + 1 + end];
                    let base = path.parent().unwrap_or_else(|| Path::new("."));
                    let mut target = base.join(rel);
                    if target.extension().is_none() {
                        target.set_extension("blm");
                    }
                    expand_into(&target, seen, out)?;
                    continue;
                }
            }
            return Err(format!("malformed import in '{}': {}", path.display(), line));
        }
        out.push_str(line);
        out.push('\n');
    }
    Ok(())
}

/// Locate a usable C compiler. Returns a command template description.
fn find_vcvars() -> Option<String> {
    if let Ok(p) = std::env::var("BLUM_VCVARS") {
        if Path::new(&p).exists() {
            return Some(p);
        }
    }

    let vswhere = r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe";
    if Path::new(vswhere).exists() {
        if let Ok(out) = Command::new(vswhere)
            .args(["-latest", "-products", "*", "-property", "installationPath"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                let candidate = format!(r"{}\VC\Auxiliary\Build\vcvars64.bat", s);
                if Path::new(&candidate).exists() {
                    return Some(candidate);
                }
            }
        }
    }

    let editions = ["Community", "Professional", "Enterprise", "BuildTools"];
    let years = ["2022", "2019", "2017", "2015"];
    let roots = [
        r"C:\Program Files\Microsoft Visual Studio",
        r"C:\Program Files (x86)\Microsoft Visual Studio",
    ];
    for root in roots {
        for year in years {
            for ed in editions {
                let p = format!("{}\\{}\\{}\\VC\\Auxiliary\\Build\\vcvars64.bat", root, year, ed);
                if Path::new(&p).exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn find_gcc_like() -> Option<String> {
    for name in ["gcc", "clang", "cc"] {
        if Command::new(name).arg("--version").output().is_ok() {
            return Some(name.to_string());
        }
    }
    None
}

/// Compile generated C into a native executable.
pub fn compile_c(c_path: &Path, exe_path: &Path) -> Result<(), String> {
    if let Some(vcvars) = find_vcvars() {
        let cmdline = format!(
            "call \"{}\" >nul && cl /nologo /O2 /W3 /Fe:\"{}\" \"{}\"",
            vcvars,
            exe_path.display(),
            c_path.display()
        );
        let out = {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.raw_arg(&cmdline);
            }
            #[cfg(not(windows))]
            {
                cmd.arg(&cmdline);
            }
            cmd.output()
                .map_err(|e| format!("failed to launch cl.exe: {}", e))?
        };
        if !out.status.success() {
            return Err(format!(
                "MSVC compilation failed.\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        return Ok(());
    }

    if let Some(cc) = find_gcc_like() {
        let out = Command::new(&cc)
            .args(["-O2", "-std=c11", "-o"])
            .arg(exe_path)
            .arg(c_path)
            .output()
            .map_err(|e| format!("failed to launch {}: {}", cc, e))?;
        if !out.status.success() {
            return Err(format!(
                "{} compilation failed.\nstderr:\n{}",
                cc,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        return Ok(());
    }

    Err("no C compiler found: install MSVC (with vcvars64.bat) or gcc/clang".into())
}

pub fn run_exe(exe: &Path) -> Result<i32, String> {
    let status = Command::new(exe)
        .status()
        .map_err(|e| format!("failed to run '{}': {}", exe.display(), e))?;
    Ok(status.code().unwrap_or(-1))
}
