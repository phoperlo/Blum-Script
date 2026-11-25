//! Source loading: `import "file.blm";` expansion and process execution.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn expand_imports(path: &Path) -> Result<String, String> {
    let mut seen = HashSet::new();
    expand(path, &mut seen, 0)
}

fn expand(path: &Path, seen: &mut HashSet<PathBuf>, depth: usize) -> Result<String, String> {
    if depth > 64 {
        return Err("import nesting is too deep".into());
    }
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !seen.insert(key) {
        return Ok(String::new());
    }
    let src = fs::read_to_string(path)
        .map_err(|e| format!("cannot read '{}': {}", path.display(), e))?;

    let mut out = String::new();
    for line in src.lines() {
        let t = line.trim();
        let mut it = t.split_whitespace();
        if it.next() == Some("import") {
            if let Some(raw) = it.next() {
                let name = raw.trim_end_matches(';').trim_matches('"');
                let base = path.parent().unwrap_or(Path::new("."));
                let mut target = base.join(name);
                if !target.exists() {
                    target.set_extension("blm");
                }
                out.push_str(&expand(&target, seen, depth + 1)?);
                out.push('\n');
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

pub fn run_exe(path: &Path) -> Result<i32, String> {
    let status = std::process::Command::new(path)
        .status()
        .map_err(|e| format!("cannot run '{}': {}", path.display(), e))?;
    Ok(status.code().unwrap_or(1))
}
