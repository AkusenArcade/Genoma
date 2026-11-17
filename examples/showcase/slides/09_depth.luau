--!strict
-- A third dimension, through a perspective: a ring of cards turning about
-- its axis, one card flipping to its back, and a panel unfolding from its
-- top edge on a spring.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")

local u = style.u
local cap = style.cap
local t = style.t

g.timing {
    turn = { duration = 1.0, curve = { 0.65, 0, 0.35, 1 } },
    flip = { duration = 0.9, curve = { 0.65, 0, 0.35, 1 } },
    flip_face = { duration = 0, delay = 0.45 },
    unfold3d = { spring = { stiffness = 120, damping = 12 } },
    fold3d = { duration = 0.5, curve = { 0.65, 0, 0.35, 1 } },
}

-- The ring's cards: each its own gradient.
local CARDS = {
    { "#5ED3C1", "#1D3F7A" }, { "#B9DE6B", "#3E6B1E" }, { "#F2C14E", "#8A3B12" },
    { "#E05E7A", "#4A1238" }, { "#9B8CFF", "#2A1F6B" }, { "#7FD8FF", "#14506B" },
    { "#DBF4A7", "#5E8A2A" },
}
local RADIUS = 300
local CARD_W, CARD_H = 170, 230

return {
    key = "depth",
    kicker = "Depth",
    title = "A third dimension",
    caption = "Any subtree turns about either axis through a perspective, moves toward the eye or away, about a pivot of its own. Cards that turn, panels that unfold, on springs.",
    duration = 11,
    code = [[
-- A ring of cards: each placed by its angle.
g.capsule {
    offset_x = function() return math.sin(a()) * 300 end,
    depth = function() return (math.cos(a()) - 1) * 300 end,
    rotate_y = function() return math.deg(a()) end,
    perspective = 1400,
    transition = { offset_x = "turn", depth = "turn",
                   rotate_y = "turn" },
}

-- A panel unfolding from its top edge.
g.capsule {
    pivot_y = 0,
    rotate_x = function() return if open.value then 0 else -90 end,
    transition = { rotate_x = "unfold" },
}]],

    build = function(ctx: { [string]: any }): any
        local spin = g.signal(0)
        g.timer { every = 1.6, on_tick = function()
            if g.untracked(ctx.arrived) then spin.value += 1 end
        end }
        local flipped = style.cycle(ctx.arrived, 2.2, 2)
        local unfolded = style.cycle(ctx.arrived, 2.6, 2)

        local n = #CARDS
        local function angle(i: number): number return (i - 1 + spin.value) * 2 * math.pi / n end

        -- Back to front, so the near cards are drawn over the far ones.
        local function order(): { number }
            local out = {}
            for i = 1, n do table.insert(out, i) end
            table.sort(out, function(a, b) return math.cos(angle(a)) < math.cos(angle(b)) end)
            return out
        end

        local function card(i: () -> number): any
            local k = g.untracked(i)
            local c = CARDS[k]
            return cap {
                overlay = true,
                attach = "center",
                w = CARD_W, h = CARD_H, r = 26,
                offset_x = function() return u(math.sin(angle(k)) * RADIUS) end,
                depth = function() return u((math.cos(angle(k)) - 1) * RADIUS * 0.9) end,
                rotate_y = function() return math.deg(angle(k)) end,
                perspective = function() return u(1400) end,
                opacity = function() return 0.4 + 0.6 * (math.cos(angle(k)) + 1) / 2 end,
                fill = c[1],
                fill_bottom = c[2],
                fill_angle = 160,
                rim_top = "#FFFFFF88",
                rim_bottom = "#FFFFFF18",
                transition = { offset_x = "turn", depth = "turn", rotate_y = "turn", opacity = "turn" },
                cap { overlay = true, attach = "center", w = 70, h = 70, fill = "#FFFFFF30", rim_top = "#FFFFFFAA", rim_bottom = "#FFFFFF22" },
            }
        end

        local function front(): boolean return flipped.value == 1 end
        local open = function() return unfolded.value == 1 end

        return frame.new {
            number = ctx.number,
            kicker = "Depth",
            title = "A third dimension",
            caption = "Any subtree turns about either axis through a perspective, moves toward the eye or away, about a pivot of its own. Cards that turn, panels that unfold, on springs.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                -- The ring.
                g.box {
                    overlay = true,
                    attach = "corner",
                    width = function() return u(1000) end,
                    height = function() return u(420) end,
                    g.each {
                        overlay = true,
                        attach = "center",
                        width = function() return u(1000) end,
                        height = function() return u(420) end,
                        items = order,
                        key = function(k: number) return tostring(k) end,
                        row = card,
                    },
                },
                -- The card that flips: its front and back trade places at
                -- the half turn, edge on.
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(140) end,
                    offset_y = function() return u(470) end,
                    direction = "column",
                    align = "center",
                    gap = function() return u(18) end,
                    cap {
                        overlay = false,
                        w = 240, h = 260, r = 30,
                        rotate_y = function() return if front() then 0 else 180 end,
                        perspective = function() return u(900) end,
                        fill = function() return if front() then t.colour("accent") else t.colour("accent2") end,
                        fill_bottom = function() return if front() then t.colour("deep") else "#14506B" end,
                        rim_top = "#FFFFFF88",
                        rim_bottom = "#FFFFFF18",
                        shadow = "#00000099",
                        shadow_blur = function() return u(50) end,
                        shadow_y = function() return u(20) end,
                        transition = { rotate_y = "flip", fill = "flip_face", fill_bottom = "flip_face" },
                        cap { overlay = true, attach = "center",
                            w = function() return if front() then 90 else 140 end,
                            h = function() return if front() then 140 else 60 end,
                            fill = "#FFFFFF38", rim_top = "#FFFFFFAA", rim_bottom = "#FFFFFF22",
                            transition = { width = "flip_face", height = "flip_face" } },
                    },
                    style.text("label", { text = "ROTATE_Y", color = style.c("muted") }),
                },
                -- The panel that unfolds from its top edge.
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(520) end,
                    offset_y = function() return u(470) end,
                    direction = "column",
                    align = "center",
                    gap = function() return u(18) end,
                    cap {
                        overlay = false,
                        w = 380, h = 70, r = 35,
                        fill = function() return g.colour.lift(t.colour("background"), 0.12, true) end,
                        rim_top = "#FFFFFF66",
                        rim_bottom = "#FFFFFF10",
                        cap {
                            overlay = true,
                            attach = "below",
                            y = 14,
                            w = 380, h = 190, r = 28,
                            pivot_y = 0,
                            rotate_x = function() return if open() then 0 else -90 end,
                            perspective = function() return u(900) end,
                            opacity = function() return if open() then 1 else 0 end,
                            fill = function() return g.colour.lift(t.colour("background"), 0.09, true) end,
                            fill_bottom = function() return g.colour.lift(t.colour("background"), 0.03, true) end,
                            rim_top = "#FFFFFF55",
                            rim_bottom = "#FFFFFF10",
                            transition = {
                                rotate_x = function() return if open() then "unfold3d" else "fold3d" end,
                                opacity = function() return if open() then "fade" else "fold3d" end,
                            },
                            direction = "column",
                            gap = function() return u(16) end,
                            padding = function() return u(28) end,
                            cap { overlay = false, w = 260, h = 16, fill = style.c("line") },
                            cap { overlay = false, w = 320, h = 16, fill = style.c("line") },
                            cap { overlay = false, w = 200, h = 16, fill_stops = style.accent_stops, fill_across = true },
                        },
                    },
                    g.box { height = function() return u(200) end },
                    style.text("label", { text = "ROTATE_X  ·  PIVOT AT THE TOP", color = style.c("muted") }),
                },
            },
        }
    end,
}
