--!strict
-- Type: one sentence set in runs of their own (bold, italic, a colour, a
-- word struck through); the display face from light to heavy; a paragraph
-- that wraps again as its column changes width; a name that ends in an
-- ellipsis where its room ends.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")

local u = style.u
local cap = style.cap
local t = style.t

local SENTENCE = "Words with a voice: weight, slant, colour, and second thoughts."
local PARAGRAPH = "Text is shaped by cosmic-text and drawn from a glyph atlas on the GPU. It wraps at words, keeps its runs, and ends in an ellipsis where its room ends."
local WIDTHS = { 560, 380, 470 }

-- Where a word sits in the sentence, as runs count it: from 1, end included.
local function find(word: string): (number, number)
    local a, b = string.find(SENTENCE, word, 1, true)
    return a :: number, b :: number
end

return {
    key = "type",
    kicker = "Type",
    title = "Words with a voice",
    caption = "Any installed font, its weights and slant, tracking, runs of styles inside one text, wrapping at words and an ellipsis where the room ends.",
    duration = 10,
    code = [[
g.text {
    text = "Words with a voice: weight, slant, colour…",
    font = "Spectral", size = 52,
    runs = {
        { from = 21, to = 26, weight = 800 },
        { from = 29, to = 33, italic = true },
        { from = 36, to = 41, color = "#5ed3c1" },
        { from = 48, to = 62, strike = true },
    },
}

g.text { text = paragraph, wrap = true,
         max_width = function() return column() end }]],

    build = function(ctx: { [string]: any }): any
        local width = style.cycle(ctx.arrived, 1.8, #WIDTHS)
        local function column(): number return WIDTHS[width.value] end

        local runs = {}
        do
            local a, b = find("weight")
            table.insert(runs, { from = a, to = b, weight = 800 })
            a, b = find("slant")
            table.insert(runs, { from = a, to = b, italic = true })
            a, b = find("colour")
            table.insert(runs, { from = a, to = b, color = "#5ED3C1" })
            a, b = find("second thoughts")
            table.insert(runs, { from = a, to = b, strike = true, color = "#F08A6C" })
        end

        local weights = { direction = "row", gap = function() return u(26) end, align = "end" }
        for _, w in { 400, 500, 600, 700, 800, 900 } do
            table.insert(weights, g.box {
                direction = "column",
                align = "center",
                gap = function() return u(6) end,
                g.text { text = "Ag", font = style.display, weight = w, size = function() return math.floor(u(72)) end, color = style.c("text") },
                style.text("label", { text = tostring(w), color = style.c("faint") }),
            })
        end

        return frame.new {
            number = ctx.number,
            kicker = "Type",
            title = "Words with a voice",
            caption = "Any installed font, its weights and slant, tracking, runs of styles inside one text, wrapping at words and an ellipsis where the room ends.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(20) end,
                    offset_y = function() return u(20) end,
                    g.text {
                        text = SENTENCE,
                        font = style.body,
                        size = function() return math.floor(u(54)) end,
                        wrap = true,
                        max_width = function() return u(940) end,
                        color = style.c("text"),
                        runs = runs,
                    },
                },
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(20) end,
                    offset_y = function() return u(220) end,
                    g.box(weights),
                },
                -- The column that changes width, and the paragraph in it.
                cap {
                    x = 20, y = 430,
                    w = function() return column() + 60 end,
                    h = 340, r = 30,
                    fill = function() return g.colour.alpha(t.colour("text"), 0.05) end,
                    rim_top = function() return g.colour.alpha("#ffffff", 0.25) end,
                    rim_bottom = function() return g.colour.alpha("#ffffff", 0.05) end,
                    transition = { width = "firm" },
                    padding = function() return u(30) end,
                    align = "start",
                    justify = "start",
                    style.text("lead", {
                        text = PARAGRAPH,
                        wrap = true,
                        max_width = function() return u(column()) end,
                        color = style.c("muted"),
                    }),
                },
                -- A name in a pill that is too small for it.
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(690) end,
                    offset_y = function() return u(470) end,
                    direction = "column",
                    gap = function() return u(16) end,
                    style.text("label", { text = "TRACKING", tracking = 0.6, color = style.c("accent") }),
                    style.text("label", { text = "AN ELLIPSIS", color = style.c("faint") }),
                    cap {
                        overlay = false,
                        w = 260, h = 56,
                        fill = function() return g.colour.alpha(t.colour("accent2"), 0.14) end,
                        rim_top = function() return g.colour.alpha(t.colour("accent2"), 0.6) end,
                        rim_bottom = function() return g.colour.alpha(t.colour("accent2"), 0.15) end,
                        padding_x = function() return u(22) end,
                        style.text("body", {
                            text = "A name much longer than its room",
                            max_width = function() return u(216) end,
                            color = style.c("text"),
                        }),
                    },
                },
            },
        }
    end,
}
