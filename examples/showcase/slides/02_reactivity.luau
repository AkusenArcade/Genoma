--!strict
-- One signal, five things that read it. Each change runs down the lines to
-- them, each a beat after the one above, and nothing else moves.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")

local u = style.u
local cap = style.cap
local t = style.t

local VALUES = { 0.64, 0.22, 0.91, 0.47, 0.78, 0.35 }

-- The consumers' rows: their centres down the right of the demo.
local ROWS = { 120, 260, 400, 540, 680 }
local SOURCE_X, SOURCE_Y, SOURCE_W, SOURCE_H = 40, 340, 260, 120
local ROW_X = 560

return {
    key = "reactivity",
    kicker = "Reactivity",
    title = "One value, everywhere",
    caption = "A signal changes; only what reads it runs again, and the scene moves to the new value. No polling, no diffing, no frame when nothing changed.",
    duration = 11,
    code = [[
local level = g.signal(0.64)

-- Bindings: functions that read it.
g.text {
    text = function()
        return math.floor(level.value * 100) .. "%"
    end,
}
g.capsule {
    arc = function() return level.value end,
    transition = { arc = "follow" },
}

-- Every binding that read it runs again.
-- Nothing else does.
level.value = 0.22]],

    build = function(ctx: { [string]: any }): any
        local step = style.cycle(ctx.arrived, 2.2, #VALUES)
        local function v(): number return VALUES[step.value] end
        -- A spark down the lines for a moment after each change.
        local spark = g.signal(false)
        g.watch(function() return step.value end, function()
            spark.value = true
            g.after(0.35, function() spark.value = false end)
        end)

        local nodes: { any } = {}

        -- The lines, from the source to each row.
        for i, y in ROWS do
            table.insert(nodes, g.thread {
                overlay = true,
                attach = "corner",
                width = function() return u(1000) end,
                height = function() return u(800) end,
                stroke = function() return u(2) end,
                from_x = function() return u(SOURCE_X + SOURCE_W) end,
                from_y = function() return u(SOURCE_Y + SOURCE_H / 2) end,
                to_x = function() return u(ROW_X - 24) end,
                to_y = function() return u(y) end,
                bend_x = function() return u((SOURCE_X + SOURCE_W + ROW_X) / 2) end,
                bend_y = function() return u(y) end,
                node = function() return u(5) end,
                fill = function()
                    return if spark.value then t.colour("glow") else g.colour.alpha(t.colour("accent"), 0.28)
                end,
                color = style.c("accent"),
                transition = { fill = function() return if spark.value then "spark_" .. i else "dim" end },
            })
        end

        -- The source: the signal itself.
        table.insert(nodes, cap {
            x = SOURCE_X, y = SOURCE_Y, w = SOURCE_W, h = SOURCE_H, r = 60,
            fill_stops = function() return { { 0, g.colour.alpha(t.colour("accent"), 0.22) }, { 1, g.colour.alpha(t.colour("accent"), 0.06) } } end,
            rim_top = function() return g.colour.alpha(t.colour("glow"), 0.9) end,
            rim_bottom = function() return g.colour.alpha(t.colour("accent"), 0.25) end,
            shadow = function() return g.colour.alpha(t.colour("accent"), if spark.value then 0.55 else 0.18) end,
            shadow_blur = function() return u(50) end,
            transition = { shadow = function() return if spark.value then "flash" else "dim" end },
            direction = "column",
            align = "center",
            justify = "center",
            style.text("label", { text = "level", color = style.c("muted") }),
            style.text("number", {
                text = function() return string.format("%.2f", v()) end,
            }),
        })

        -- A row: its label, and what it draws from the value.
        local function row(i: number, label: string, node: any): any
            return g.box {
                overlay = true,
                attach = "corner",
                offset_x = function() return u(ROW_X) end,
                offset_y = function() return u(ROWS[i] - 55) end,
                width = function() return u(440) end,
                height = function() return u(110) end,
                direction = "row",
                align = "center",
                gap = function() return u(28) end,
                g.box {
                    width = function() return u(90) end,
                    style.text("label", { text = label, color = style.c("faint") }),
                },
                node,
            }
        end

        local function follow(i: number): string return "follow_" .. i end

        table.insert(nodes, row(1, "TEXT", style.text("title", {
            text = function() return math.floor(v() * 100 + 0.5) .. "%" end,
            color = style.c("text"),
        })))
        table.insert(nodes, row(2, "ARC", g.box {
            width = function() return u(92) end,
            height = function() return u(92) end,
            cap { overlay = true, attach = "center", w = 92, h = 92, stroke = function() return u(7) end,
                rim_top = style.c("line"), rim_bottom = style.c("line") },
            cap { overlay = true, attach = "center", w = 92, h = 92, stroke = function() return u(7) end,
                rim_top = style.c("glow"), rim_bottom = style.c("accent"),
                arc = v, transition = { arc = follow(2) } },
        }))
        table.insert(nodes, row(3, "WIDTH", g.box {
            width = function() return u(300) end,
            height = function() return u(14) end,
            cap { w = 300, h = 14, fill = style.c("line") },
            cap { w = function() return 300 * v() end, h = 14, fill_stops = style.accent_stops, fill_across = true,
                transition = { width = follow(3) } },
        }))
        table.insert(nodes, row(4, "COLOUR", g.box {
            direction = "row",
            gap = function() return u(14) end,
            cap {
                overlay = false, w = 64, h = 64,
                fill = function() return g.colour.mix(t.colour("accent2"), t.colour("accent"), v()) end,
                shadow = function() return g.colour.alpha(g.colour.mix(t.colour("accent2"), t.colour("accent"), v()), 0.5) end,
                shadow_blur = function() return u(30) end,
                transition = { fill = follow(4), shadow = follow(4) },
            },
            cap {
                overlay = false, w = 64, h = 64,
                fill = function() return g.colour.hsla((260 - 200 * v()) / 360, 0.65, 0.62, 1) end,
                transition = { fill = follow(4) },
            },
        }))
        table.insert(nodes, row(5, "WORDS", g.box {
            opacity = 1,
            style.text("lead", {
                text = function()
                    local x = v()
                    return if x < 0.33 then "Quiet." elseif x < 0.75 then "Busy, but fine." else "Nearly full!"
                end,
                italic = true,
                color = function()
                    local x = v()
                    return if x < 0.33 then t.colour("accent2") elseif x < 0.75 then t.colour("text") else "#F08A6C"
                end,
                transition = { color = follow(5) },
            }),
        }))

        local demo = { width = function() return u(1000) end, height = function() return u(800) end }
        for _, n in nodes do table.insert(demo, n) end

        return frame.new {
            number = ctx.number,
            kicker = "Reactivity",
            title = "One value, everywhere",
            caption = "A signal changes; only what reads it runs again, and the scene moves to the new value. No polling, no diffing, no frame when nothing changed.",
            arrived = ctx.arrived,
            demo = g.box(demo),
        }
    end,
}
