--!strict
-- blur_radius: a node and everything in it drawn blurred, after depth has
-- turned it. The slider sets the spread (logical units, as CSS's blur());
-- at 0 nothing is done, and a spread at rest is blurred once and kept.
--
--   genoma examples/blur
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme
local spread = g.signal(4)
local tilt = g.signal(35)

local function card(title: string, colour: string): any
    return g.capsule {
        direction = "column",
        align = "start",
        gap = 6,
        padding = 14,
        radius = 12,
        width = 150,
        fill = colour,
        rim_top = "#ffffff40",
        rim_bottom = "#00000000",
        g.text { size = 16, weight = 600, color = "#10131a", text = title },
        g.text { size = 12, color = "#10131acc", text = "Text, shapes and depth, blurred together." , wrap = true, max_width = 122 },
    }
end

return g.surface {
    anchor = "top",
    margin = { top = 120 },
    namespace = "genoma-blur",

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
            gap = 18,
            padding = 10,
            blur_radius = function() return spread.value end,
            card("Flat", "#88c0d0"),
            g.box {
                rotate_y = function() return tilt.value end,
                perspective = 600,
                card("Turned", "#e6b062"),
            },
            g.capsule {
                width = 70,
                height = 70,
                fill = "#e88aa8",
                rim_top = "#00000000",
                rim_bottom = "#00000000",
            },
        },

        g.box {
            gap = 12,
            g.text { size = 12, width = 60, color = function() return theme.get("faint") end, text = function() return `blur {spread.value // 0.5 * 0.5}` end },
            kit.slider { width = 260, value = function() return spread.value / 20 end, on_change = function(v) spread.value = v * 20 end },
        },
    },
}
