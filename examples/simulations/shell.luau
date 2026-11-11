--!strict
-- Simulations a shader node runs on the GPU (`compute`): a grid of cells
-- each worked out from its neighbours a step before, or a flock of agents
-- each from all the others. Three come built in; the fourth is a file of
-- one's own (life.wgsl). Press over the first and the last, move over the
-- waves, and the flock follows the pointer.
--
--   genoma examples/simulations
local g = require("@genoma")
local kit = require("@genoma/kit")

local effects = kit.effects
local theme = kit.theme

local W, H = 260, 180

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
    namespace = "genoma-simulations",

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
            panel("reaction-diffusion", effects.reaction_diffusion {
                width = W, height = H, grid = { W / 2, H / 2 }, steps = 480,
                from = "#0d0f14", to = "#9ece6a",
            }),
            panel("waves", effects.waves {
                width = W, height = H, grid = { W / 2, H / 2 }, steps = 120,
                color = "#1f4f7a", rain = 3,
            }),
        },
        g.box {
            gap = 14,
            panel("boids", effects.boids {
                width = W, height = H, count = 160, size = 5, color = "#c0caf5", pull = 120,
            }),
            panel("a simulation of one's own", g.shader {
                width = W, height = H,
                compute = g.dir .. "/life.wgsl",
                grid = { W / 3, H / 3 }, steps = 12, clock = true,
            }),
        },
    },
}
