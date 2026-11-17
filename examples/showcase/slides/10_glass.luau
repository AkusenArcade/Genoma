--!strict
-- Glass over a scene that moves: a frosted pane blurring what is behind it,
-- a lens bending it at the rim with its colours parting, and a group
-- pulled in and out of focus.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")
local effects = require("@genoma/kit/effects")

local u = style.u
local cap = style.cap
local t = style.t

-- The blobs behind, by step: x, y of each.
local BLOBS = {
    { { 60, 40 }, { 520, 240 }, { 300, 480 } },
    { { 420, 20 }, { 120, 420 }, { 640, 360 } },
    { { 640, 120 }, { 300, 180 }, { 80, 470 } },
}
local COLOURS = { { "#5ED3C1", "#1D3F7A" }, { "#F2C14E", "#8A3B12" }, { "#E05E7A", "#4A1238" } }

return {
    key = "glass",
    kicker = "Blur & glass",
    title = "Frosted, bent light",
    caption = "Blur what lies behind a shape, blur a whole subtree, or bend what is under a lens at its rim with the colours parting. Drawn in the shell, wherever the compositor offers no blur.",
    duration = 11,
    code = [[
-- Frosted: what the surface drew under it.
g.capsule {
    backdrop_blur = 28,
    fill = "#ffffff12",
}

-- A lens: bent at its rim, colours parting.
effects.glass {
    width = 300, height = 300,
    radius = 150, bend = 24, aberration = 3,
}

-- A group in and out of focus.
g.box { blur_radius = function()
    return if sharp.value then 0 else 14
end, transition = { blur_radius = "morph" } }]],

    build = function(ctx: { [string]: any }): any
        local step = style.cycle(ctx.arrived, 1.8, #BLOBS)
        local lens = style.cycle(ctx.arrived, 2.0, 2)
        local focus = style.cycle(ctx.arrived, 1.6, 2)

        local scene: { any } = {}
        for i = 1, 3 do
            table.insert(scene, cap {
                x = function() return BLOBS[step.value][i][1] end,
                y = function() return BLOBS[step.value][i][2] end,
                w = 300, h = 300,
                fill_stops = { { 0, COLOURS[i][1] }, { 1, COLOURS[i][2] .. "00" } },
                fill_shape = "radial",
                transition = { offset_x = "soft", offset_y = "soft" },
            })
        end
        table.insert(scene, g.box {
            overlay = true,
            attach = "corner",
            offset_x = function() return u(10) end,
            offset_y = function() return u(60) end,
            style.text("hero", { text = "GLASS", size = nil, color = style.c("text") }),
        })
        -- Stripes the lens bends.
        for i = 0, 9 do
            table.insert(scene, cap {
                x = 40 + i * 62, y = 230, w = 22, h = 120, r = 11,
                fill = if i % 2 == 0 then t.colour("accent") else t.colour("accent2"),
            })
        end

        local demo: { any } = {
            width = function() return u(1000) end,
            height = function() return u(800) end,
        }
        for _, n in scene do table.insert(demo, n) end

        -- The lens, travelling over the word and the stripes.
        table.insert(demo, g.box {
            overlay = true,
            attach = "corner",
            offset_x = function() return u(if lens.value == 1 then 120 else 520) end,
            offset_y = function() return u(70) end,
            transition = { offset_x = "morph" },
            effects.glass {
                width = function() return u(300) end,
                height = function() return u(300) end,
                radius = function() return u(150) end,
                bend = function() return u(26) end,
                aberration = 3.5,
                frost = 0,
                tint = "#FFFFFF0C",
                rim = 0.6,
            },
        })

        -- A frosted pane.
        table.insert(demo, cap {
            x = 40, y = 470, w = 420, h = 280, r = 34,
            fill = "#FFFFFF10",
            backdrop_blur = function() return u(30) end,
            rim_top = "#FFFFFF66",
            rim_bottom = "#FFFFFF12",
            shadow = "#00000066",
            shadow_blur = function() return u(40) end,
            direction = "column",
            align = "start",
            justify = "end",
            padding = function() return u(30) end,
            gap = function() return u(6) end,
            style.text("title", { text = "Frosted", color = "#FFFFFF" }),
            style.text("label", { text = "BACKDROP_BLUR", color = "#FFFFFFB0" }),
        })

        -- In and out of focus: the card sharp, and the card blurred, both
        -- drawn once and kept, crossing over. A radius animated frame by
        -- frame would blur it again at every step, and each start of the
        -- turn cost a frame.
        local function card(): any
            return cap {
                overlay = false, w = 420, h = 200, r = 34,
                fill_stops = function() return { { 0, t.colour("glow") }, { 1, t.colour("accent2") } } end,
                fill_angle = 120,
                align = "center",
                justify = "center",
                style.text("number", { text = "f / 1.4", color = "#0B0F0A" }),
            }
        end
        local function sharp(): boolean return focus.value == 1 end
        table.insert(demo, g.box {
            overlay = true,
            attach = "corner",
            offset_x = function() return u(540) end,
            offset_y = function() return u(470) end,
            width = function() return u(420) end,
            height = function() return u(280) end,
            direction = "column",
            align = "start",
            justify = "end",
            gap = function() return u(6) end,
            g.box {
                width = function() return u(420) end,
                height = function() return u(200) end,
                -- Never quite 0, either of them: what is not drawn is not
                -- kept, and would be made again as it comes back.
                g.box {
                    overlay = true,
                    attach = "corner",
                    opacity = function() return if sharp() then 1 else 0.002 end,
                    transition = { opacity = "morph" },
                    card(),
                },
                g.box {
                    overlay = true,
                    attach = "corner",
                    blur_radius = function() return u(16) end,
                    opacity = function() return if sharp() then 0.002 else 1 end,
                    transition = { opacity = "morph" },
                    card(),
                },
            },
            style.text("label", { text = "BLUR_RADIUS", color = style.c("muted") }),
        })

        return frame.new {
            number = ctx.number,
            kicker = "Blur & glass",
            title = "Frosted, bent light",
            caption = "Blur what lies behind a shape, blur a whole subtree, or bend what is under a lens at its rim with the colours parting. Drawn in the shell, wherever the compositor offers no blur.",
            arrived = ctx.arrived,
            demo = g.box(demo),
        }
    end,
}
