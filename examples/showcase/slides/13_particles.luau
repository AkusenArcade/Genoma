--!strict
-- Many things, cheaply: fireworks from an emitter whose sparks are an
-- emitter of their own, a flock of boids stepped on the GPU, a pattern
-- grown by reaction and diffusion, and a glow put on a group as its
-- post-processing pass.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")
local effects = require("@genoma/kit/effects")

local u = style.u
local cap = style.cap
local t = style.t

return {
    key = "particles",
    kicker = "Particles",
    title = "Many, cheaply",
    caption = "Emitters at a real rate, with gravity, trails and bursts of their own. Simulations stepped on the GPU: a flock, a reaction. Passes that post-process a whole group.",
    duration = 11,
    code = [[
-- Rockets that burst where they die.
g.emitter {
    rate = 1.6, lifetime = 1.1, speed = 300,
    spread = 30, gravity = 80, trail = 0.15,
    g.emitter { sub = true, burst = 40,
                speed = 120, spread = 360,
                fill = "#ff9ec7", color_end = "#7a5cff00" },
}

-- A flock, stepped on the GPU.
effects.boids { count = 600, speed = 80 }

-- A glow pass over a whole group.
g.box { g.text { text = "NEON" },
        effects.glow { radius = 18, strength = 1.6 } }]],

    build = function(ctx: { [string]: any }): any
        return frame.new {
            number = ctx.number,
            kicker = "Particles",
            title = "Many, cheaply",
            caption = "Emitters at a real rate, with gravity, trails and bursts of their own. Simulations stepped on the GPU: a flock, a reaction. Passes that post-process a whole group.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                frame.tile(0, 0, 490, 390, "EMITTERS  ·  SUB-EMITTERS", function(w, h)
                    return g.emitter {
                        width = w,
                        height = h,
                        from_x = function() return w() / 2 end,
                        from_y = function() return h() - u(10) end,
                        rate = 2.6,
                        lifetime = 1.1,
                        speed = function() return u(330) end,
                        spread = 34,
                        gravity = function() return u(90) end,
                        particle_size = function() return u(3.5) end,
                        trail = 0.16,
                        fill = "#FFE9B0",
                        g.emitter {
                            sub = true,
                            burst = 60,
                            lifetime = 1.5,
                            speed = function() return u(160) end,
                            spread = 360,
                            gravity = function() return u(60) end,
                            particle_size = function() return u(3.5) end,
                            size_end = 1,
                            trail = 0.12,
                            fill = "#FF9EC7",
                            color_end = "#7A5CFF00",
                        },
                    }
                end),
                frame.tile(510, 0, 490, 390, "BOIDS  ·  ON THE GPU", function(w, h)
                    return g.box {
                        width = w,
                        height = h,
                        g.capsule { overlay = true, attach = "corner", width = w, height = h, radius = 0,
                            fill = "#06121A", fill_bottom = "#0B1F14", rim_top = style.CLEAR, rim_bottom = style.CLEAR },
                        effects.boids {
                            overlay = true,
                            attach = "corner",
                            width = w,
                            height = h,
                            count = 700,
                            size = 5,
                            color = "#5ED3C1",
                            speed = 90,
                            sight = 26,
                        },
                    }
                end),
                frame.tile(0, 410, 490, 360, "REACTION · DIFFUSION", function(w, h)
                    return effects.reaction_diffusion {
                        width = w,
                        height = h,
                        from = "#071008",
                        to = "#B9DE6B",
                        feed = 0.055,
                        kill = 0.062,
                        steps = 480,
                    }
                end),
                frame.tile(510, 410, 490, 360, "A GLOW PASS", function(w, h)
                    return g.box {
                        width = w,
                        height = h,
                        align = "center",
                        justify = "center",
                        g.box {
                            width = w,
                            height = h,
                            align = "center",
                            justify = "center",
                            direction = "column",
                            gap = function() return u(18) end,
                            g.text {
                                text = "NEON",
                                font = style.display,
                                weight = 700,
                                size = function() return math.floor(u(110)) end,
                                tracking = 0.12,
                                color = "#FF6BD5",
                            },
                            g.box {
                                direction = "row",
                                gap = function() return u(16) end,
                                cap { overlay = false, w = 70, h = 12, fill = "#5ED3C1" },
                                cap { overlay = false, w = 120, h = 12, fill = "#F2C14E" },
                                cap { overlay = false, w = 50, h = 12, fill = "#B9DE6B" },
                            },
                            effects.glow {
                                overlay = true,
                                attach = "corner",
                                width = w,
                                height = h,
                                radius = function() return u(22) end,
                                strength = 1.7,
                                threshold = 0.2,
                            },
                        },
                    }
                end),
            },
        }
    end,
}
