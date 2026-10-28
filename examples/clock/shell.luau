--!strict
-- Milestone 1: a clock capsule, the size and voice of Bioma's.
local g = require("@genoma")
local clock = require("@genoma/clock")
local theme = require("theme")

-- The one place durations are written (Bioma's Timing).
g.timing {
    reflow = { duration = 0.22, curve = { 0.2, 0.0, 0.0, 1.0 } },
    theme = { duration = 0.25, curve = { 0.4, 0.0, 0.2, 1.0 } },
}

local function two(n: number): string
    return string.format("%02d", n)
end

return g.surface {
    anchor = "top",
    margin = { top = 72 },

    g.capsule {
        role = "timer",
        name = "Clock",
        height = 40,
        -- Orbitron's figures are proportional: a fixed width keeps the
        -- capsule still while the seconds change (Bioma: counters at a
        -- fixed width).
        min_width = 116,
        padding_x = 18,
        fill = theme.glass,
        rim_top = theme.rim,
        rim_bottom = theme.line,

        g.text {
            font = "Orbitron",
            size = 15,
            tabular = true,
            color = theme.text,
            text = function()
                return two(clock.hour) .. ":" .. two(clock.minute) .. ":" .. two(clock.second)
            end,
        },
    },
}
