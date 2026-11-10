--!strict
-- Charts from the kit, drawn on the GPU in one rectangle each: the CPU's
-- last two minutes as a line and as an area, refreshed every second, and
-- a week of something as bars.
--
--   genoma examples/charts
--
-- A chart is a g.shader on the built-in "genoma:chart": its numbers are
-- its values, so a history of a thousand points costs one node.
local g = require("@genoma")
local vitals = require("@genoma/vitals")
local chart = require("@genoma/kit/chart")
local theme = require("@genoma/kit/theme")

local HISTORY = 120

-- The CPU as a percentage, oldest first, one sample a second.
local cpu = g.signal({} :: { number })
local function sample()
    local list = table.clone(cpu.value)
    table.insert(list, vitals.cpu * 100)
    if #list > HISTORY then table.remove(list, 1) end
    cpu.value = list
end
sample()
g.timer { every = 1, on_tick = sample }

-- A wave, so the area has a shape before the CPU has a history.
local wave: { number } = {}
for i = 1, 200 do
    wave[i] = 50 + 30 * math.sin(i / 14) + 12 * math.sin(i / 3.3)
end

local function label(text: string): any
    return g.text { size = 12, weight = 600, color = function() return theme.get("faint") end, text = text }
end

return g.surface {
    anchor = { "top", "right" },
    margin = { 16, 16, 0, 0 },
    namespace = "genoma-charts",

    g.capsule {
        direction = "column",
        gap = 10,
        padding = 16,
        radius = 14,
        fill = function() return theme.get("background") end,

        label("CPU, last two minutes"),
        chart.line { width = 280, height = 48, values = function() return cpu.value end, min = 0, max = 100 },
        label("CPU as an area"),
        chart.area { width = 280, height = 48, values = function() return cpu.value end, min = 0, max = 100, fade = 0.9 },
        label("A wave, 200 points"),
        chart.area { width = 280, height = 64, values = wave, color = "#7aa2f7", fade = 0.6, line = 2 },
        label("A week"),
        chart.bars { width = 280, height = 64, values = { 3, 5, 2, 8, 6, 9, 4 }, color = "#9ece6a", gap = 0.3, radius = 3 },
    },
}
