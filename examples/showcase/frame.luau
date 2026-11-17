--!strict
-- A slide's frame: its number and kicker, its title and what it says on the
-- left, coming in one after the other once the slide has arrived, and its
-- demo on the right, in a box of its own. The title slide draws its own.
local g = require("@genoma")
local style = require("style")

local M = {}

local u = style.u

-- Something that rises into place, the n-th of the cascade, once `arrived()`.
function M.rise(n: number, arrived: () -> boolean, node: any): any
    return g.box {
        -- All but invisible before it rises, not 0: drawn on the slide's
        -- first frame, what it needs is ready before it is seen.
        opacity = function() return if arrived() then 1 else 0.002 end,
        offset_y = function() return if arrived() then 0 else u(26) end,
        transition = { opacity = "rise_" .. n, offset_y = "rise_" .. n },
        node,
    }
end

-- A rounded pane at (x, y) in a demo, w × h, its content clipped to its
-- corners and its label in its corner. `node` is built at the pane's size,
-- in logical units: `node(w, h)`.
function M.tile(x: number, y: number, w: number, h: number, label: string, node: (w: () -> number, h: () -> number) -> any): any
    local function W(): number return u(w) end
    local function H(): number return u(h) end
    return g.capsule {
        overlay = true,
        attach = "corner",
        offset_x = function() return u(x) end,
        offset_y = function() return u(y) end,
        width = W,
        height = H,
        radius = function() return u(30) end,
        clip = true,
        fill = "#05070480",
        rim_top = "#FFFFFF40",
        rim_bottom = "#FFFFFF10",
        g.box { overlay = true, attach = "corner", width = W, height = H, node(W, H) },
        g.capsule {
            overlay = true,
            attach = "corner",
            offset_x = function() return u(18) end,
            offset_y = function() return u(h - 46) end,
            height = function() return u(28) end,
            padding_x = function() return u(12) end,
            fill = "#0B0F0AB0",
            rim_top = "#00000000",
            rim_bottom = "#00000000",
            style.text("label", { text = label, color = "#F1F6ECD0" }),
        },
    }
end

-- `t`: number, kicker, title, caption, demo (a node), arrived ().
-- The demo's room is 1000 × 800 at the stage's right.
function M.new(t: { [string]: any }): any
    local arrived = t.arrived
    return g.box {
        width = function() return u(style.W) end,
        height = function() return u(style.H) end,
        g.box {
            overlay = true,
            attach = "corner",
            offset_x = function() return u(120) end,
            offset_y = function() return u(150) end,
            direction = "column",
            align = "start",
            gap = function() return u(22) end,
            width = function() return u(640) end,
            M.rise(1, arrived, style.text("kicker", {
                text = string.format("%02d  ·  %s", t.number, string.upper(t.kicker)),
                color = style.c("accent"),
            })),
            M.rise(2, arrived, style.text("title", {
                text = t.title,
                wrap = true,
                max_width = function() return u(640) end,
            })),
            M.rise(3, arrived, style.text("lead", {
                text = t.caption,
                wrap = true,
                max_width = function() return u(560) end,
                color = style.c("muted"),
            })),
        },
        g.box {
            overlay = true,
            attach = "corner",
            offset_x = function() return u(820) end,
            offset_y = function() return u(140) end,
            width = function() return u(1000) end,
            height = function() return u(800) end,
            align = "center",
            justify = "center",
            M.rise(4, arrived, t.demo),
        },
    }
end

return M
