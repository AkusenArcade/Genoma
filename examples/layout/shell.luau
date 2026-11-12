--!strict
-- Layout beyond a row and a column: children that flow onto lines,
-- overlays pinned to their parent's edges, and scrolling Genoma does by
-- itself — down a page, across a carousel that snaps to its cards, both
-- ways over a map — one scroller inside another, each taking only its own
-- axis and handing the rest on.
--
--   genoma examples/layout
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme

local function colour(name: string): () -> string
    return function() return theme.get(name) end
end

local function heading(text: string): any
    return g.text { size = 12, weight = 600, tracking = 0.06, color = colour("faint"), text = string.upper(text) }
end

-- ---- Flow ------------------------------------------------------------------

local tags = {
    "rust", "luau", "wayland", "niri", "shaders", "wgpu", "taffy", "layer shell",
    "reactive", "springs", "particles", "morph", "blur", "notifications", "a much longer tag",
}

local chips = {}
for _, tag in tags do
    table.insert(chips, g.capsule {
        height = 28,
        padding_x = 12,
        fill = colour("control"),
        g.text { size = 13, color = colour("text"), text = tag },
    })
end

-- ---- Pinned overlays --------------------------------------------------------

local pinned = g.capsule {
    width = 600,
    height = 140,
    radius = 12,
    fill = colour("control"),
    g.text { size = 14, color = colour("faint"), text = "A card, its parts pinned to its edges" },
    -- Two edges: a corner.
    g.capsule {
        overlay = true,
        top = 10,
        right = 10,
        height = 22,
        padding_x = 10,
        fill = colour("accent"),
        g.text { size = 12, weight = 600, color = colour("on_accent"), text = "NEW" },
    },
    -- Both sides of an axis and no width: stretched between them.
    g.capsule {
        overlay = true,
        left = 8,
        right = 8,
        bottom = 8,
        height = 30,
        radius = 6,
        fill = colour("selection"),
        g.text { size = 12, color = colour("text"), text = "left = 8, right = 8, bottom = 8" },
    },
    -- One edge: centred along the other axis.
    g.capsule {
        overlay = true,
        left = 12,
        width = 8,
        height = 40,
        radius = 4,
        fill = colour("accent"),
    },
}

-- ---- Scrolling ----------------------------------------------------------------

local card_colours = { "#7aa2f7", "#bb9af7", "#9ece6a", "#e0af68", "#f7768e", "#7dcfff", "#ff9e64", "#c0caf5" }
local cards = {}
for i, c in card_colours do
    table.insert(cards, g.capsule {
        width = 180,
        height = 110,
        radius = 14,
        direction = "column",
        align = "start",
        justify = "end",
        padding = 14,
        fill = c,
        g.text { size = 22, weight = 700, color = "#11131a", text = tostring(i) },
    })
end

local where = g.signal("1")
local carousel = g.box {
    id = "carousel",
    scroll = "x",
    snap = "center",
    width = 560,
    height = 120,
    gap = 12,
    padding_x = 190,
    on_scroll = function(x, _)
        -- The card in the middle: cards 180 wide, 12 apart.
        where.value = tostring(math.clamp(math.floor(x / 192 + 0.5) + 1, 1, #cards))
    end,
    table.unpack(cards),
}

-- A map larger than its window, both ways.
local tiles = {}
for row = 1, 8 do
    for column = 1, 12 do
        local shade = ((row + column) % 2 == 0)
        table.insert(tiles, g.capsule {
            width = 60,
            height = 60,
            radius = 6,
            fill = function() return if shade then theme.get("control") else theme.get("control_hover") end,
            g.text { size = 11, color = colour("faint"), text = `{row},{column}` },
        })
    end
end
local map = g.box {
    id = "map",
    scroll = "both",
    width = 560,
    height = 180,
    g.box { width = 12 * 64, flow = true, gap = 4, align = "start", table.unpack(tiles) },
}

local rows = {}
for i = 1, 12 do
    table.insert(rows, g.capsule {
        height = 36,
        padding_x = 14,
        justify = "start",
        fill = colour("control"),
        g.text { size = 14, color = colour("text"), text = `Row {i}` },
    })
end

local page = g.box {
    id = "page",
    scroll = "y",
    direction = "column",
    align = "stretch",
    width = 600,
    height = 380,
    gap = 12,
    padding_x = 20,
    g.box {
        gap = 10,
        heading("Carousel"),
        g.text { size = 12, color = colour("faint"), text = function() return `card {where.value} in the middle` end },
    },
    carousel,
    heading("Map"),
    map,
    heading("Rows"),
    table.unpack(rows),
}

return g.surface {
    anchor = "top",
    margin = { top = 60 },
    namespace = "genoma-layout",

    g.capsule {
        direction = "column",
        align = "start",
        gap = 14,
        padding = 20,
        radius = 18,
        fill = colour("background"),
        rim_top = colour("line"),
        rim_bottom = colour("line"),

        heading("Flow"),
        g.box { width = 600, flow = true, gap = 8, gap_lines = 8, align = "start", table.unpack(chips) },

        heading("Pinned"),
        pinned,

        heading("Scrolling: a page, a carousel and a map inside it"),
        g.capsule { width = 640, height = 380, radius = 12, fill = colour("control_press"), page },
    },
}
