--!strict
-- An SVG file as nodes (g.svg): each element with an id is a part to bind
-- and animate, drawn on the GPU. The gauge's needle turns with the
-- processor's load and its arc is drawn on as far as the memory in use;
-- currentColor follows the theme. A click morphs the shape beside it into
-- the next outline.
--
--   genoma examples/svg
local g = require("@genoma")
local kit = require("@genoma/kit")
local vitals = require("@genoma/vitals")

local theme = kit.theme

g.timing {
    needle = { duration = 0.6, curve = { 0.2, 0.0, 0.0, 1.0 } },
    morph = { duration = 0.5, curve = { 0.4, 0.0, 0.2, 1.0 } },
}

local shapes = {
    "M50 2 L61 38 L98 38 L68 60 L79 96 L50 74 L21 96 L32 60 L2 38 L39 38 Z",
    "M50 2 A48 48 0 1 1 49.9 2 Z",
    "M10 10 H90 V90 H10 Z",
}
local shape = g.signal(1)

return g.surface {
    anchor = "top",
    margin = { top = 120 },
    namespace = "genoma-svg",

    g.capsule {
        gap = 24,
        padding = 20,
        radius = 14,
        align = "center",
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,

        g.svg {
            src = "gauge.svg",
            width = 160,
            height = 120,
            color = function() return theme.get("text") end,
            parts = {
                -- From -90° (empty) to +90° (every core busy), about its hub.
                needle = {
                    rotate = function() return vitals.cpu * 180 - 90 end,
                    pivot_at = { 80, 100 },
                    transition = { rotate = "needle" },
                },
                level = {
                    trim_end = function() return vitals.ram end,
                    transition = { trim_end = "needle" },
                },
            },
        },

        g.path {
            width = 100,
            height = 100,
            d = function() return shapes[shape.value] end,
            fill = function() return theme.get("accent") end,
            transition = { d = "morph" },
            on_click = function()
                shape.value = shape.value % #shapes + 1
            end,
        },
    },
}
