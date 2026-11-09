--!strict
-- The component kit, every piece on one panel: buttons of each kind,
-- switches, a slider driving a progress bar, a dropdown that retints the
-- whole kit, a menu, a tooltip, and a list of a thousand rows.
--
--   genoma examples/kit
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme

local accents = {
    { name = "Blue", colour = "#7aa2f7" },
    { name = "Green", colour = "#8fcf8a" },
    { name = "Amber", colour = "#e6b062" },
    { name = "Rose", colour = "#e88aa8" },
}

local accent = g.signal(1)
local level = g.signal(0.4)
local wifi = g.signal(true)
local bluetooth = g.signal(false)
local said = g.signal("Nothing done yet.")
local menu_open = g.signal(false)
local chosen = g.signal(1 :: number?)
local page = g.signal(1)
local name = g.signal("")
local asking = g.signal(false)

local rows = {}
for i = 1, 1000 do
    rows[i] = { id = i, name = `Item {i}`, size = `{(i * 37) % 900 + 100} KB` }
end

local function heading(text: string): any
    return g.text {
        size = 11,
        weight = 600,
        tracking = 0.08,
        color = function() return theme.get("faint") end,
        text = string.upper(text),
    }
end

local function line(...: any): any
    local row: { [any]: any } = { ... }
    row.gap = 10
    return g.box(row)
end

-- A surface of its own, over everything while it asks.
local ask = kit.dialog {
    present = function() return asking.value end,
    on_close = function() asking.value = false end,
    title = "Delete everything?",
    text = "The thousand rows go, and cannot be brought back. This is an example: nothing is deleted.",
    actions = {
        { text = "Cancel" },
        { text = "Delete", kind = "danger", on_click = function() said.value = "Deleted everything (not really)." end },
    },
}

local panel = g.surface {
    -- Hung from the top: the tabs stay where they are while the page below
    -- them changes height.
    anchor = "top",
    margin = { top = 120 },
    keyboard = "on_demand",
    namespace = "genoma-kit",

    g.capsule {
        -- Once the panel has the keys (a click on it), Tab and the arrows
        -- walk its controls, Return and space press them.
        navigate = true,
        focus_color = function() return theme.get("accent") end,
        direction = "column",
        align = "start",
        gap = 14,
        padding = 18,
        radius = 14,
        width = 460,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,

        kit.tabs {
            items = { "Controls", "About" },
            selected = function() return page.value end,
            on_select = function(i) page.value = i end,
        },
        g.text {
            present = function() return page.value == 2 end,
            size = 14,
            wrap = true,
            max_width = 424,
            color = function() return theme.get("text") end,
            text = "Every piece of @genoma/kit on one panel. The tabs choose what shows; this page is the second.",
        },
        g.box {
            present = function() return page.value == 1 end,
            direction = "column",
            align = "start",
            gap = 14,
            heading("Buttons"),
            line(
                kit.button { text = "Plain", on_click = function() said.value = "Plain clicked." end },
                kit.button { text = "Accent", kind = "accent", on_click = function() said.value = "Accent clicked." end },
                kit.button { text = "Danger", kind = "danger", on_click = function() said.value = "Danger clicked." end },
                kit.button { text = "Flat", kind = "flat", on_click = function() said.value = "Flat clicked." end },
                kit.button { text = "Off", disabled = true, on_click = function() said.value = "Never." end }
            ),

            heading("Switches"),
            line(
                kit.toggle { name = "Wi-Fi", on = function() return wifi.value end, on_toggle = function(on) wifi.value = on end },
                g.text { size = 14, color = function() return theme.get("text") end, text = "Wi-Fi" },
                kit.toggle { name = "Bluetooth", on = function() return bluetooth.value end, on_toggle = function(on) bluetooth.value = on end },
                g.text { size = 14, color = function() return theme.get("text") end, text = "Bluetooth" }
            ),

            heading("Level"),
            line(
                kit.slider { name = "Level", width = 260, value = function() return level.value end, on_change = function(v) level.value = v end },
                g.text {
                    size = 13,
                    tabular = true,
                    color = function() return theme.get("faint") end,
                    text = function() return `{math.floor(level.value * 100 + 0.5)}%` end,
                }
            ),
            kit.progress { width = 424, value = function() return level.value end },

            heading("Field"),
        kit.field {
            width = 300,
            placeholder = "Your name",
            clearable = true,
            text = function() return name.value end,
            on_change = function(t: string) name.value = t end,
            on_submit = function(t: string) said.value = `Hello, {t}.` end,
        },

        heading("Choices"),
            line(
                kit.dropdown {
                    width = 160,
                    options = function()
                        local names = {}
                        for i, a in accents do names[i] = a.name end
                        return names
                    end,
                    selected = function() return accent.value end,
                    on_select = function(i)
                        accent.value = i
                        theme.set { accent = accents[i].colour, selection = theme.alpha(accents[i].colour, 0.2) }
                    end,
                },
                kit.button {
                    text = "Actions ▾",
                    on_click = function() menu_open.value = not menu_open.value end,
                    kit.menu {
                        present = function() return menu_open.value end,
                        on_close = function() menu_open.value = false end,
                        items = {
                            { text = "Copy", keys = "Ctrl+C", on_select = function() said.value = "Copied." end },
                            { text = "Paste", keys = "Ctrl+V", disabled = true },
                            "-",
                            { text = "Delete", danger = true, on_select = function() said.value = "Deleted." end },
                        },
                    },
                },
                kit.tooltip {
                    text = "Says what the last action was",
                    kit.button { text = "?", kind = "flat" },
                },
                kit.button { text = "Delete…", kind = "danger", on_click = function() asking.value = true end }
            ),

            heading("A thousand rows"),
            g.box {
                width = 424,
                direction = "column",
                align = "stretch",
                kit.list {
                    height = 200,
                    items = function() return rows end,
                    key = function(r) return r.id end,
                    label = function(r) return r.name end,
                    detail = function(r) return r.size end,
                    selected = function() return chosen.value end,
                    on_select = function(i) chosen.value = i end,
                    on_activate = function(i) said.value = `Opened {rows[i].name}.` end,
                },
            },
        },
        g.text { size = 13, color = function() return theme.get("faint") end, text = function() return said.value end },
    },
}

return { panel, ask }
