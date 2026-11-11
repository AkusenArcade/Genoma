--!strict
-- Post-processing: a pass child (`pass = true`) takes the node it is in,
-- drawn with its other children into a picture, and draws it again.
-- Passes run in the order written, each reading what the last one left.
-- At rest each picture is drawn once and kept; a pass with a clock draws
-- it again each frame.
--
--   genoma examples/passes
local g = require("@genoma")
local kit = require("@genoma/kit")

local effects = kit.effects
local theme = kit.theme

-- Something worth post-processing: a lit word over two shapes.
local function card(passes: { any }): any
    local node: { [any]: any } = {
        width = 220,
        height = 130,
        radius = 14,
        fill = "#131722",
        rim_top = "#3b4261",
        g.capsule { overlay = true, attach = "corner", offset_x = 18, offset_y = 18, width = 54, height = 54, fill = "#7aa2f7" },
        g.capsule { overlay = true, attach = "corner", offset_x = 150, offset_y = 64, width = 44, height = 44, radius = 8, fill = "#e0af68" },
        g.text { size = 34, weight = 700, color = "#f7f7f7", text = "Genoma" },
    }
    for _, p in passes do
        table.insert(node, p)
    end
    return g.capsule(node)
end

local function panel(title: string, body: any): any
    return g.box {
        direction = "column",
        gap = 8,
        body,
        g.text { size = 12, color = function() return theme.get("faint") end, text = title },
    }
end

return g.surface {
    anchor = "top",
    margin = { top = 120 },
    namespace = "genoma-passes",

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
            panel("as it is", card({})),
            panel("pixelate", card({ effects.pixelate { size = 6 } })),
            panel("chromatic", card({ effects.chromatic { offset = 4 } })),
        },
        g.box {
            gap = 14,
            panel("glow", card({ effects.glow { radius = 14, strength = 1.6, threshold = 0.45 } })),
            panel("wave, then chromatic", card({
                effects.wave { amplitude = 3, wavelength = 50, speed = 0.4 },
                effects.chromatic { offset = 2 },
            })),
            panel("a pass of one's own", card({
                g.shader { pass = true, src = g.dir .. "/scanlines.wgsl", values = { 2, 40 }, clock = true },
            })),
        },
    },
}
