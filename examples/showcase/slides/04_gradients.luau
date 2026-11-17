--!strict
-- Gradients in every shape a fill takes: along a line at any angle, out
-- from the middle, round it, and six stops at once. The angles turn on a
-- straight line in time, so the turn never slows.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")

local u = style.u
local cap = style.cap

local AURORA = {
    { 0, "#0E2A3B" }, { 0.25, "#1E7F8A" }, { 0.45, "#5ED3C1" },
    { 0.62, "#B9DE6B" }, { 0.8, "#F2C14E" }, { 1, "#E05E7A" },
}

return {
    key = "gradients",
    kicker = "Gradients",
    title = "Colour that flows",
    caption = "Linear at any angle, radial, conic, up to six stops, on any capsule. The angle is a value like any other: bind it, animate it.",
    duration = 10,
    code = [[
g.capsule {
    width = 220, height = 220, radius = 40,
    fill_stops = {
        { 0, "#5ed3c1" }, { 0.5, "#b9de6b" },
        { 1, "#e05e7a" },
    },
    fill_shape = "conic",
    fill_angle = function() return angle.value end,
    transition = { fill_angle = "linear_2" },
}

-- A quarter turn every two seconds, never easing.
g.timer { every = 2, on_tick = function()
    angle.value += 90
end }]],

    build = function(ctx: { [string]: any }): any
        local angle = g.signal(0)
        g.timer { every = 2, on_tick = function()
            if g.untracked(ctx.arrived) then angle.value += 90 end
        end }
        g.watch(ctx.arrived, function(on: boolean) if on then angle.value += 90 end end)
        local pulse = style.cycle(ctx.arrived, 1.6, 2)

        local function tile(x: number, label: string, props: { [any]: any }): any
            props.overlay = true
            props.attach = "corner"
            props.x = x
            props.y = 40
            props.w = 210
            props.h = 210
            props.r = 44
            props.rim_top = function() return g.colour.alpha("#ffffff", 0.35) end
            props.rim_bottom = function() return g.colour.alpha("#ffffff", 0.05) end
            return g.box {
                overlay = true,
                attach = "corner",
                cap(props),
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(x) end,
                    offset_y = function() return u(272) end,
                    width = function() return u(210) end,
                    justify = "center",
                    style.text("label", { text = label, color = style.c("muted") }),
                },
            }
        end

        local function turning(): number return angle.value end

        return frame.new {
            number = ctx.number,
            kicker = "Gradients",
            title = "Colour that flows",
            caption = "Linear at any angle, radial, conic, up to six stops, on any capsule. The angle is a value like any other: bind it, animate it.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                tile(20, "LINEAR", {
                    fill = "#5ED3C1", fill_bottom = "#1D3F7A",
                    fill_angle = turning,
                    transition = { fill_angle = "linear_2" },
                }),
                tile(270, "RADIAL", {
                    fill_stops = function()
                        return if pulse.value == 1
                            then { { 0, "#FFF4C2" }, { 0.45, "#F2C14E" }, { 1, "#3A1D0C" } }
                            else { { 0, "#FFE0EA" }, { 0.45, "#E05E7A" }, { 1, "#2A0C1E" } }
                    end,
                    fill_shape = "radial",
                }),
                tile(520, "CONIC", {
                    fill_stops = { { 0, "#5ED3C1" }, { 0.33, "#B9DE6B" }, { 0.66, "#E05E7A" }, { 1, "#5ED3C1" } },
                    fill_shape = "conic",
                    fill_angle = turning,
                    transition = { fill_angle = "linear_2" },
                }),
                tile(770, "SIX STOPS", {
                    fill_stops = AURORA,
                    fill_angle = function() return 135 + angle.value / 4 end,
                    transition = { fill_angle = "linear_2" },
                }),
                -- The wide pane: the six stops, slowly turning, under glass.
                cap {
                    overlay = true,
                    attach = "corner",
                    x = 20, y = 360, w = 960, h = 380, r = 52,
                    fill_stops = AURORA,
                    fill_angle = function() return 90 + angle.value / 8 end,
                    transition = { fill_angle = "linear_2" },
                    shadow = "#E05E7A55",
                    shadow_blur = function() return u(120) end,
                    shadow_y = function() return u(40) end,
                    align = "center",
                    justify = "center",
                    cap {
                        overlay = false,
                        w = 760, h = 230, r = 36,
                        fill = "#0B0F0A40",
                        backdrop_blur = function() return u(30) end,
                        rim_top = "#FFFFFF66",
                        rim_bottom = "#FFFFFF14",
                        direction = "column",
                        align = "center",
                        justify = "center",
                        gap = function() return u(6) end,
                        style.text("title", { text = "fill_stops", font = nil, color = "#FFFFFF" }),
                        style.text("label", { text = "UP TO SIX  ·  ANY ANGLE  ·  UNDER FROSTED GLASS", color = "#FFFFFFB0" }),
                    },
                },
            },
        }
    end,
}
