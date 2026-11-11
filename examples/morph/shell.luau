--!strict
-- Shared transitions: a chip and a panel with the same `morph` name are
-- one thing in two states. Press a chip and it opens into its panel, the
-- shape growing from the chip's place and roundness to the panel's, the
-- chip fading out inside it and the panel's content coming in; press the
-- panel and it closes back into its chip.
--
--   genoma examples/morph
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme

-- The morph's own timing: a little longer than a reflow, easing out.
g.timing {
    morph = { duration = 0.45, curve = { 0.2, 0.0, 0.0, 1.0 } },
}

local cards = {
    { name = "Weather", colour = "#7aa2f7", says = "Light rain from four, clearing by the evening. 14° now, 9° tonight." },
    { name = "Music", colour = "#bb9af7", says = "Nothing is playing. The last album was left at its seventh track." },
    { name = "Mail", colour = "#9ece6a", says = "Three new messages, one of them flagged, all from this morning." },
}

local open = g.signal(0)

local function chip(i: number, card: { [string]: any }): any
    return g.capsule {
        id = "chip-" .. i,
        morph = "card-" .. i,
        present = function() return open.value ~= i end,
        height = 34,
        padding_x = 16,
        fill = card.colour,
        on_click = function() open.value = i end,
        g.text { size = 14, weight = 600, color = "#11131a", text = card.name },
    }
end

local function panel(i: number, card: { [string]: any }): any
    return g.capsule {
        id = "panel-" .. i,
        morph = "card-" .. i,
        present = function() return open.value == i end,
        direction = "column",
        align = "start",
        gap = 10,
        width = 420,
        padding = 20,
        radius = 18,
        fill = card.colour,
        on_click = function() open.value = 0 end,
        g.text { size = 22, weight = 700, color = "#11131a", text = card.name },
        g.text { size = 14, color = "#11131a", max_width = 380, wrap = true, text = card.says },
    }
end

local chips = {}
local panels = {}
for i, card in cards do
    table.insert(chips, chip(i, card))
    table.insert(panels, panel(i, card))
end

return g.surface {
    anchor = "top",
    margin = { top = 120 },
    namespace = "genoma-morph",

    g.capsule {
        direction = "column",
        align = "start",
        gap = 16,
        padding = 18,
        radius = 14,
        min_width = 456,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,
        g.box { gap = 10, table.unpack(chips) },
        g.box { direction = "column", table.unpack(panels) },
    },
}
