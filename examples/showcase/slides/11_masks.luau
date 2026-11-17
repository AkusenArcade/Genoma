--!strict
-- A node as another's mask: a word cut out of a field of moving noise, a
-- turning star showing a conic gradient through it. And three lights
-- added where they cross, as a compositor's blend modes add them.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")
local effects = require("@genoma/kit/effects")

local u = style.u
local cap = style.cap
local t = style.t

local function star(points: number, inner: number, size: number): string
    local out = {}
    local c = size / 2
    for i = 0, points * 2 - 1 do
        local r = if i % 2 == 0 then c else c * inner
        local a = math.pi * i / points - math.pi / 2
        table.insert(out, (if i == 0 then "M " else "L ") .. string.format("%.1f %.1f", c + r * math.cos(a), c + r * math.sin(a)))
    end
    return table.concat(out, " ") .. " Z"
end

-- Where the three lights sit, apart and together.
local LIGHTS = {
    { { 60, 0 }, { 0, 100 }, { 120, 100 } },
    { { 60, -60 }, { -70, 140 }, { 190, 140 } },
}
local COLOURS = { "#FF3B6B", "#3BFF8A", "#3B7BFF" }

return {
    key = "masks",
    kicker = "Masks & blends",
    title = "Shapes that reveal",
    caption = "Any node can be its parent's mask: a word, a path, a shape that moves. And what is drawn meets what is under it by blend mode: multiply, screen, add, overlay.",
    duration = 10,
    code = [[
-- A word as the mask of a field of noise.
g.box {
    effects.noise { width = 900, height = 260,
                    from = "#5ed3c1", to = "#dbf4a7",
                    size = 120, drift = 0.4 },
    g.text { text = "LIVING", size = 230,
             font = "Orbitron", mask = true },
}

-- Three lights, added where they cross.
g.capsule { width = 240, height = 240,
            fill = "#ff3b6b", blend = "add" }]],

    build = function(ctx: { [string]: any }): any
        local apart = style.cycle(ctx.arrived, 1.8, 2)
        local turn = g.signal(0)
        g.timer { every = 3, on_tick = function() turn.value += 60 end }
        g.after(0.05, function() turn.value += 60 end)

        local lights = {
            overlay = true,
            attach = "corner",
            offset_x = function() return u(90) end,
            offset_y = function() return u(420) end,
            width = function() return u(420) end,
            height = function() return u(360) end,
        }
        for i = 1, 3 do
            table.insert(lights, cap {
                x = function() return LIGHTS[apart.value][i][1] + 40 end,
                y = function() return LIGHTS[apart.value][i][2] + 30 end,
                w = 200, h = 200,
                fill = COLOURS[i],
                blend = "add",
                transition = { offset_x = "soft", offset_y = "soft" },
            })
        end

        return frame.new {
            number = ctx.number,
            kicker = "Masks & blends",
            title = "Shapes that reveal",
            caption = "Any node can be its parent's mask: a word, a path, a shape that moves. And what is drawn meets what is under it by blend mode: multiply, screen, add, overlay.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                -- The word, filled with moving noise.
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(10) end,
                    offset_y = function() return u(20) end,
                    width = function() return u(980) end,
                    height = function() return u(300) end,
                    effects.noise {
                        overlay = true,
                        attach = "corner",
                        width = function() return u(980) end,
                        height = function() return u(300) end,
                        from = "#1D3F7A",
                        to = "#DBF4A7",
                        size = 110,
                        drift = 0.5,
                        contrast = 1.6,
                    },
                    g.text {
                        overlay = true,
                        attach = "corner",
                        offset_y = function() return u(10) end,
                        text = "LIVING",
                        font = style.display,
                        weight = 800,
                        size = function() return math.floor(u(250)) end,
                        color = "#FFFFFF",
                        mask = true,
                    },
                },
                g.box(lights),
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(200) end,
                    offset_y = function() return u(775) end,
                    style.text("label", { text = "BLEND  ·  ADD", color = style.c("muted") }),
                },
                -- A turning star, the conic gradient behind it.
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(600) end,
                    offset_y = function() return u(400) end,
                    width = function() return u(340) end,
                    height = function() return u(340) end,
                    cap {
                        x = 0, y = 0, w = 340, h = 340,
                        fill_stops = { { 0, "#5ED3C1" }, { 0.33, "#B9DE6B" }, { 0.66, "#E05E7A" }, { 1, "#5ED3C1" } },
                        fill_shape = "conic",
                        fill_angle = function() return turn.value * 2 end,
                        transition = { fill_angle = "linear_3" },
                    },
                    g.path {
                        overlay = true,
                        attach = "corner",
                        width = function() return u(340) end,
                        height = function() return u(340) end,
                        -- The star turns one way, the light under it the other.
                        d = function()
                            local k = style.k.value
                            return (string.gsub(star(6, 0.5, 340), "%-?%d+%.?%d*", function(n: string) return string.format("%.2f", tonumber(n) :: number * k) end))
                        end,
                        fill = "#FFFFFF",
                        rotate = function() return turn.value end,
                        transition = { rotate = "linear_3" },
                        mask = true,
                    },
                },
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(690) end,
                    offset_y = function() return u(775) end,
                    style.text("label", { text = "A PATH AS A MASK", color = style.c("muted") }),
                },
            },
        }
    end,
}
