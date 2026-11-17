--!strict
-- The timing set: one place where every duration and curve is named, and
-- every transition names one. Six lanes cross the same distance, each by
-- its own entry; beside each, the curve itself, drawn. A heart that beats
-- by its own rate and a thread that sways by its own period close it.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")

local u = style.u
local cap = style.cap
local t = style.t

g.timing {
    lane_linear = { duration = 1.1, curve = { 0, 0, 1, 1 } },
    lane_out = { duration = 1.1, curve = { 0.16, 1, 0.3, 1 } },
    lane_in_out = { duration = 1.1, curve = { 0.65, 0, 0.35, 1 } },
    lane_back = { duration = 1.1, curve = { 0.34, 1.56, 0.64, 1 } },
    lane_spring = { spring = { stiffness = 140, damping = 9 } },
    lane_firm = { spring = { stiffness = 320, damping = 26 } },
}

local LANES = {
    { "lane_linear", "LINEAR", { 0, 0, 1, 1 } },
    { "lane_out", "EASE OUT", { 0.16, 1, 0.3, 1 } },
    { "lane_in_out", "EASE IN OUT", { 0.65, 0, 0.35, 1 } },
    { "lane_back", "OVERSHOOT", { 0.34, 1.56, 0.64, 1 } },
    { "lane_spring", "SPRING · LOOSE", nil },
    { "lane_firm", "SPRING · FIRM", nil },
}

local TRACK_X, TRACK_W, LANE_Y, LANE_H = 300, 600, 30, 96
local DOT = 34

-- A curve's picture in a 70 × 50 box; a spring's, an oscillation dying out.
local function curve_d(c: { number }?): string
    local w, h = 70, 50
    if c == nil then
        local out = { "M 0 " .. h }
        for i = 1, 40 do
            local x = i / 40
            local y = 1 - math.exp(-5 * x) * math.cos(14 * x)
            table.insert(out, string.format("L %.1f %.1f", x * w, h - y * h * 0.8))
        end
        return table.concat(out, " ")
    end
    return string.format("M 0 %d C %.1f %.1f %.1f %.1f %d %.1f", h, c[1] * w, h - c[2] * h * 0.8, c[3] * w, h - c[4] * h * 0.8, w, h - h * 0.8)
end

local function scaled(d: string): string
    local k = style.k.value
    return (string.gsub(d, "%-?%d+%.?%d*", function(n: string) return string.format("%.2f", tonumber(n) :: number * k) end))
end

return {
    key = "motion",
    kicker = "Motion",
    title = "Everything moves on purpose",
    caption = "Durations, curves and springs are named once, in a timing set. A transition names its entry; change the entry and everything that uses it moves differently.",
    duration = 10,
    code = [[
g.timing {
    out = { duration = 1.1, curve = { 0.16, 1, 0.3, 1 } },
    back = { duration = 1.1, curve = { 0.34, 1.56, 0.64, 1 } },
    loose = { spring = { stiffness = 140, damping = 9 } },
}

g.capsule {
    offset_x = function()
        return if right.value then 560 else 0
    end,
    transition = { offset_x = "loose" },
}]],

    build = function(ctx: { [string]: any }): any
        local side = style.cycle(ctx.arrived, 1.7, 2)
        local function right(): boolean return side.value == 2 end

        local nodes: { any } = {}
        for i, lane in LANES do
            local y = LANE_Y + (i - 1) * LANE_H
            -- The curve, drawn.
            table.insert(nodes, g.path {
                overlay = true,
                attach = "corner",
                offset_x = function() return u(20) end,
                offset_y = function() return u(y + 10) end,
                width = function() return u(70) end,
                height = function() return u(50) end,
                d = function() return scaled(curve_d(lane[3])) end,
                stroke = function() return u(2.5) end,
                cap = "round",
                color = style.c("accent2"),
            })
            table.insert(nodes, g.box {
                overlay = true,
                attach = "corner",
                offset_x = function() return u(110) end,
                offset_y = function() return u(y + 27) end,
                style.text("label", { text = lane[2], color = style.c("muted") }),
            })
            -- The track and the dot that crosses it.
            table.insert(nodes, cap { x = TRACK_X, y = y + 34, w = TRACK_W + DOT, h = 2, r = 1, fill = style.c("line") })
            table.insert(nodes, cap {
                x = function() return TRACK_X + (if right() then TRACK_W else 0) end,
                y = y + 35 - DOT / 2,
                w = DOT, h = DOT,
                fill_stops = function() return { { 0, t.colour("glow") }, { 1, t.colour("accent") } } end,
                fill_shape = "radial",
                shadow = function() return g.colour.alpha(t.colour("accent"), 0.6) end,
                shadow_blur = function() return u(24) end,
                transition = { offset_x = lane[1] },
            })
        end

        -- A heart that beats, by its own rate; a thread that sways.
        table.insert(nodes, g.box {
            overlay = true,
            attach = "corner",
            offset_x = function() return u(300) end,
            offset_y = function() return u(640) end,
            direction = "row",
            align = "center",
            gap = function() return u(30) end,
            cap {
                overlay = false, w = 46, h = 46,
                fill = "#E05E7A",
                shadow = "#E05E7AAA",
                shadow_blur = function() return u(30) end,
                pulse = 72,
            },
            style.text("label", { text = "PULSE  ·  72 A MINUTE", color = style.c("muted") }),
        })
        table.insert(nodes, g.box {
            overlay = true,
            attach = "corner",
            offset_x = function() return u(640) end,
            offset_y = function() return u(640) end,
            direction = "row",
            align = "center",
            gap = function() return u(30) end,
            cap {
                overlay = false, w = 240, h = 14,
                fill = style.c("line"),
                justify = "start",
                cap {
                    overlay = false, w = 50, h = 14,
                    fill_stops = function() return { { 0, t.colour("accent2") }, { 1, t.colour("glow") } } end,
                    fill_across = true,
                    shadow = function() return g.colour.alpha(t.colour("accent2"), 0.6) end,
                    shadow_blur = function() return u(18) end,
                    sway = 2.4,
                    sway_range = function() return u(190) end,
                },
            },
            style.text("label", { text = "SWAY", color = style.c("muted") }),
        })

        local demo = { width = function() return u(1000) end, height = function() return u(800) end }
        for _, n in nodes do table.insert(demo, n) end
        return frame.new {
            number = ctx.number,
            kicker = "Motion",
            title = "Everything moves on purpose",
            caption = "Durations, curves and springs are named once, in a timing set. A transition names its entry; change the entry and everything that uses it moves differently.",
            arrived = ctx.arrived,
            demo = g.box(demo),
        }
    end,
}
