--!strict
-- Particles over their lives: colour and size that change as they age,
-- trails drawn behind them, and sub-emitters that burst where a parent's
-- particles die, all worked out on the GPU from each particle's birth.
-- The second row is stepped instead, on the GPU too: particles pulled
-- toward a node, bouncing off others, pushed away by a third. The third
-- draws them as pictures: leaves that turn as they fall, and stars drawn
-- to the pointer.
--
--   genoma examples/particles
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme

local W, H = 240, 180

local function panel(title: string, body: any): any
    return g.box {
        direction = "column",
        gap = 8,
        g.capsule {
            width = W,
            height = H,
            radius = 12,
            clip = true,
            fill = "#0b0d12",
            rim_top = function() return theme.get("line") end,
            body,
        },
        g.text { size = 12, color = function() return theme.get("faint") end, text = title },
    }
end

return g.surface {
    anchor = "top",
    margin = { top = 120 },
    namespace = "genoma-particles",

    g.capsule {
        direction = "column",
        align = "start",
        gap = 16,
        padding = 18,
        radius = 14,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,

        g.box {
            gap = 14,
            -- Embers rising from a fire: yellow to deep red, shrinking.
            panel("colour and size over life", g.emitter {
                overlay = true, attach = "corner", width = W, height = H,
                from_x = W / 2, from_y = H - 16,
                rate = 70, lifetime = 2.2, speed = 50, spread = 40, gravity = -12,
                particle_size = 7, size_end = 1.5,
                fill = "#ffd27a", color_end = "#b0302080",
            }),
            -- Comets: long tails, falling under gravity.
            panel("trails", g.emitter {
                overlay = true, attach = "corner", width = W, height = H,
                from_x = 30, from_y = H - 20,
                rate = 4, lifetime = 2.4, speed = 150, spread = 30, heading = 40, gravity = 60,
                particle_size = 5, trail = 0.35,
                fill = "#9ad7ff", color_end = "#7aa2f7",
            }),
            -- Fireworks: rockets that burst where they die, sparks with tails.
            panel("sub-emitters", g.emitter {
                overlay = true, attach = "corner", width = W, height = H,
                from_x = W / 2, from_y = H - 10,
                rate = 1.6, lifetime = 1.1, speed = 150, spread = 30, gravity = 40,
                particle_size = 3, trail = 0.15, fill = "#ffe9b0",
                g.emitter {
                    sub = true, burst = 28,
                    lifetime = 1.3, speed = 70, spread = 360, gravity = 50,
                    particle_size = 3, size_end = 1, trail = 0.12,
                    fill = "#ff9ec7", color_end = "#7a5cff00",
                },
            }),
        },
        g.box {
            gap = 14,
            -- Thrown past a core that pulls them into orbit.
            panel("attracted", g.box {
                width = W, height = H,
                g.capsule { id = "core", overlay = true, width = 14, height = 14, fill = "#b9de6b" },
                g.emitter {
                    overlay = true, attach = "corner", width = W, height = H,
                    from_x = 20, from_y = H - 20,
                    rate = 40, lifetime = 4, speed = 90, spread = 20, heading = 30,
                    attract = "core", attraction = 260, attraction_reach = 200,
                    particle_size = 3, trail = 0.2,
                    fill = "#b9de6b", color_end = "#4f8a6b00",
                },
            }),
            -- Rain on a shelf and a floor.
            panel("bouncing", g.box {
                width = W, height = H,
                g.capsule { id = "shelf", overlay = true, attach = "corner", offset_x = 40, offset_y = 80, width = 110, height = 12, radius = 6, fill = "#3b4261" },
                g.capsule { id = "floor", overlay = true, attach = "corner", offset_x = 0, offset_y = H - 10, width = W, height = 10, radius = 0, fill = "#3b4261" },
                g.emitter {
                    overlay = true, attach = "corner", width = W, height = H,
                    from_x = W / 2 - 20, from_y = 6,
                    rate = 30, lifetime = 3, speed = 70, spread = 140, heading = 180, gravity = 260,
                    collide = "shelf floor", bounce = 0.45,
                    particle_size = 4,
                    fill = "#7dcfff", color_end = "#7dcfff40",
                },
            }),
            -- A fountain parted by a stone that pushes.
            panel("pushed", g.box {
                width = W, height = H,
                g.capsule { id = "stone", overlay = true, attach = "corner", offset_x = W / 2 - 9, offset_y = 50, width = 18, height = 18, fill = "#e0af68" },
                g.emitter {
                    overlay = true, attach = "corner", width = W, height = H,
                    from_x = W / 2, from_y = H - 10,
                    rate = 80, lifetime = 2.6, speed = 200, spread = 14, gravity = 110,
                    attract = "stone", attraction = -1600, attraction_reach = 70,
                    particle_size = 3,
                    fill = "#e0af68", color_end = "#f7768e00",
                },
            }),
        },
        g.box {
            gap = 14,
            -- Leaves: an SVG drawn in each particle's colour, turning.
            panel("sprites", g.emitter {
                overlay = true, attach = "corner", width = W, height = H,
                from_x = W / 2, from_y = -10,
                rate = 6, lifetime = 5, speed = 30, spread = 120, heading = 180, gravity = 12,
                icon = g.dir .. "/leaf.svg", particle_size = 16, spin = 120,
                fill = "#9ece6a", color_end = "#e0af6800",
            }),
            -- Stars that come to the pointer while it is over the panel.
            panel("drawn to the pointer", g.emitter {
                overlay = true, attach = "corner", width = W, height = H,
                from_x = W / 2, from_y = H / 2,
                rate = 25, lifetime = 3, speed = 60, spread = 360,
                attract = "pointer", attraction = 300, attraction_reach = 160,
                icon = g.dir .. "/star.svg", particle_size = 10, size_end = 4, spin = 200,
                fill = "#ffd27a", color_end = "#ff9ec700",
            }),
        },
    },
}
