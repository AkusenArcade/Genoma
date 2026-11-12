--!strict
-- Gestures, with a pointer, a touchpad or a screen: a card thrown along a
-- track that settles on one of three stops, a picture zoomed with two
-- fingers (or the wheel), and rows held to pin them and swiped away.
-- What was last recognised is told at the foot.
--
--   genoma examples/gestures
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme
local gestures = kit.gestures

local function colour(name: string): () -> string
    return function() return theme.get(name) end
end

local function heading(text: string): any
    return g.text { size = 12, weight = 600, tracking = 0.06, color = colour("faint"), text = string.upper(text) }
end

local said = g.signal("Try a gesture.")

-- ---- Thrown -------------------------------------------------------------------

local stops = { { 0, 0 }, { 230, 0 }, { 460, 0 } }
local card = gestures.draggable {
    axis = "x",
    bounds = { 0, 460, 0, 0 },
    snap = stops,
    on_release = function(x)
        said.value = `thrown: settles at {math.floor(x + 0.5)}`
    end,
}

local track = g.capsule {
    width = 600,
    height = 100,
    radius = 14,
    justify = "start",
    padding_x = 10,
    fill = colour("control_press"),
    -- The stops, where the card can rest.
    g.capsule { overlay = true, left = 70, width = 6, height = 6, radius = 3, fill = colour("line") },
    g.capsule { overlay = true, left = 300, width = 6, height = 6, radius = 3, fill = colour("line") },
    g.capsule { overlay = true, left = 530, width = 6, height = 6, radius = 3, fill = colour("line") },
    g.capsule(card.attach {
        id = "card",
        width = 120,
        height = 80,
        radius = 12,
        offset_x = card.x,
        cursor = function() return if card.dragging() then "grabbing" else "grab" end,
        fill = colour("accent"),
        g.text { size = 14, weight = 600, color = colour("on_accent"), text = "Throw me" },
    }),
}

-- ---- Pinched --------------------------------------------------------------------

local zoom = gestures.zoomable { min = 0.5, max = 2.5, wheel = true }

local picture = g.capsule {
    width = 600,
    height = 220,
    radius = 14,
    clip = true,
    fill = colour("control_press"),
    g.capsule(zoom.attach {
        id = "picture",
        width = function() return 200 * zoom.scale() end,
        height = function() return 130 * zoom.scale() end,
        radius = function() return 14 * zoom.scale() end,
        fill_stops = { { 0, "#7aa2f7" }, { 0.5, "#bb9af7" }, { 1, "#f7768e" } },
        fill_angle = function() return 135 + zoom.rotation() end,
        g.text {
            size = 14,
            weight = 600,
            tabular = true,
            color = "#11131a",
            text = function() return string.format("×%.2f  %+.0f°", zoom.scale(), zoom.rotation()) end,
        },
    }),
}

-- ---- Held and swiped ----------------------------------------------------------------

local names = { "Grocery list", "Call the plumber", "Pay the rent", "Book the train" }
local pinned = g.signal({} :: { [number]: boolean })
local rows = {}
local swipers = {}
for i, name in names do
    local row = gestures.dismissable {
        size = 560,
        on_dismiss = function(way) said.value = `dismissed "{name}" {way}` end,
    }
    swipers[i] = row
    table.insert(rows, g.capsule(row.attach {
        id = "row-" .. i,
        width = 560,
        height = 40,
        padding_x = 14,
        justify = "start",
        gap = 10,
        offset_x = row.offset,
        opacity = row.opacity,
        present = function() return not row.gone() or math.abs(row.offset()) < 600 end,
        fill = function() return if pinned.value[i] then theme.get("selection") else theme.get("control") end,
        on_long_press = function()
            local next = table.clone(pinned.value)
            next[i] = not next[i]
            pinned.value = next
            said.value = `held: "{name}" {if next[i] then "pinned" else "unpinned"}`
        end,
        on_click = function() said.value = `clicked "{name}"` end,
        g.text { size = 14, color = colour("text"), text = name },
        g.text { size = 12, color = colour("faint"), text = function() return if pinned.value[i] then "pinned" else "" end },
    }))
end

return g.surface {
    anchor = "top",
    margin = { top = 60 },
    namespace = "genoma-gestures",

    g.capsule {
        direction = "column",
        align = "start",
        gap = 14,
        padding = 20,
        radius = 18,
        fill = colour("background"),
        rim_top = colour("line"),
        rim_bottom = colour("line"),
        -- Any swipe or pinch nobody inside took comes up to here.
        on_swipe = function(way, speed) said.value = `swipe {way}, {math.floor(speed)} a second` end,

        heading("Thrown, and settling on a stop"),
        track,
        heading("Pinched, or the wheel"),
        picture,
        g.box {
            gap = 10,
            kit.button { text = "Reset zoom", on_click = function() zoom.reset() end },
        },
        heading("Held to pin, swiped away"),
        g.box { direction = "column", gap = 8, align = "start", table.unpack(rows) },
        g.box {
            gap = 10,
            kit.button {
                text = "Bring them back",
                on_click = function()
                    for _, r in swipers do r.restore() end
                end,
            },
            g.text { size = 13, color = colour("faint"), text = function() return said.value end },
        },
    },
}
