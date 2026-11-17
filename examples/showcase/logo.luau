--!strict
-- Genoma's mark, drawn as it is in docs/media/logo.svg: a capsule, and the
-- strand that crosses it, cut where it passes under itself, clipped to the
-- capsule's inside. Each line is a `g.path` in the logo's gradient, so it can
-- draw itself on: `drawn()` true runs each one's `trim_end` from 0 to 1, the
-- capsule first, the strand after it.
local g = require("@genoma")
local style = require("style")
local STRANDS = require("strands")

local M = {}

-- The logo's drawing: the capsule at (6.6, 2.6), 10.8 × 18.8, on a 1.5 line.
local X0, Y0, W0, H0, LINE = 6.6, 2.6, 10.8, 18.8, 1.5

local function fmt(n: number): string return string.format("%.2f", n) end

-- `t.height()`: the capsule's height in logical units; `t.drawn()`.
function M.new(t: { [string]: any }): any
    local function s(): number return t.height() / H0 end
    local function line(): number return LINE * s() end
    local function inner_w(): number return W0 * s() end
    local function inner_h(): number return H0 * s() end

    -- The capsule from the middle of its top, clockwise, so it closes where
    -- it began.
    local function capsule_d(): string
        local h, w, r = inner_h(), inner_w(), inner_w() / 2
        local o = line() / 2
        local cx = o + r
        return table.concat({
            "M", fmt(cx), fmt(o),
            "A", fmt(r), fmt(r), "0 0 1", fmt(o + w), fmt(o + r),
            "L", fmt(o + w), fmt(o + h - r),
            "A", fmt(r), fmt(r), "0 0 1", fmt(o), fmt(o + h - r),
            "L", fmt(o), fmt(o + r),
            "A", fmt(r), fmt(r), "0 0 1", fmt(cx), fmt(o),
        }, " ")
    end

    -- A strand in the clip's own units: the capsule's inside.
    local function strand_d(points: { number }): string
        local out = {}
        local k = s()
        for i = 1, #points, 2 do
            table.insert(out, (if i == 1 then "M " else "L ") .. fmt((points[i] - X0) * k) .. " " .. fmt((points[i + 1] - Y0) * k))
        end
        return table.concat(out, " ")
    end

    -- The logo's gradient, top-left to bottom-right of the capsule, in the
    -- node's own units; `shift` for a node that starts half a line in.
    local function paint(shift: number): () -> { number }
        return function()
            local k = s()
            local o = shift * line() / 2
            return { 1, 0, 1, 0, 0, 1, 0, 0, (9.5 - X0) * k + o, o, (14.5 - X0) * k + o, H0 * k + o }
        end
    end

    local function drawn(): number return if t.drawn() then 1 else 0 end

    local function strokes(): { any }
        local nodes = {
            g.path {
                overlay = true,
                attach = "corner",
                width = function() return inner_w() + line() end,
                height = function() return inner_h() + line() end,
                d = capsule_d,
                stroke = line,
                cap = "round",
                join = "round",
                stroke_paint = paint(1),
                stroke_stops = style.accent_stops,
                trim_end = drawn,
                transition = { trim_end = "draw" },
            },
        }
        local clip = {
            overlay = true,
            attach = "corner",
            offset_x = function() return line() / 2 end,
            offset_y = function() return line() / 2 end,
            width = inner_w,
            height = inner_h,
            radius = function() return inner_w() / 2 end,
            clip = true,
        }
        for i, points in STRANDS do
            table.insert(clip, g.path {
                overlay = true,
                attach = "corner",
                width = inner_w,
                height = inner_h,
                d = function() return strand_d(points) end,
                stroke = line,
                cap = "round",
                join = "round",
                stroke_paint = paint(0),
                stroke_stops = style.accent_stops,
                trim_end = drawn,
                transition = { trim_end = if i == 1 then "draw_strand" else "draw_strand_late" },
            })
        end
        table.insert(nodes, g.box(clip))
        return nodes
    end

    local glow = { overlay = true, attach = "corner", opacity = 0.55, blur_radius = function() return 14 * s() end }
    for _, n in strokes() do table.insert(glow, n) end
    local sharp = { overlay = true, attach = "corner" }
    for _, n in strokes() do table.insert(sharp, n) end

    local root = {
        width = function() return inner_w() + line() end,
        height = function() return inner_h() + line() end,
        g.box(glow),
        g.box(sharp),
    }
    for k, v in t.box or {} do root[k] = v end
    return g.box(root)
end

return M
