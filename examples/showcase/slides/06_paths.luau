--!strict
-- Outlines of any shape, on the GPU: a line that writes itself and is
-- wiped away, one shape becoming the next, dashes that march, and an SVG
-- whose parts are nodes of their own — a needle that turns, an arc that
-- fills.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")

local u = style.u
local t = style.t

-- Outlines in a 220 × 220 box, all of cubic and straight segments so the
-- stage's scale can multiply every number.
local function star(points: number, inner: number): string
    local out = {}
    for i = 0, points * 2 - 1 do
        local r = if i % 2 == 0 then 105 else 105 * inner
        local a = math.pi * i / points - math.pi / 2
        table.insert(out, (if i == 0 then "M " else "L ") .. string.format("%.1f %.1f", 110 + r * math.cos(a), 112 + r * math.sin(a)))
    end
    return table.concat(out, " ") .. " Z"
end
local K = 0.5523 * 100
local SHAPES = {
    -- A circle, as four cubics.
    string.format("M 110 10 C %.1f 10 210 %.1f 210 110 C 210 %.1f %.1f 210 110 210 C %.1f 210 10 %.1f 10 110 C 10 %.1f %.1f 10 110 10 Z",
        110 + K, 110 - K, 110 + K, 110 + K, 110 - K, 110 + K, 110 - K, 110 - K),
    star(5, 0.45),
    "M 110 200 C 40 152 10 112 10 72 C 10 36 38 14 70 14 C 90 14 104 26 110 42 C 116 26 130 14 150 14 C 182 14 210 36 210 72 C 210 112 180 152 110 200 Z",
    star(8, 0.72),
    "M 50 20 L 170 20 C 190 20 200 30 200 50 L 200 170 C 200 190 190 200 170 200 L 50 200 C 30 200 20 190 20 170 L 20 50 C 20 30 30 20 50 20 Z",
}
local NAMES = { "CIRCLE", "STAR", "HEART", "SUN", "SQUARE" }

-- A line across the demo, as one flowing signature.
local WAVE = "M 20 120 C 120 0 200 0 260 110 C 320 220 400 220 470 110 C 540 0 620 0 690 110 C 760 220 840 220 940 70"

-- Every number in a path scaled to the stage.
local function scaled(d: string, extra: number?): string
    local k = style.k.value * (extra or 1)
    return (string.gsub(d, "%-?%d+%.?%d*", function(n: string) return string.format("%.2f", tonumber(n) :: number * k) end))
end

return {
    key = "paths",
    kicker = "Paths & SVG",
    title = "Any outline",
    caption = "SVG path data drawn on the GPU: strokes that write themselves, outlines that morph point by point, marching dashes, and SVG documents whose parts are nodes you can move.",
    duration = 11,
    code = [[
-- One shape becoming another.
g.path {
    d = function() return SHAPES[step.value] end,
    fill = "#b9de6b",
    transition = { d = "morph" },
}

-- An SVG whose parts are nodes.
g.svg {
    src = "gauge.svg",
    parts = {
        needle = {
            rotate = function() return cpu() * 180 - 90 end,
            pivot_at = { 80, 100 },
        },
        level = { trim_end = cpu },
    },
}]],

    build = function(ctx: { [string]: any }): any
        local shape = style.cycle(ctx.arrived, 1.6, #SHAPES)
        -- The signature: 1 empty, 2 written, 3 wiped away.
        local writing = style.cycle(ctx.arrived, 1.7, 3)
        local reading = style.cycle(ctx.arrived, 1.3, 5)
        local READINGS = { 0.25, 0.72, 0.48, 0.93, 0.6 }
        local march = g.signal(0)
        g.timer { every = 2, on_tick = function() march.value += 120 end }
        g.after(0.05, function() march.value += 120 end)

        local function wrote(): string return if writing.value == 1 then "instant" else "draw" end

        return frame.new {
            number = ctx.number,
            kicker = "Paths & SVG",
            title = "Any outline",
            caption = "SVG path data drawn on the GPU: strokes that write themselves, outlines that morph point by point, marching dashes, and SVG documents whose parts are nodes you can move.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                -- The signature.
                g.path {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(20) end,
                    offset_y = function() return u(20) end,
                    width = function() return u(960) end,
                    height = function() return u(240) end,
                    d = function() return scaled(WAVE) end,
                    stroke = function() return u(6) end,
                    cap = "round",
                    stroke_paint = function() return { 1, 0, 1, 0, 0, 1, 0, 0, 0, 0, u(960), 0 } end,
                    stroke_stops = function() return { { 0, t.colour("accent2") }, { 0.5, t.colour("glow") }, { 1, "#E05E7A" } } end,
                    trim_start = function() return if writing.value == 3 then 1 else 0 end,
                    trim_end = function() return if writing.value == 1 then 0 else 1 end,
                    transition = { trim_start = wrote, trim_end = wrote },
                },
                -- The morph.
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(10) end,
                    offset_y = function() return u(340) end,
                    direction = "column",
                    align = "center",
                    gap = function() return u(26) end,
                    g.path {
                        width = function() return u(300) end,
                        height = function() return u(300) end,
                        d = function() return scaled(SHAPES[shape.value], 300 / 220) end,
                        fill_paint = function() return { 2, 0, 1, 0, 0, 1, 0, 0, u(110), u(80), u(300), u(110), u(80), 0 } end,
                        fill_stops = function() return { { 0, t.colour("glow") }, { 1, t.colour("deep") } } end,
                        transition = { d = "morph" },
                    },
                    style.text("label", { text = function() return NAMES[shape.value] end, color = style.c("muted") }),
                },
                -- Marching dashes.
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(360) end,
                    offset_y = function() return u(340) end,
                    direction = "column",
                    align = "center",
                    gap = function() return u(26) end,
                    g.path {
                        width = function() return u(300) end,
                        height = function() return u(300) end,
                        d = function() return scaled(SHAPES[1], 300 / 220) end,
                        stroke = function() return u(5) end,
                        color = style.c("accent2"),
                        cap = "round",
                        dash = function() return u(18) end,
                        dash_gap = function() return u(14) end,
                        dash_offset = function() return u(march.value) end,
                        transition = { dash_offset = "linear_2" },
                    },
                    style.text("label", { text = "DASHES", color = style.c("muted") }),
                },
                -- The SVG, by its parts.
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(680) end,
                    offset_y = function() return u(400) end,
                    direction = "column",
                    align = "center",
                    gap = function() return u(22) end,
                    g.svg {
                        src = "gauge.svg",
                        width = function() return u(320) end,
                        height = function() return u(240) end,
                        color = style.c("text"),
                        parts = {
                            needle = {
                                rotate = function() return READINGS[reading.value] * 180 - 90 end,
                                pivot_at = { 80, 100 },
                                transition = { rotate = "firm" },
                            },
                            level = {
                                trim_end = function() return READINGS[reading.value] end,
                                transition = { trim_end = "firm" },
                            },
                        },
                    },
                    style.text("label", { text = "SVG, BY ITS PARTS", color = style.c("muted") }),
                },
            },
        }
    end,
}
