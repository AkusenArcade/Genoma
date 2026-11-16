//! `genoma module`: shared modules from git, each pinned to a commit in the
//! interface's `modules.json` and checked out under its `modules/`, where
//! `require("@name")` finds it first.
//!
//! The file is what a shell commits; `genoma module install` brings every
//! module back to its pinned commit on another machine.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::{Map, Value, json};

const USAGE: &str = "genoma module add <name> <git-url> [rev] | install | list | remove <name>   [--dir <interface>]";

pub fn run(args: Vec<String>) -> i32 {
    let mut dir = PathBuf::from(".");
    let mut words = Vec::new();
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        if a == "--dir" {
            match args.next() {
                Some(d) => dir = PathBuf::from(d),
                None => return fail("--dir needs a directory"),
            }
        } else {
            words.push(a);
        }
    }
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        ["add", name, url] => add(&dir, name, url, None),
        ["add", name, url, rev] => add(&dir, name, url, Some(rev)),
        ["install"] => install(&dir),
        ["list"] => list(&dir),
        ["remove", name] => remove(&dir, name),
        _ => Err(format!("usage: {USAGE}")),
    };
    match result {
        Ok(()) => 0,
        Err(e) => fail(&e),
    }
}

fn fail(message: &str) -> i32 {
    eprintln!("genoma module: {message}");
    1
}

/// A name `require("@name")` can take: lower-case letters, digits, `-`
/// and `_`; "genoma" is the toolkit's own.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "genoma" && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("modules.json")
}

fn read_manifest(dir: &Path) -> Result<Map<String, Value>, String> {
    let path = manifest_path(dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Err(format!("{}: not an object", path.display())),
            Err(e) => Err(format!("{}: {e}", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn write_manifest(dir: &Path, map: &Map<String, Value>) -> Result<(), String> {
    let path = manifest_path(dir);
    let text = serde_json::to_string_pretty(&Value::Object(map.clone())).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Runs git, its output trimmed, or what it said when it failed.
fn git(at: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    if let Some(at) = at {
        command.arg("-C").arg(at);
    }
    command.args(["-c", "advice.detachedHead=false"]).args(args);
    let out = command.output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Clones `url` into `target` at `rev` (a tag, a branch, a commit; the
/// default branch when none), and answers the commit it stands on.
fn fetch_at(url: &str, rev: Option<&str>, target: &Path) -> Result<String, String> {
    if target.exists() {
        std::fs::remove_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    git(None, &["clone", "--quiet", url, &target.to_string_lossy()])?;
    if let Some(rev) = rev {
        git(Some(target), &["checkout", "--quiet", rev])?;
    }
    git(Some(target), &["rev-parse", "HEAD"])
}

fn add(dir: &Path, name: &str, url: &str, rev: Option<&str>) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("{name}: a module's name is lower-case letters, digits, - and _ (and not genoma)"));
    }
    let modules = dir.join("modules");
    std::fs::create_dir_all(&modules).map_err(|e| format!("{}: {e}", modules.display()))?;
    // Fetched beside, then moved in: a failed fetch leaves the module
    // that was there.
    let fresh = modules.join(format!(".{name}.new"));
    let commit = fetch_at(url, rev, &fresh).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&fresh);
    })?;
    if !fresh.join("init.luau").is_file() {
        eprintln!("genoma module: {name} has no init.luau: only require(\"@{name}/<part>\") will find it");
    }
    let target = modules.join(name);
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    std::fs::rename(&fresh, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    let mut manifest = read_manifest(dir)?;
    manifest.insert(name.to_owned(), json!({ "git": url, "rev": rev, "commit": commit }));
    write_manifest(dir, &manifest)?;
    alias(dir, name, true);
    println!("{name} {} at {}", rev.unwrap_or("default branch"), &commit[..commit.len().min(12)]);
    Ok(())
}

/// The module named in `.luaurc`'s aliases, where `genoma new` made one,
/// so the editor and luau-analyze resolve `@name` as Genoma does.
fn alias(dir: &Path, name: &str, on: bool) {
    let path = dir.join(".luaurc");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(Value::Object(mut config)) = serde_json::from_str::<Value>(&text) else {
        eprintln!("genoma module: {} is not plain JSON, the alias for {name} is left to you", path.display());
        return;
    };
    let aliases = config.entry("aliases").or_insert_with(|| json!({}));
    let Some(aliases) = aliases.as_object_mut() else { return };
    if on {
        aliases.insert(name.to_owned(), json!(format!("./modules/{name}")));
    } else {
        aliases.remove(name);
    }
    if let Ok(text) = serde_json::to_string_pretty(&Value::Object(config)) {
        let _ = std::fs::write(&path, text + "\n");
    }
}

fn entry(name: &str, value: &Value) -> Result<(String, Option<String>, String), String> {
    let get = |k: &str| value.get(k).and_then(Value::as_str).map(str::to_owned);
    match (get("git"), get("commit")) {
        (Some(url), Some(commit)) => Ok((url, get("rev"), commit)),
        _ => Err(format!("modules.json: {name} needs git and commit")),
    }
}

/// Every module of `modules.json` at its pinned commit.
fn install(dir: &Path) -> Result<(), String> {
    let manifest = read_manifest(dir)?;
    if manifest.is_empty() {
        println!("no modules in {}", manifest_path(dir).display());
    }
    for (name, value) in &manifest {
        let (url, _, commit) = entry(name, value)?;
        let target = dir.join("modules").join(name);
        let at = target.is_dir().then(|| git(Some(&target), &["rev-parse", "HEAD"]).ok()).flatten();
        if at.as_deref() == Some(commit.as_str()) {
            println!("{name} already at {}", &commit[..12.min(commit.len())]);
            continue;
        }
        if target.is_dir() {
            git(Some(&target), &["fetch", "--quiet", "origin"])?;
            git(Some(&target), &["checkout", "--quiet", &commit])?;
        } else {
            fetch_at(&url, Some(&commit), &target)?;
        }
        println!("{name} at {}", &commit[..12.min(commit.len())]);
    }
    Ok(())
}

fn list(dir: &Path) -> Result<(), String> {
    for (name, value) in &read_manifest(dir)? {
        let (url, rev, commit) = entry(name, value)?;
        let target = dir.join("modules").join(name);
        let state = if !target.is_dir() {
            "missing: genoma module install".to_owned()
        } else {
            match git(Some(&target), &["rev-parse", "HEAD"]) {
                Ok(at) if at != commit => format!("at {}, not the pinned commit", &at[..12.min(at.len())]),
                Ok(_) if !git(Some(&target), &["status", "--porcelain"]).unwrap_or_default().is_empty() => "changed by hand".into(),
                Ok(_) => "ok".into(),
                Err(e) => e,
            }
        };
        println!("{name}\t{}\t{}\t{url}\t{state}", rev.as_deref().unwrap_or("-"), &commit[..12.min(commit.len())]);
    }
    Ok(())
}

fn remove(dir: &Path, name: &str) -> Result<(), String> {
    let mut manifest = read_manifest(dir)?;
    let known = manifest.remove(name).is_some();
    let target = dir.join("modules").join(name);
    let present = target.is_dir();
    if !known && !present {
        return Err(format!("no module {name}"));
    }
    if present {
        std::fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    alias(dir, name, false);
    write_manifest(dir, &manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(at: &Path) {
        std::fs::create_dir_all(at).unwrap();
        let g = |args: &[&str]| git(Some(at), args).unwrap();
        g(&["init", "--quiet", "-b", "main"]);
        g(&["config", "user.email", "test@example.org"]);
        g(&["config", "user.name", "test"]);
        std::fs::write(at.join("init.luau"), "return { version = 1 }").unwrap();
        g(&["add", "."]);
        g(&["commit", "--quiet", "-m", "one"]);
        g(&["tag", "v1"]);
        std::fs::write(at.join("init.luau"), "return { version = 2 }").unwrap();
        g(&["commit", "--quiet", "-am", "two"]);
    }

    #[test]
    fn pinned_and_brought_back() {
        let base = std::env::temp_dir().join(format!("genoma-test-module-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (source, shell) = (base.join("source"), base.join("shell"));
        repo(&source);
        std::fs::create_dir_all(&shell).unwrap();
        std::fs::write(shell.join(".luaurc"), "{ \"languageMode\": \"strict\", \"aliases\": { \"genoma\": \"./types\" } }").unwrap();
        let url = format!("file://{}", source.display());

        add(&shell, "dial", &url, Some("v1")).unwrap();
        let init = shell.join("modules/dial/init.luau");
        assert_eq!(std::fs::read_to_string(&init).unwrap(), "return { version = 1 }");
        let manifest = read_manifest(&shell).unwrap();
        let (_, rev, commit) = entry("dial", &manifest["dial"]).unwrap();
        assert_eq!(rev.as_deref(), Some("v1"));
        let luaurc = || serde_json::from_str::<Value>(&std::fs::read_to_string(shell.join(".luaurc")).unwrap()).unwrap();
        assert_eq!(luaurc()["aliases"]["dial"], "./modules/dial");
        assert_eq!(luaurc()["aliases"]["genoma"], "./types", "the rest kept");

        // Gone, then brought back to the same commit.
        std::fs::remove_dir_all(shell.join("modules/dial")).unwrap();
        install(&shell).unwrap();
        assert_eq!(git(Some(&shell.join("modules/dial")), &["rev-parse", "HEAD"]).unwrap(), commit);

        // Pinned again, to the branch's head.
        add(&shell, "dial", &url, None).unwrap();
        assert_eq!(std::fs::read_to_string(&init).unwrap(), "return { version = 2 }");

        assert!(add(&shell, "Bad Name", &url, None).is_err());
        assert!(add(&shell, "genoma", &url, None).is_err());
        assert!(add(&shell, "ghost", &url, Some("no-such-tag")).is_err());
        assert!(!shell.join("modules/.ghost.new").exists(), "a failed fetch leaves nothing behind");

        remove(&shell, "dial").unwrap();
        assert!(!shell.join("modules/dial").exists());
        assert!(luaurc()["aliases"].get("dial").is_none());
        assert!(read_manifest(&shell).unwrap().is_empty());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
