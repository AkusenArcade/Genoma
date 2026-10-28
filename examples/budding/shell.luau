--!strict
-- Milestone 4: budding in every direction (PRD §8.2). Bioma's cells open by
-- budding (Akusen, 2026-10-28), and they open below a top membrane, above
-- the dock and beside a side cell. Click a cell: its panel leaves from the
-- edge that faces it, joined by a neck that narrows and parts; click again
-- and it is taken back.
local g = require("@genoma")
local theme = require("theme")

g.timing {
    bud_open = { duration = 0.45, curve = { 0.2, 0.9, 0.3, 1.0 } },
    bud_close = { duration = 0.30, curve = { 0.5, 0.0, 0.9, 0.5 } },
    -- The content enters once the shape has grown, and leaves first.
    content_open = { duration = 0.15, delay = 0.25, curve = { 0.2, 0.9, 0.3, 1.0 } },
    content_close = { duration = 0.08, curve = { 0.5, 0.0, 0.9, 0.5 } },
}

local W, H = 760, 600
local GAP = 14 -- the expansion's gap, as in Bioma
local CELL_W, CELL_H = 120, 40

type Side = "below" | "above" | "left" | "right"

-- Where the panel goes, by the side it leaves from: the attach, the axis it
-- travels on, and which way.
local SIDES = {
    below = { attach = "below-center", axis = "offset_y", sign = 1 },
    above = { attach = "above-center", axis = "offset_y", sign = -1 },
    left = { attach = "left", axis = "offset_x", sign = -1 },
    right = { attach = "right", axis = "offset_x", sign = 1 },
}

local function budding(side: Side, x: number, y: number, pw: number, ph: number)
    local open = g.state("budding." .. side, false)
    local s = SIDES[side]
    -- Inside the cell while closed, where nothing of it is drawn; out at
    -- the gap when open.
    local inside = -s.sign * 40
    local panel: { [any]: any } = {
        overlay = true,
        attach = s.attach,
        width = pw,
        height = ph,
        radius = 20,
        fill = theme.cellFill,
        rim_top = theme.rim,
        rim_bottom = theme.line,
        blur = true,
        growth = function() return if open.value then 1 else 0 end,
        -- The neck: soft and wide as it leaves, gone once it is out.
        bud = function() return if open.value then 0 else 36 end,
        transition = {
            [s.axis] = function() return if open.value then "bud_open" else "bud_close" end,
            growth = function() return if open.value then "bud_open" else "bud_close" end,
            bud = function() return if open.value then "bud_open" else "bud_close" end,
        },
        g.text {
            font = "Spectral",
            size = 15,
            color = theme.text,
            text = "budded " .. side,
            opacity = function() return if open.value then 1 else 0 end,
            transition = {
                opacity = function() return if open.value then "content_open" else "content_close" end,
            },
        },
    }
    panel[s.axis] = function() return if open.value then s.sign * GAP else inside end
    return g.capsule {
        role = "button",
        name = "Bud " .. side,
        overlay = true,
        width = CELL_W,
        height = CELL_H,
        fill = theme.cellFill,
        rim_top = theme.rim,
        rim_bottom = theme.line,
        blur = true,
        offset_x = x - W / 2,
        offset_y = y - H / 2,
        on_click = function() open.value = not open.value end,
        g.text { font = "Spectral", size = 15, color = theme.text, text = side },
        g.capsule(panel),
    }
end

return g.surface {
    anchor = "top",
    margin = { top = 200 },
    -- A floating surface as Bioma names one: niri's rule turns xray off.
    namespace = "bioma-float",

    g.box {
        width = W,
        height = H,
        justify = "center",
        -- As from a top membrane, from the dock, and from cells at the sides.
        budding("below", W / 2, 40, 220, 88),
        budding("above", W / 2, H - 40, 220, 88),
        budding("right", 90, H / 2, 200, 120),
        budding("left", W - 90, H / 2, 200, 120),
    },
}
