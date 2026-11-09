--!strict
-- An on-screen display: when the default output's volume or mute changes,
-- a capsule with the level rises near the bottom of the screen and goes
-- after a moment. `genoma ipc osd show` shows it too (a keybind's own OSD).
--
--   genoma examples/osd
local g = require("@genoma")
local audio = require("@genoma/audio")
local kit = require("@genoma/kit")

local theme = kit.theme
g.timing_default { osd_in = { duration = 0.18, curve = { 0.2, 0, 0, 1 } }, osd_out = { duration = 0.3, curve = { 0.4, 0, 1, 1 } } }

local function sink(): any?
    for _, node in audio.nodes do
        if node.name == audio.default_sink then return node end
    end
    return nil
end

local shown = g.signal(false)
local hiding: any = nil
local function show()
    shown.value = true
    if hiding then hiding:stop() end
    hiding = g.after(1.5, function() hiding = nil; shown.value = false end)
end

-- What changes is told, not what is first read.
local first = true
g.watch(function()
    local s = sink()
    return if s then `{s.volume}:{s.muted}` else ""
end, function()
    if first then
        first = false
        return
    end
    show()
end)
g.ipc("osd", { show = function()
    show()
    return ""
end })

local function level(): number
    local s = sink()
    return if s then math.clamp(s.volume, 0, 1) else 0
end
local function muted(): boolean
    local s = sink()
    return s ~= nil and s.muted
end

return g.surface {
    anchor = "bottom",
    margin = { bottom = 80 },
    layer = "overlay",
    namespace = "genoma-osd",
    present = function() return shown.value end,

    g.capsule {
        gap = 12,
        padding_x = 16,
        height = 44,
        radius = 22,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,
        opacity = function() return if shown.value then 1 else 0 end,
        offset_y = function() return if shown.value then 0 else 12 end,
        transition = {
            opacity = function() return if shown.value then "osd_in" else "osd_out" end,
            offset_y = function() return if shown.value then "osd_in" else "osd_out" end,
        },
        g.text {
            size = 13,
            weight = 600,
            width = 64,
            color = function() return if muted() then theme.get("faint") else theme.get("text") end,
            text = function() return if muted() then "Muted" else "Volume" end,
        },
        kit.progress {
            width = 180,
            value = function() return if muted() then 0 else level() end,
        },
        g.text {
            size = 12,
            tabular = true,
            width = 36,
            color = function() return theme.get("faint") end,
            text = function() return `{math.floor(level() * 100 + 0.5)}%` end,
        },
    },
}
