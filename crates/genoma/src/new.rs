//! `genoma new <directory>`: a shell to start from. A bar along the top
//! with its name and a clock that opens a popup, written with the kit; the
//! types where an editor finds them (`types/`, `.luaurc`); a README that
//! says how to run, check and type it. Nothing is written into a directory
//! that already holds something.

use std::path::Path;

const SHELL: &str = r#"--!strict
-- {name}: a desktop shell made with Genoma.
--
--   genoma .            runs it; saving a file reloads it
--   genoma check .      checks its Luau and its shaders
--   genoma types types  brings the types up to date after updating Genoma
--
-- What everything takes: Genoma's docs/reference.md.
local g = require("@genoma")
local kit = require("@genoma/kit")
local clock = require("@genoma/clock")
local vitals = require("@genoma/vitals")

local theme = kit.theme

-- Whether the clock's popup is open.
local open = g.signal(false)

local function two(n: number): string return string.format("%02d", n) end
local function percent(v: number): string return `{math.round(v * 100)}%` end

local function text(t: string | () -> string, faint: boolean?): any
    return g.text {
        text = t,
        size = function() return theme.get("text_size") end,
        color = function() return if faint then theme.get("faint") else theme.get("text") end,
    }
end

return g.surface {
    -- Along the top of every screen, keeping windows out of its strip.
    anchor = { "top", "left", "right" },
    outputs = "all",
    reserve = true,
    namespace = "{slug}",
    g.capsule {
        grow = 1,
        height = 40,
        direction = "row",
        align = "center",
        padding_x = 12,
        gap = 8,
        radius = 0,
        fill = function() return theme.get("background") end,
        text("{name}"),
        g.box { grow = 1 },
        kit.button {
            -- An id: what a test presses (genoma snapshot --click clock).
            id = "clock",
            kind = "flat",
            name = "Clock",
            text = function() return two(clock.hour) .. ":" .. two(clock.minute) end,
            on_click = function() open.value = not g.untracked(function() return open.value end) end,
            -- Opens below the clock while `open` says so; a press elsewhere
            -- closes it.
            g.popup {
                side = "bottom",
                gap = 6,
                present = function() return open.value end,
                on_dismiss = function() open.value = false end,
                g.capsule {
                    direction = "column",
                    gap = 6,
                    padding = 14,
                    radius = function() return theme.get("radius") end,
                    fill = function() return theme.get("background") end,
                    rim_top = function() return theme.get("line") end,
                    rim_bottom = function() return theme.get("line") end,
                    text(function() return "Processor " .. percent(vitals.cpu) end),
                    text(function() return "Memory " .. percent(vitals.ram) end),
                    text("Written in shell.luau", true),
                },
            },
        },
    },
}
"#;

const README: &str = r#"# {name}

A desktop shell made with [Genoma](https://github.com/AkusenArcade/Genoma).

| | |
|---|---|
| `genoma .` | run it; saving a file reloads it |
| `genoma check .` | check its Luau and its shaders, each mistake with its line |
| `genoma types types` | bring `types/` up to date after updating Genoma |
| `GENOMA_IPC={slug} genoma ipc genoma profile on` | what each surface costs, drawn over it |

`types/` holds Genoma's Luau types and its kit, for the editor and for
`luau-analyze` (`.luaurc` points `@genoma` at it); Genoma itself does not
read it. What every node, service and function takes is in Genoma's
`docs/reference.md`.
"#;

pub fn make(dir: &Path) -> Result<(), String> {
    if dir.exists() && std::fs::read_dir(dir).map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("{} already holds something", dir.display()));
    }
    let name = dir
        .canonicalize()
        .ok()
        .or_else(|| std::env::current_dir().ok().map(|c| c.join(dir)))
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "shell".into());
    let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let fill = |t: &str| t.replace("{name}", &name).replace("{slug}", &slug);
    let write = |file: &str, text: &str| std::fs::write(dir.join(file), text).map_err(|e| format!("{file}: {e}"));
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    write("shell.luau", &fill(SHELL))?;
    write("README.md", &fill(README))?;
    write(".luaurc", "{ \"languageMode\": \"strict\", \"aliases\": { \"genoma\": \"./types\" } }\n")?;
    crate::docs::write_types(&dir.join("types")).map_err(|e| format!("types: {e}"))?;
    println!("{name} is in {}: `genoma {}` runs it.", dir.display(), dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_new_shell_checks_clean_and_will_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("genoma-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        super::make(&dir.join("My Shell")).unwrap();
        let shell = std::fs::read_to_string(dir.join("My Shell/shell.luau")).unwrap();
        assert!(shell.contains("namespace = \"my-shell\"") && shell.contains("text(\"My Shell\")"));
        assert!(genoma_script::check_luau("shell.luau", &shell).is_ok());
        assert!(dir.join("My Shell/types/init.luau").exists() && dir.join("My Shell/types/kit/button.luau").exists());
        assert!(super::make(&dir.join("My Shell")).unwrap_err().contains("already holds"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
