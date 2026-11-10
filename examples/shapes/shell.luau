--!strict
-- Shapes of any outline, and shapes drawn as one: g.path from a polygon
-- or SVG path data (filled, stroked, dashed, even-odd), and capsules
-- combined by their distance fields — two drops joined by a neck that
-- narrows as the slider pulls them apart, a moon cut out of a disc, a
-- lens where two discs meet.
--
--   genoma examples/shapes
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme
local apart = g.signal(0.55)

-- A five-pointed star's corners around (r, r).
local function star(r: number, inner: number): { number }
    local points = {}
    for i = 0, 9 do
        local a = math.pi / 5 * i - math.pi / 2
        local d = if i % 2 == 0 then r else r * inner
        table.insert(points, r + d * math.cos(a))
        table.insert(points, r + d * math.sin(a))
    end
    return points
end

local function label(text: string): any
    return g.text { size = 12, color = function() return theme.get("faint") end, text = text }
end

local function cell(title: string, body: any): any
    return g.box {
        direction = "column",
        gap = 10,
        width = 130,
        body,
        label(title),
    }
end

local accent = function() return theme.get("accent") end

return g.surface {
    anchor = "top",
    margin = { top = 120 },
    namespace = "genoma-shapes",

    g.capsule {
        direction = "column",
        align = "start",
        gap = 18,
        padding = 20,
        radius = 14,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,

        g.box {
            gap = 10,
            align = "end",
            cell("polygon", g.path {
                points = star(40, 0.45),
                fill = accent,
                stroke = 2,
                color = "#ffffff55",
                join = "round",
            }),
            cell("path, dashed", g.path {
                width = 110,
                height = 80,
                d = "M 4 60 C 30 0, 50 0, 55 40 S 85 80, 106 20",
                stroke = 4,
                color = accent,
                cap = "round",
                dash = 10,
                dash_gap = 8,
            }),
            cell("even-odd", g.path {
                d = "M 40 0 A 40 40 0 1 1 39.9 0 Z M 40 18 A 22 22 0 1 0 40.1 18 Z",
                even_odd = true,
                fill = "#e6b062",
            }),
        },

        g.box {
            gap = 10,
            align = "end",
            cell("union, soft", g.box {
                width = 120,
                height = 80,
                g.capsule {
                    id = "drop",
                    overlay = true,
                    attach = "corner",
                    offset_x = 10,
                    offset_y = 20,
                    width = 40,
                    height = 40,
                    -- Its outline only: the union draws it.
                    fill = "#00000000",
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                },
                g.capsule {
                    overlay = true,
                    attach = "corner",
                    combine_with = "drop",
                    combine_soft = 24,
                    offset_x = function() return 30 + apart.value * 60 end,
                    offset_y = 25,
                    width = 30,
                    height = 30,
                    fill = "#88c0d0",
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                },
            }),
            cell("three, soft", g.box {
                width = 120,
                height = 80,
                g.capsule { id = "left", overlay = true, attach = "corner", offset_x = 8, offset_y = 22, width = 36, height = 36, fill = "#00000000", rim_top = "#00000000", rim_bottom = "#00000000" },
                g.capsule { id = "right", overlay = true, attach = "corner", offset_x = 76, offset_y = 22, width = 36, height = 36, fill = "#00000000", rim_top = "#00000000", rim_bottom = "#00000000" },
                -- One shape with both: three drops, two necks.
                g.capsule {
                    overlay = true,
                    attach = "corner",
                    combine_with = "left right",
                    combine_soft = 20,
                    offset_x = 47,
                    offset_y = function() return 10 + apart.value * 30 end,
                    width = 26,
                    height = 26,
                    fill = "#a3be8c",
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                },
            }),
            cell("subtract", g.box {
                width = 80,
                height = 80,
                g.capsule {
                    id = "bite",
                    overlay = true,
                    attach = "corner",
                    offset_x = 24,
                    offset_y = 4,
                    width = 60,
                    height = 60,
                    fill = "#00000000",
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                },
                g.capsule {
                    overlay = true,
                    attach = "corner",
                    combine_with = "bite",
                    combine = "subtract",
                    combine_soft = 2,
                    offset_x = 4,
                    offset_y = 8,
                    width = 66,
                    height = 66,
                    fill = "#e6b062",
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                },
            }),
            cell("intersect", g.box {
                width = 100,
                height = 80,
                g.capsule {
                    id = "lens",
                    overlay = true,
                    attach = "corner",
                    offset_x = 0,
                    offset_y = 4,
                    width = 72,
                    height = 72,
                    fill = "#00000000",
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                },
                g.capsule {
                    overlay = true,
                    attach = "corner",
                    combine_with = "lens",
                    combine = "intersect",
                    offset_x = 28,
                    offset_y = 4,
                    width = 72,
                    height = 72,
                    fill = "#e88aa8",
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                },
            }),
        },

        g.box {
            gap = 12,
            label("Apart"),
            kit.slider {
                width = 260,
                name = "Apart",
                value = function() return apart.value end,
                on_change = function(v) apart.value = v end,
            },
        },
    },
}
