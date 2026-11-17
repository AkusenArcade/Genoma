--!strict
-- Distance fields meet softly. Four drops orbit a body and are one shape
-- with it while they are close; a drop crosses a word and the word melts
-- into it; a panel buds out of a cell and is taken back.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")

local u = style.u
local cap = style.cap
local t = style.t

-- Where the four drops are, around the body's centre (230, 230), per step.
local ORBITS = {
    { { 230, 110 }, { 350, 230 }, { 230, 350 }, { 110, 230 } },
    { { 320, 140 }, { 320, 320 }, { 140, 320 }, { 140, 140 } },
    { { 230, 50 }, { 410, 230 }, { 230, 410 }, { 50, 230 } },
    { { 300, 160 }, { 300, 300 }, { 160, 300 }, { 160, 160 } },
}
local DROP = 88

local function hidden(props: { [any]: any }): any
    props.fill = style.CLEAR
    return cap(props)
end

return {
    key = "fusion",
    kicker = "Fusion",
    title = "Shapes that merge",
    caption = "Distance fields joined softly: up to five shapes become one, a word melts into a drop, a panel buds out of its cell. All drawn by one shader, from the fields.",
    duration = 11,
    code = [[
-- Shapes that draw nothing themselves…
g.capsule { id = "a", width = 88, height = 88,
            fill = "#00000000" }
g.capsule { id = "b", width = 88, height = 88,
            fill = "#00000000" }

-- …and one that is drawn as one shape with them.
g.capsule {
    width = 160, height = 160,
    combine_with = "a b",
    combine_soft = 60,
    fill = "#b9de6b",
}]],

    build = function(ctx: { [string]: any }): any
        local step = style.cycle(ctx.arrived, 1.5, #ORBITS)
        local across = style.cycle(ctx.arrived, 2.4, 2)
        local budded = style.cycle(ctx.arrived, 1.9, 2)
        local id = "fusion" .. ctx.number

        local nodes: { any } = {}
        local ids = {}
        for i = 1, 4 do
            local name = id .. "-drop" .. i
            table.insert(ids, name)
            table.insert(nodes, hidden {
                id = name,
                x = function() return ORBITS[step.value][i][1] - DROP / 2 end,
                y = function() return ORBITS[step.value][i][2] - DROP / 2 + 170 end,
                w = DROP, h = DROP,
                transition = { offset_x = "soft", offset_y = "soft" },
            })
        end
        table.insert(nodes, cap {
            x = 150, y = 320, w = 160, h = 160,
            combine_with = table.concat(ids, " "),
            combine_soft = function() return u(58) end,
            fill_stops = function() return { { 0, t.colour("glow") }, { 1, t.colour("accent2") } } end,
            fill_angle = 160,
            shadow = function() return g.colour.alpha(t.colour("accent"), 0.35) end,
            shadow_blur = function() return u(60) end,
        })

        -- A drop crossing a word: the word joins it where they meet.
        table.insert(nodes, g.text {
            id = id .. "-word",
            overlay = true,
            attach = "corner",
            offset_x = function() return u(540) end,
            offset_y = function() return u(110) end,
            text = "GENOMA",
            font = style.display,
            size = function() return math.floor(u(84)) end,
            weight = 600,
            color = style.CLEAR,
        })
        table.insert(nodes, cap {
            x = function() return if across.value == 1 then 500 else 930 end,
            y = 118, w = 70, h = 70,
            combine_with = id .. "-word",
            combine_soft = function() return u(26) end,
            fill = style.c("text"),
            transition = { offset_x = "morph" },
        })

        -- A panel budding out of a cell.
        local open = function() return budded.value == 1 end
        table.insert(nodes, cap {
            x = 640, y = 400, w = 220, h = 70,
            fill = function() return g.colour.lift(t.colour("background"), 0.1, true) end,
            rim_top = function() return g.colour.alpha("#ffffff", 0.45) end,
            rim_bottom = function() return g.colour.alpha("#ffffff", 0.06) end,
            style.text("label", { text = "A CELL", color = style.c("muted") }),
            g.capsule {
                overlay = true,
                attach = "below-center",
                width = function() return u(300) end,
                height = function() return u(150) end,
                radius = function() return u(36) end,
                offset_y = function() return if open() then u(18) else -u(40) end,
                growth = function() return if open() then 1 else 0 end,
                bud = function() return if open() then 0 else u(40) end,
                fill = function() return g.colour.lift(t.colour("background"), 0.1, true) end,
                rim_top = function() return g.colour.alpha("#ffffff", 0.45) end,
                rim_bottom = function() return g.colour.alpha("#ffffff", 0.06) end,
                transition = { offset_y = "morph", growth = "morph", bud = "morph" },
                style.text("body", {
                    text = "budded out of it",
                    italic = true,
                    color = style.c("glow"),
                    opacity = function() return if open() then 1 else 0 end,
                    transition = { opacity = "fade" },
                }),
            },
        })

        local demo = { width = function() return u(1000) end, height = function() return u(800) end }
        for _, n in nodes do table.insert(demo, n) end
        return frame.new {
            number = ctx.number,
            kicker = "Fusion",
            title = "Shapes that merge",
            caption = "Distance fields joined softly: up to five shapes become one, a word melts into a drop, a panel buds out of its cell. All drawn by one shader, from the fields.",
            arrived = ctx.arrived,
            demo = g.box(demo),
        }
    end,
}
