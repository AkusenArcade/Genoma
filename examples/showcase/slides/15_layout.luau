--!strict
-- Layout and input: chips that flow onto the next line as their column
-- narrows, a carousel that snaps to a card, a list of ten thousand rows of
-- which only those in view exist, and a card to drag by hand (the deck
-- itself turns on a swipe, and a finger scrolls any list).
local g = require("@genoma")
local style = require("style")
local frame = require("frame")
local kit = require("@genoma/kit")

local u = style.u
local cap = style.cap
local t = style.t

g.timing { drag_back = { spring = { stiffness = 180, damping = 14 } }, held = { duration = 0 } }

local TAGS = { "flow", "snap", "scroll", "fling", "g.list", "swipe", "pinch", "long press", "drag & drop",
    "touch", "IME", "focus", "Tab", "wheel", "keyboard" }
local CARDS = { { "#5ED3C1", "#1D3F7A" }, { "#B9DE6B", "#3E6B1E" }, { "#F2C14E", "#8A3B12" }, { "#E05E7A", "#4A1238" }, { "#9B8CFF", "#2A1F6B" } }
local ROWS = 10000
local REVEAL = { 1, 2481, 7350, 9999, 120 }
local TYPED = "Hello, Genoma."

return {
    key = "layout",
    kicker = "Layout & input",
    title = "Under the hand",
    caption = "Rows, columns and flow; scrolling with fling and snap; lists that mount only what shows. Swipes, pinches, long presses, drag and drop, touch, and a text field with input methods.",
    duration = 11,
    code = [[
-- Chips that go on to the next line.
g.box { flow = true, gap = 10, gap_lines = 10,
        width = function() return column() end, chips }

-- A carousel that settles on a card.
g.box { scroll = "x", snap = "center", cards }

-- Ten thousand rows; only those in view exist.
g.list {
    items = rows, row_height = 48,
    reveal = function() return wanted.value end,
    row = function(item) return row(item) end,
}]],

    build = function(ctx: { [string]: any }): any
        local narrow = style.cycle(ctx.arrived, 1.8, 2)
        local card = style.cycle(ctx.arrived, 1.4, #CARDS)
        local reveal = style.cycle(ctx.arrived, 1.6, #REVEAL)
        local typed = g.signal(0)
        g.timer { every = 0.11, on_tick = function()
            if g.untracked(ctx.arrived) then typed.value = (typed.value + 1) % (#TYPED + 12) end
        end }

        -- The chips.
        local chips = {
            flow = true,
            direction = "row",
            gap = function() return u(10) end,
            gap_lines = function() return u(12) end,
            width = function() return u(if narrow.value == 1 then 440 else 300) end,
            transition = { width = "soft" },
        }
        for i, tag in TAGS do
            table.insert(chips, cap {
                overlay = false,
                h = 40,
                padding_x = function() return u(16) end,
                fill = function() return g.colour.alpha(if i % 3 == 0 then t.colour("accent2") else t.colour("accent"), 0.14) end,
                rim_top = function() return g.colour.alpha(if i % 3 == 0 then t.colour("accent2") else t.colour("accent"), 0.6) end,
                rim_bottom = function() return g.colour.alpha(t.colour("accent"), 0.15) end,
                style.text("body", { text = tag, color = style.c("text") }),
            })
        end

        -- The carousel's cards.
        local strip = {
            direction = "row",
            gap = function() return u(20) end,
            padding_x = function() return u(90) end,
        }
        for _, c in CARDS do
            table.insert(strip, cap {
                overlay = false,
                w = 220, h = 150, r = 26,
                fill = c[1], fill_bottom = c[2], fill_angle = 150,
                rim_top = "#FFFFFF88", rim_bottom = "#FFFFFF18",
            })
        end

        local rows = {}
        for i = 1, ROWS do rows[i] = i end

        -- The card to drag: it follows the hand and springs home.
        local held = g.signal(false)
        local at = g.signal({ 0, 0 })
        local grip = { 0, 0 }

        return frame.new {
            number = ctx.number,
            kicker = "Layout & input",
            title = "Under the hand",
            caption = "Rows, columns and flow; scrolling with fling and snap; lists that mount only what shows. Swipes, pinches, long presses, drag and drop, touch, and a text field with input methods.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                frame.tile(0, 0, 480, 390, "FLOW", function(w, h)
                    return g.box {
                        width = w, height = h,
                        padding = function() return u(24) end,
                        align = "start",
                        justify = "start",
                        g.box(chips),
                    }
                end),
                frame.tile(500, 0, 500, 390, "SCROLL  ·  SNAP", function(w, h)
                    return g.box {
                        width = w, height = h,
                        align = "center",
                        justify = "center",
                        g.box {
                            width = w,
                            height = function() return u(190) end,
                            scroll = "x",
                            snap = "center",
                            scroll_x = function() return u((card.value - 1) * 240) end,
                            transition = { scroll_x = "soft" },
                            align = "center",
                            g.box(strip),
                        },
                    }
                end),
                frame.tile(0, 410, 480, 360, "10 000 ROWS  ·  G.LIST", function(w, h)
                    return g.list {
                        width = w,
                        height = function() return h() - u(64) end,
                        row_height = g.untracked(function() return u(48) end),
                        items = function() return rows end,
                        key = function(i: number) return tostring(i) end,
                        reveal = function() return REVEAL[reveal.value] end,
                        row = function(item: () -> number)
                            local function n(): number return item() end
                            local function lit(): boolean return n() == REVEAL[reveal.value] end
                            return g.capsule {
                                width = w,
                                height = function() return u(48) end,
                                radius = 0,
                                padding_x = function() return u(24) end,
                                direction = "row",
                                align = "center",
                                justify = "between",
                                fill = function() return if lit() then g.colour.alpha(t.colour("accent"), 0.18) else "#00000000" end,
                                rim_top = style.CLEAR,
                                rim_bottom = style.CLEAR,
                                style.text("label", { text = function() return string.format("ROW %05d", n()) end, color = style.c("text") }),
                                cap { overlay = false, w = function() return 40 + (n() * 37) % 160 end, h = 8, fill = function()
                                    return g.colour.alpha(t.colour("accent2"), if lit() then 0.9 else 0.35) end },
                            }
                        end,
                    }
                end),
                frame.tile(500, 410, 500, 360, "DRAG  ·  TYPE", function(w, h)
                    return g.box {
                        width = w, height = h,
                        direction = "column",
                        align = "center",
                        justify = "center",
                        gap = function() return u(26) end,
                        cap {
                            overlay = false,
                            w = 200, h = 110, r = 28,
                            role = "button",
                            cursor = function() return if held.value then "grabbing" else "grab" end,
                            fill_stops = function() return { { 0, t.colour("glow") }, { 1, t.colour("accent2") } } end,
                            fill_angle = 135,
                            rim_top = "#FFFFFFAA", rim_bottom = "#FFFFFF22",
                            shadow = "#00000088",
                            shadow_blur = function() return u(if held.value then 50 else 24) end,
                            offset_x = function() return at.value[1] end,
                            offset_y = function() return at.value[2] end,
                            transition = {
                                offset_x = function() return if held.value then "held" else "drag_back" end,
                                offset_y = function() return if held.value then "held" else "drag_back" end,
                            },
                            on_press = function(_: number, x: number, y: number) grip = { x, y } end,
                            on_drag_start = function() held.value = true end,
                            on_drag = function(x: number, y: number) at.value = { x - grip[1], y - grip[2] } end,
                            on_drag_end = function()
                                g.batch(function()
                                    held.value = false
                                    at.value = { 0, 0 }
                                end)
                            end,
                            style.text("label", { text = "DRAG ME", color = "#0B0F0A" }),
                        },
                        kit.field {
                            width = g.untracked(function() return u(360) end),
                            placeholder = "Type here",
                            text = function() return string.sub(TYPED, 1, typed.value) end,
                        },
                    }
                end),
            },
        }
    end,
}
