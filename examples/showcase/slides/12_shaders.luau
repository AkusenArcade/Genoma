--!strict
-- Pixels of one's own: a WGSL shader written for this slide, warping noise
-- with noise, and the effects Genoma ships beside it — noise, ripples, a
-- dissolve, a vignette with grain — each a node with its numbers named.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")
local effects = require("@genoma/kit/effects")

local u = style.u
local t = style.t

local function rgba(hex: string): { number }
    local c = g.colour.parse(hex)
    return { c.r, c.g, c.b, c.a }
end

return {
    key = "shaders",
    kicker = "Shaders",
    title = "Your own pixels",
    caption = "A node can be a WGSL shader of your own, with a library of noise, distance fields and colour in every one. Or one of the effects Genoma ships, its parameters bound like any value.",
    duration = 11,
    code = [[
-- plasma.wgsl
fn shade(p: vec2<f32>) -> vec4<f32> {
    let uv = p / shader.size.y * 2.2;
    let q = vec2<f32>(g_fbm(uv, 5), g_fbm(uv + 5.2, 5));
    let f = 0.5 + 0.5 * g_fbm(uv + 3.0 * q, 5);
    return vec4<f32>(mix(g_rgba(0).rgb,
                         g_rgba(4).rgb, f), 1.0);
}

-- shell.luau
g.shader {
    src = g.dir .. "/plasma.wgsl",
    clock = true,
    values = function() return colours() end,
}]],

    build = function(ctx: { [string]: any }): any
        -- The dissolve goes and comes back: its progress, on a clock.
        local clock = g.signal(0)
        g.timer { every = 1 / 30, on_tick = function()
            if g.untracked(ctx.arrived) then clock.value += 1 / 30 end
        end }

        return frame.new {
            number = ctx.number,
            kicker = "Shaders",
            title = "Your own pixels",
            caption = "A node can be a WGSL shader of your own, with a library of noise, distance fields and colour in every one. Or one of the effects Genoma ships, its parameters bound like any value.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                frame.tile(0, 0, 640, 400, "YOUR OWN WGSL", function(w, h)
                    return g.shader {
                        width = w,
                        height = h,
                        src = g.dir .. "/plasma.wgsl",
                        clock = true,
                        fps = 60,
                        values = function()
                            local out = {}
                            for _, c in { t.colour("accent2"), t.colour("glow"), "#E05E7A" } do
                                for _, v in rgba(c) do table.insert(out, v) end
                            end
                            return out
                        end,
                    }
                end),
                frame.tile(670, 0, 330, 400, "RIPPLE", function(w, h)
                    return g.box {
                        width = w,
                        height = h,
                        g.capsule { overlay = true, attach = "corner", width = w, height = h, radius = 0,
                            fill = "#0E2A3B", fill_bottom = "#05070A", rim_top = style.CLEAR, rim_bottom = style.CLEAR },
                        effects.ripple { overlay = true, attach = "corner", width = w, height = h,
                            color = "#5ED3C1", centre = { 0.5, 0.45 }, spacing = 26, speed = 0.8, reach = 260, ring = 0.25 },
                    }
                end),
                frame.tile(0, 430, 320, 340, "NOISE", function(w, h)
                    return effects.noise { width = w, height = h, from = "#0B1A12", to = "#B9DE6B", size = 70, drift = 0.35, contrast = 1.3 }
                end),
                frame.tile(340, 430, 320, 340, "DISSOLVE", function(w, h)
                    return g.box {
                        width = w,
                        height = h,
                        g.capsule { overlay = true, attach = "corner", width = w, height = h, radius = 0,
                            fill = "#1D3F7A", fill_bottom = "#05070A", rim_top = style.CLEAR, rim_bottom = style.CLEAR },
                        g.box {
                            overlay = true,
                            attach = "corner",
                            width = w,
                            height = h,
                            opacity = 1,
                            effects.dissolve {
                                width = w,
                                height = h,
                                color = "#F2C14E",
                                edge_color = "#FFFFFF",
                                progress = function() return 0.5 - 0.48 * math.cos(clock.value * 1.6) end,
                                edge = 0.07,
                                grain = 30,
                            },
                        },
                    }
                end),
                frame.tile(680, 430, 320, 340, "VIGNETTE + GRAIN", function(w, h)
                    return g.box {
                        width = w,
                        height = h,
                        g.capsule { overlay = true, attach = "corner", width = w, height = h, radius = 0,
                            fill_stops = { { 0, "#E05E7A" }, { 0.5, "#F2C14E" }, { 1, "#5ED3C1" } }, fill_angle = 135,
                            rim_top = style.CLEAR, rim_bottom = style.CLEAR },
                        effects.vignette { overlay = true, attach = "corner", width = w, height = h, strength = 0.85, start = 0.25, softness = 0.7 },
                        effects.grain { overlay = true, attach = "corner", width = w, height = h, amount = 0.22, size = 1.6, rate = 18 },
                    }
                end),
            },
        }
    end,
}
