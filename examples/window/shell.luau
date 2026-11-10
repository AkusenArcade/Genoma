--!strict
-- Two windows the compositor manages as any program's: tiled or floating,
-- in its alt-tab, with the kit's title bar to move them.
--
-- A window with an `on_close` is only told when the compositor asks to
-- close it, and goes when its `present` lets it go: "Preferences" asks
-- first, "About" lets go at once (its title counts the presses). One with
-- no `on_close` closes itself, for good. Each opens again from the other,
-- or from outside:
--
--   genoma examples/window
--   genoma ipc window open        (GENOMA_IPC names the socket)
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme

local prefs_open = g.signal(true)
local about_open = g.signal(false)
local asking = g.signal(false)
local dark = g.signal(true)
local volume = g.signal(0.6)
local name = g.signal("")
local presses = g.signal(0)

local function row(label: string, control: any): any
    return g.box {
        direction = "row",
        align = "center",
        gap = 12,
        g.text { grow = 1, text = label, size = function() return theme.get("text_size") end, color = function() return theme.get("text") end },
        control,
    }
end

-- The window's whole area: the title bar, then its content, filling it.
local function frame(title: string | () -> string, on_close: () -> (), ...: any): any
    return g.capsule {
        grow = 1,
        direction = "column",
        align = "stretch",
        radius = 0,
        fill = function() return theme.get("background") end,
        kit.titlebar { title = title, on_close = on_close },
        g.box {
            grow = 1,
            direction = "column",
            align = "stretch",
            gap = 14,
            padding = 18,
            ...,
        },
    }
end

local prefs = g.window {
    title = "Preferences",
    app_id = "genoma.example.window",
    width = 460,
    height = 340,
    min = { 360, 280 },
    present = function() return prefs_open.value end,
    on_close = function() asking.value = true end,
    frame("Preferences", function() asking.value = true end,
        row("Dark look", kit.toggle { on = function() return dark.value end, on_toggle = function(on) dark.value = on end }),
        row("Volume", kit.slider { width = 180, value = function() return volume.value end, on_change = function(v) volume.value = v end }),
        row("Name", kit.field { width = 180, placeholder = "Anyone", text = function() return name.value end, on_change = function(t) name.value = t end }),
        g.box { grow = 1 },
        g.box {
            present = function() return not asking.value end,
            direction = "row",
            gap = 8,
            justify = "end",
            kit.button { text = "About…", on_click = function() about_open.value = true end },
            kit.button { text = "Close", kind = "accent", on_click = function() asking.value = true end },
        },
        g.box {
            present = function() return asking.value end,
            direction = "row",
            align = "center",
            gap = 8,
            g.text { grow = 1, text = "Close Preferences?", size = function() return theme.get("text_size") end, color = function() return theme.get("text") end },
            kit.button { text = "Keep", on_click = function() asking.value = false end },
            kit.button {
                text = "Close",
                kind = "danger",
                on_click = function()
                    asking.value = false
                    prefs_open.value = false
                end,
            },
        }
    ),
}

local about = g.window {
    title = function() return `About ({presses.value})` end,
    app_id = "genoma.example.about",
    width = 320,
    height = 200,
    present = function() return about_open.value end,
    on_close = function() about_open.value = false end,
    frame(function() return `About ({presses.value})` end, function() about_open.value = false end,
        g.text { text = "A window made by Genoma.", size = function() return theme.get("text_size") end, color = function() return theme.get("faint") end },
        g.box { grow = 1 },
        g.box {
            direction = "row",
            gap = 8,
            justify = "end",
            kit.button { text = "Press", on_click = function() presses.value += 1 end },
            kit.button { text = "Preferences", on_click = function() prefs_open.value = true end },
        }
    ),
}

g.ipc("window", {
    open = function(): string
        prefs_open.value = true
        return "open"
    end,
    about = function(): string
        about_open.value = true
        return "open"
    end,
})

return { prefs, about }
