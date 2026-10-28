--!strict
-- Milestone 4: the living layer, one primitive at a time (PRD §8).
-- Click the first capsule to link the two: the thread grows from it to the
-- second, nodes at both ends. Click the second to move it: it travels on a
-- spring, the thread's end with it, and the thread's middle on a softer one,
-- so the thread bends while the capsule moves and settles after it.
local g = require("@genoma")
local theme = require("theme")
local vitals = require("@genoma/vitals")

g.timing {
    -- Cell.qml's grow, on easeOpenFlat; and Timing.close on easeClose.
    grow = { duration = 0.25, curve = { 0.2, 0.9, 0.3, 1.0 } },
    retract = { duration = 0.15, curve = { 0.5, 0.0, 0.9, 0.5 } },
    -- A capsule moving to a new place, and the thread's middle following
    -- it a little behind.
    travel = { spring = { stiffness = 220, damping = 22 } },
    sway = { spring = { stiffness = 90, damping = 9 } },
    -- Budding: the panel leaves its cell as a cell divides, the neck
    -- narrowing to nothing as it goes; reabsorption runs back, faster.
    bud_open = { duration = 0.45, curve = { 0.2, 0.9, 0.3, 1.0 } },
    bud_close = { duration = 0.30, curve = { 0.5, 0.0, 0.9, 0.5 } },
    instant = { duration = 0 },
    thread_hold = { duration = 0.35, curve = { 1.0, 0.0, 1.0, 0.0 } },
    -- A panel unfolding from its top edge, and folding back, on springs.
    unfold = { spring = { stiffness = 170, damping = 16 } },
    fold = { spring = { stiffness = 260, damping = 28 } },
}

local W, H = 640, 330
local linked = g.state("living.linked", false)
local budded = g.state("living.budded", false)
local unfolded = g.state("living.unfolded", false)
local ROW = 80 -- the link row's centre; the bud row sits below it
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
            offset_y = ROW - H / 2,
            on_click = function() linked.value = not linked.value end,
            g.text { font = "Spectral", size = 15, color = theme.text, text = function() return if linked.value then "linked" else "link" end },
        },
        cell {
            role = "button",
            name = "Move",
            offset_x = function() return bX() - W / 2 end,
            offset_y = ROW - H / 2,
            transition = { offset_x = "travel" },
            on_click = function() far.value = not far.value end,
            g.text { font = "Spectral", size = 15, color = theme.text, text = "move" },
        },
        -- The bud: a click and a panel leaves the cell the way a cell divides,
        -- joined by a neck that narrows and parts; again and it is taken back.
        cell {
            role = "button",
            name = "Bud",
            offset_x = A_X - W / 2,
            offset_y = 190 - H / 2,
            on_click = function() budded.value = not budded.value end,
            g.text { font = "Spectral", size = 15, color = theme.text, text = function() return if budded.value then "taken back" else "bud" end },
            g.capsule {
                overlay = true,
                attach = "below",
                width = 220,
                height = 88,
                radius = 20,
                fill = theme.cellFill,
                rim_top = theme.rim,
                rim_bottom = theme.line,
                blur = true,
                -- Inside the cell while closed, where nothing of it is drawn;
                -- out below it, at the expansion's gap, when open.
                offset_y = function() return if budded.value then 14 else -40 end,
                growth = function() return if budded.value then 1 else 0 end,
                -- The neck: soft and wide as it leaves, gone once it is out.
                bud = function() return if budded.value then 0 else 36 end,
                transition = {
                    offset_y = function() return if budded.value then "bud_open" else "bud_close" end,
                    growth = function() return if budded.value then "bud_open" else "bud_close" end,
                    bud = function() return if budded.value then "bud_open" else "bud_close" end,
                },
            },
        },
        -- Depth: a panel that unfolds from its top edge, as a page turns down
        -- toward the viewer (PRD §8.4). Its edge stays sharp at every angle;
        -- its text stays flat for now (PRD §12, question 2).
        cell {
            role = "button",
            name = "Unfold",
            offset_x = 330 - W / 2,
            offset_y = 190 - H / 2,
            on_click = function() unfolded.value = not unfolded.value end,
            g.text { font = "Spectral", size = 15, color = theme.text, text = function() return if unfolded.value then "fold" else "unfold" end },
            g.capsule {
                overlay = true,
                attach = "below",
                offset_y = 14,
                width = 220,
                height = 88,
                radius = 20,
                fill = theme.cellFill,
                rim_top = theme.rim,
                rim_bottom = theme.line,
                blur = true,
                pivot_y = 0,
                perspective = 500,
                rotate_x = function() return if unfolded.value then 0 else -90 end,
                opacity = function() return if unfolded.value then 1 else 0 end,
                transition = {
                    rotate_x = function() return if unfolded.value then "unfold" else "fold" end,
                    opacity = function() return if unfolded.value then "instant" else "thread_hold" end,
                },
                g.capsule {
                    width = 160, height = 6, radius = 3, fill = theme.line,
                },
            },
        },
        -- Particles along the link, as many as the processor is busy: a
        -- flow with data behind it (PRD §8.3), carried from the first capsule
        -- to the second, still when nothing is linked.
        g.emitter {
            overlay = true,
            width = W,
            height = H,
            from_x = A_X + 60,
            from_y = ROW,
            to_x = function() return bX() - 60 end,
            to_y = ROW,
            -- Along the thread's own curve, on the thread's own springs.
            bend_x = function() return (A_X + 60 + bX() - 60) / 2 end,
            bend_y = ROW + 36,
            transition = { to_x = "travel", bend_x = "sway" },
            rate = function() return if linked.value then vitals.cpu * 300 else 0 end,
            lifetime = 1.2,
            spread = 0.5,
            particle_size = 3,
            fill = theme.primary,
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
            from_y = ROW,
            to_x = function() return bX() - 60 end,
            to_y = ROW,
            bend_x = function() return (A_X + 60 + bX() - 60) / 2 end,
            bend_y = ROW + 36,
            growth = function() return if linked.value then 1 else 0 end,
            transition = {
                growth = function() return if linked.value then "grow" else "retract" end,
                to_x = "travel",
                bend_x = "sway",
            },
        },
    },
}
