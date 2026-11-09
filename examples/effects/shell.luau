--!strict
-- The shader effects Genoma ships, each on a panel of its own: drifting
-- noise, rings spreading from a point, a colour dissolving as the slider
-- says, and a vignette and film grain laid over the noise.
--
--   genoma examples/effects
local g = require("@genoma")
local kit = require("@genoma/kit")

local effects = kit.effects
local theme = kit.theme
local gone = g.signal(0.35)

local function panel(title: string, body: any): any
    return g.box {
        direction = "column",
        gap = 8,
        g.capsule {
            width = 200,
            height = 140,
            radius = 10,
            clip = true,
            fill = "#0d0f14",
            rim_top = function() return theme.get("line") end,
            rim_bottom = function() return theme.get("line") end,
            body,
        },
        g.text { size = 12, color = function() return theme.get("faint") end, text = title },
    }
end

return g.surface {
    anchor = "top",
    margin = { top = 120 },
    namespace = "genoma-effects",

    g.capsule {
        direction = "column",
        align = "start",
        gap = 16,
        padding = 18,
        radius = 14,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,

        g.box {
            gap = 14,
            panel("noise", effects.noise { width = 200, height = 140, from = "#101a33", to = "#7aa2f7", size = 60, drift = 0.12, octaves = 5, contrast = 1.4 }),
            panel("ripple", effects.ripple { width = 200, height = 140, color = "#88c0d0", centre = { 0.5, 0.5 }, spacing = 18, speed = 0.6, reach = 110, ring = 0.25 }),
            panel("dissolve", effects.dissolve {
                width = 200,
                height = 140,
                color = "#e88aa8",
                edge_color = "#ffd27a",
                edge = 0.1,
                grain = 30,
                progress = function() return gone.value end,
            }),
        },
        g.box {
            gap = 14,
            panel("vignette over noise", g.box {
                width = 200,
                height = 140,
                effects.noise { overlay = true, attach = "corner", width = 200, height = 140, from = "#2a1d10", to = "#e6b062", size = 50, contrast = 1.2 },
                effects.vignette { overlay = true, attach = "corner", width = 200, height = 140, strength = 0.85, start = 0.3, softness = 0.7 },
            }),
            panel("spotlight (follows the pointer)", g.shader {
                width = 200,
                height = 140,
                src = g.dir .. "/spotlight.wgsl",
            }),
            panel("grain over noise", g.box {
                width = 200,
                height = 140,
                effects.noise { overlay = true, attach = "corner", width = 200, height = 140, from = "#132018", to = "#8fcf8a", size = 70, contrast = 1.2 },
                effects.grain { overlay = true, attach = "corner", width = 200, height = 140, amount = 0.18, size = 1.5, rate = 18 },
            }),
        },
        g.box {
            gap = 12,
            g.text { size = 12, color = function() return theme.get("faint") end, text = "Dissolved" },
            kit.slider { width = 260, name = "Dissolved", value = function() return gone.value end, on_change = function(v) gone.value = v end },
        },
    },
}
