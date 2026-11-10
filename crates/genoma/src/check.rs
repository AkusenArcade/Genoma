//! `genoma check [directory]`: an interface's files checked without running
//! it. Every `.wgsl` as the device will compile it for a `g.shader` (with
//! Genoma's prelude and library), every `.luau` by the Luau compiler; each
//! mistake told with its file and line. The exit status is 1 when anything
//! is wrong, for a hook or CI. Types are luau-analyze's (`genoma types`).

use std::path::{Path, PathBuf};

/// Directories that hold no interface: builds, history, caches.
const SKIPPED: &[&str] = &["target", ".git", "node_modules", "graphify-out"];

pub fn run(dir: Option<&Path>) -> i32 {
    let root = dir.unwrap_or(Path::new("."));
    let mut files = Vec::new();
    walk(root, &mut files);
    files.sort();
    let mut problems = 0;
    let mut checked = 0;
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let shown = path.strip_prefix(root).unwrap_or(path).display().to_string();
        let result = match path.extension().and_then(|e| e.to_str()) {
            Some("wgsl") => genoma_render::shader::check(&text),
            Some("luau") => genoma_script::check_luau(&shown, &text),
            _ => continue,
        };
        checked += 1;
        if let Err(said) = result {
            problems += 1;
            println!("{shown}: {}", said.trim_end());
        }
    }
    println!("{checked} files checked, {problems} with a mistake");
    i32::from(problems > 0)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !SKIPPED.contains(&name.as_ref()) {
                walk(&path, out);
            }
        } else if name.ends_with(".wgsl") || name.ends_with(".luau") {
            out.push(path);
        }
    }
}
