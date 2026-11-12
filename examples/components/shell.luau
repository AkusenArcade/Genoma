--!strict
-- The kit's larger pieces: a colour picker (the swatch retints the kit's
-- accent, live), a calendar with marked days and a range, a tree that
-- opens as it is walked, and a file picker.
--
--   genoma examples/components
local g = require("@genoma")
local kit = require("@genoma/kit")
local calendar = require("@genoma/kit/calendar")
local tree = require("@genoma/kit/tree")
local color_picker = require("@genoma/kit/color_picker")
local file_picker = require("@genoma/kit/file_picker")

local theme = kit.theme

local function colour(name: string): () -> string
    return function() return theme.get(name) end
end

local function text(t: () -> string, faint: boolean?): any
    return g.text { size = 13, color = colour(if faint then "faint" else "text"), max_width = 560, wrap = true, text = t }
end

local page = g.signal(1)

-- ---- Colour ----------------------------------------------------------------------

local accent = g.signal("#7aa2f7")
local picked = {
    id = "picker",
    color = function() return accent.value end,
    on_change = function(hex: string)
        accent.value = hex
        theme.set { accent = hex, selection = theme.alpha(hex, 0.2) }
    end,
    swatches = { "#7aa2f7", "#bb9af7", "#9ece6a", "#e0af68", "#f7768e", "#7dcfff" },
}
local overlay = g.signal("#7aa2f733")
local colour_page = g.box {
    present = function() return page.value == 1 end,
    gap = 28,
    align = "start",
    color_picker(picked),
    g.box {
        direction = "column",
        align = "start",
        gap = 12,
        text(function() return "The kit's accent follows the picker: buttons, the tabs, the selection." end, true),
        g.box {
            gap = 8,
            kit.button { text = "Accent", kind = "accent" },
            kit.toggle { name = "On", on = true },
        },
        text(function() return "A swatch that opens a picker, with opacity:" end, true),
        g.box {
            gap = 10,
            color_picker.button {
                alpha = true,
                color = function() return overlay.value end,
                on_change = function(hex: string) overlay.value = hex end,
            },
            text(function() return overlay.value end),
        },
    },
}

-- ---- Calendar ----------------------------------------------------------------------

local today = calendar.today()
local day = g.signal(today :: any)
local trip = g.signal({ from = calendar.add(today, 3), to = calendar.add(today, 8) } :: any)
-- Something on every few days: the marks are the data, coloured by kind.
local function marks(d: calendar.Date): string?
    local s = calendar.stamp(d)
    if s % 7 == 0 then return "#e0af68" end
    if s % 11 == 0 then return "#f7768e" end
    return nil
end
local day_view = {
    date = function() return day.value end,
    on_select = function(d) day.value = d end,
    marks = marks,
}
local calendar_page = g.box {
    present = function() return page.value == 2 end,
    gap = 28,
    align = "start",
    calendar(day_view),
    g.box {
        direction = "column",
        align = "start",
        gap = 8,
        calendar {
            range = true,
            first_weekday = 7,
            min = today,
            date = function() return trip.value end,
            on_select = function(r) trip.value = r end,
        },
        text(function()
            local r = trip.value
            if r.from == nil then return "" end
            local function say(d) return `{d.day}/{d.month}` end
            return if r.to then `From {say(r.from)} to {say(r.to)}` else `From {say(r.from)}…`
        end, true),
    },
}

-- ---- Tree ----------------------------------------------------------------------------

local function node(name: string, kids: { any }?): any return { name = name, kids = kids } end
local project = {
    node("crates", {
        node("genoma", { node("main.rs"), node("wayland.rs"), node("wayland", { node("gesture.rs"), node("wm.rs") }) }),
        node("genoma-scene", { node("lib.rs"), node("schema.rs"), node("distance.rs") }),
        node("genoma-script", { node("lib.rs"), node("scroll.rs"), node("colour.rs"), node("list.rs") }),
        node("genoma-render", { node("lib.rs"), node("gpu.rs"), node("shaders", {}) }),
    }),
    node("kit", { node("button.luau"), node("calendar.luau"), node("tree.luau"), node("themes.luau") }),
    node("examples", {}),
    node("README.md"),
    node("CHANGELOG.md"),
}
local chosen = g.signal("crates/genoma-script" :: any)
local function path_of(item: any): string return item.path or item.name end
-- Paths as keys, so two lib.rs are two rows.
local function with_paths(list: { any }, prefix: string)
    for _, item in list do
        item.path = if prefix == "" then item.name else prefix .. "/" .. item.name
        if item.kids then with_paths(item.kids, item.path) end
    end
end
with_paths(project, "")
local walked = {
    width = 560,
    height = 330,
    roots = function() return project end,
    children = function(item) return item.kids end,
    key = path_of,
    label = function(item) return item.name end,
    detail = function(item) return if item.kids then `{#item.kids}` else nil end,
    expanded = nil,
    selected = function() return chosen.value end,
    on_select = function(key) chosen.value = key end,
}
local tree_page = g.box {
    present = function() return page.value == 3 end,
    direction = "column",
    align = "start",
    gap = 8,
    tree(walked),
    text(function() return `Chosen: {chosen.value or "nothing"}. Click the panel, then the arrows walk it: Right opens, Left closes.` end, true),
}

-- ---- Files -----------------------------------------------------------------------------

local files_said = g.signal("")
local files_page = g.box {
    present = function() return page.value == 4 end,
    direction = "column",
    align = "start",
    gap = 8,
    file_picker {
        width = 700,
        height = 300,
        folder = "~",
        preview = true,
        on_choose = function(paths) files_said.value = "Chose " .. table.concat(paths, ", ") end,
        on_cancel = function() files_said.value = "Cancelled" end,
    },
    text(function() return files_said.value end, true),
}

return g.surface {
    anchor = "top",
    margin = { top = 60 },
    namespace = "genoma-components",
    -- Pressed, it takes the keys: the arrows walk the page shown.
    keyboard = "on_demand",

    g.capsule {
        direction = "column",
        align = "start",
        gap = 16,
        padding = 20,
        radius = 18,
        fill = colour("background"),
        rim_top = colour("line"),
        rim_bottom = colour("line"),
        on_key = function(name: string, _, mods)
            if page.value == 1 then color_picker.key(picked, name)
            elseif page.value == 2 then calendar.key(day_view, name, mods)
            elseif page.value == 3 then tree.key(walked, name) end
        end,
        kit.tabs {
            items = { "Colour", "Calendar", "Tree", "Files" },
            selected = function() return page.value end,
            on_select = function(i) page.value = i end,
        },
        colour_page,
        calendar_page,
        tree_page,
        files_page,
    },
}
