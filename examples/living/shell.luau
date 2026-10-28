--!strict
-- Milestone 4: the living layer, one primitive at a time (PRD §8).
-- Click the first capsule to link the two: the thread grows from it to the
-- second, nodes at both ends. Click the second to move it: it travels on a
-- spring, the thread's end with it, and the thread's middle on a softer one,
-- so the thread bends while the capsule moves and settles after it.
local g = require("@genoma")
local theme = require("theme")

g.timing {
    -- Cell.qml's grow, on easeOpenFlat; and Timing.close on easeClose.
    grow = { duration = 0.25, curve = { 0.2, 0.9, 0.3, 1.0 } },
    retract = { duration = 0.15, curve = { 0.5, 0.0, 0.9, 0.5 } },
    -- A capsule moving to a new place, and the thread's middle following
    -- it a little behind.
    travel = { spring = { stiffness = 220, damping = 22 } },
    sway = { spring = { stiffness = 90, damping = 9 } },
}

local W, H = 640, 160
local linked = g.state("living.linked", false)
local far = g.state("living.far", true)

local A_X = 110 -- the first capsule's centre, in the stage
local function bX(): number return if far.value then 520 else 330 end

local function cell(t: { [any]: any })
    t.height = 40
    t.width = 120
    t.fill = theme.cellFill
    t.rim_top = theme.rim
    t.rim_bottom = theme.line
    t.blur = true
    t.overlay = true
    return g.capsule(t)
end

return g.surface {
    anchor = "top",
    margin = { top = 300 },
    -- A floating surface as Bioma names one: niri's rule turns xray off.
    namespace = "bioma-float",

    g.box {
        width = W,
        height = H,
        justify = "center",
        cell {
            role = "button",
            name = "Link",
            offset_x = A_X - W / 2,
            on_click = function() linked.value = not linked.value end,
            g.text { font = "Spectral", size = 15, color = theme.text, text = function() return if linked.value then "linked" else "link" end },
        },
        cell {
            role = "button",
            name = "Move",
            offset_x = function() return bX() - W / 2 end,
            transition = { offset_x = "travel" },
            on_click = function() far.value = not far.value end,
            g.text { font = "Spectral", size = 15, color = theme.text, text = "move" },
        },
        -- The thread last, so its nodes show whole over the capsules' rims.
        g.thread {
            overlay = true,
            width = W,
            height = H,
            stroke = 1.3,
            crisp = true,
            fill = theme.line,
            color = theme.node,
            node = 5,
            from_x = A_X + 60,
            from_y = H / 2,
            to_x = function() return bX() - 60 end,
            to_y = H / 2,
            bend_x = function() return (A_X + 60 + bX() - 60) / 2 end,
            bend_y = H / 2 + 36,
            growth = function() return if linked.value then 1 else 0 end,
            transition = {
                growth = function() return if linked.value then "grow" else "retract" end,
                to_x = "travel",
                bend_x = "sway",
            },
        },
    },
}
