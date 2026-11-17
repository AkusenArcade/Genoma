--!strict
-- What one capsule draws: a fill in a gradient, a rim lit from above,
-- shadows outside and in, a coloured glow, an outline apart from its edge.
-- The large one changes its mind about its shape, on a spring: it grows by
-- its width, height and radius, never by scaling, so its light stays crisp.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")

local u = style.u
local cap = style.cap
local t = style.t

-- The card's shapes: width, height, radius.
local SHAPES = {
    { 460, 300, 44 },
    { 300, 300, 150 },
    { 560, 180, 90 },
    { 360, 360, 28 },
}

return {
    key = "shapes",
    kicker = "Shapes",
    title = "Light on every edge",
    caption = "Every capsule is a distance field: any radius, a rim lit from above, soft shadows outside and inside, glows and outlines. Grown by its size, never scaled, so the light stays sharp.",
    duration = 11,
    code = [[
g.capsule {
    width = function() return shape().w end,
    height = function() return shape().h end,
    radius = function() return shape().r end,
    fill = "#26301f", fill_bottom = "#141a10",
    rim_top = "#ffffff80",      -- lit from above
    rim_bottom = "#ffffff14",
    shadow = "#000000a0", shadow_blur = 80, shadow_y = 30,
    inner_shadow = "#b9de6b40", inner_blur = 40,
    transition = { width = "soft", height = "soft",
                   radius = "soft" },
}]],

    build = function(ctx: { [string]: any }): any
        local step = style.cycle(ctx.arrived, 1.8, #SHAPES)
        local function shape(i: number): number return SHAPES[step.value][i] end
        local beat = style.cycle(ctx.arrived, 1.2, 2)

        local card = cap {
            overlay = true,
            attach = "center",
            offset_x = function() return u(-150) end,
            offset_y = function() return u(-40) end,
            w = function() return shape(1) end,
            h = function() return shape(2) end,
            r = function() return shape(3) end,
            fill = function() return g.colour.lift(t.colour("background"), 0.11, true) end,
            fill_bottom = function() return g.colour.lift(t.colour("background"), 0.03, true) end,
            rim_top = function() return g.colour.alpha("#ffffff", 0.5) end,
            rim_bottom = function() return g.colour.alpha("#ffffff", 0.07) end,
            shadow = "#000000B0",
            shadow_blur = function() return u(80) end,
            shadow_y = function() return u(30) end,
            inner_shadow = function() return g.colour.alpha(t.colour("accent"), 0.35) end,
            inner_blur = function() return u(46) end,
            inner_y = function() return u(-10) end,
            transition = { width = "soft", height = "soft", radius = "soft" },
            align = "center",
            justify = "center",
            direction = "column",
            gap = function() return u(8) end,
            style.text("label", { text = "RIM  ·  SHADOW  ·  INNER LIGHT", color = style.c("muted") }),
            style.text("label", {
                text = function() return string.format("%d × %d  r %d", shape(1), shape(2), shape(3)) end,
                color = style.c("faint"),
            }),
        }

        -- A glow that breathes.
        local orb = cap {
            overlay = true,
            attach = "center",
            offset_x = function() return u(300) end,
            offset_y = function() return u(-200) end,
            w = 150, h = 150,
            fill_stops = function() return { { 0, t.colour("glow") }, { 1, t.colour("accent2") } } end,
            fill_shape = "radial",
            shadow = function() return g.colour.alpha(t.colour("accent2"), if beat.value == 1 then 0.75 else 0.35) end,
            shadow_blur = function() return u(if beat.value == 1 then 90 else 40) end,
            transition = { shadow = "morph", shadow_blur = "morph" },
        }

        -- A focus ring: an outline apart from the edge.
        local pill = cap {
            overlay = true,
            attach = "center",
            offset_x = function() return u(300) end,
            offset_y = function() return u(20) end,
            w = 300, h = 76,
            fill = function() return g.colour.alpha(t.colour("text"), 0.06) end,
            rim_top = function() return g.colour.alpha("#ffffff", 0.3) end,
            rim_bottom = function() return g.colour.alpha("#ffffff", 0.06) end,
            outline = style.c("accent2"),
            outline_width = function() return u(2) end,
            outline_offset = function() return u(if beat.value == 1 then 7 else 3) end,
            transition = { outline_offset = "firm" },
            style.text("body", { text = "Focused", color = style.c("text") }),
        }

        -- Rings: dashed, and solid in two lights.
        local function ring(x: number, dash: number?, top: () -> string, bottom: () -> string): any
            return cap {
                overlay = true,
                attach = "center",
                offset_x = function() return u(x) end,
                offset_y = function() return u(220) end,
                w = 110, h = 110,
                stroke = function() return u(4) end,
                dash = if dash then function() return u(dash) end else nil,
                dash_gap = if dash then function() return u(dash * 0.7) end else nil,
                rim_top = top,
                rim_bottom = bottom,
            }
        end

        return frame.new {
            number = ctx.number,
            kicker = "Shapes",
            title = "Light on every edge",
            caption = "Every capsule is a distance field: any radius, a rim lit from above, soft shadows outside and inside, glows and outlines. Grown by its size, never scaled, so the light stays sharp.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                card,
                orb,
                pill,
                ring(160, 10, style.c("accent"), style.c("accent2")),
                ring(300, nil, style.c("glow"), style.c("deep")),
                ring(440, 3, style.c("text"), style.c("faint")),
            },
        }
    end,
}
