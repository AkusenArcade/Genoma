--!strict
-- One choice among a few: a button that says the chosen one and opens a
-- menu of the others.
--
--   local dropdown = require("@genoma/kit/dropdown")
--   dropdown { width = 180, options = { "Light", "Dark", "System" },
--              selected = function() return mode.value end,
--              on_select = function(i) mode.value = i end }
--
-- options: labels, or a function that gives them. selected: the chosen
-- one's place, from 1, or nil for none (`placeholder` is shown then).
-- on_select(place) is asked to change it.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")
local button = require("@genoma/kit/button")
local menu = require("@genoma/kit/menu")

local read = theme.read

export type Dropdown = {
    options: ({ string } | () -> { string })?,
    selected: (number | () -> number?)?,
    on_select: ((place: number) -> ())?,
    placeholder: string?,
    width: number?,
    disabled: (boolean | () -> boolean)?,
    name: string?,
    [any]: any,
}

return function(t: Dropdown): any
    local open = g.signal(false)
    local function options(): { string } return read(t.options) or {} end
    local function label(): string
        local at = read(t.selected)
        local list = options()
        if at and list[at] then return list[at] end
        return t.placeholder or "Choose…"
    end
    local function chosen(): boolean
        local at = read(t.selected)
        return at ~= nil and options()[at] ~= nil
    end

    return button {
        id = t.id,
        name = t.name,
        width = t.width,
        grow = t.grow,
        disabled = t.disabled,
        padding_x = 12,
        justify = "between",
        on_click = function() open.value = not g.untracked(function() return open.value end) end,
        g.text {
            grow = 1,
            size = function() return theme.get("text_size") end,
            color = function() return if chosen() then theme.get("text") else theme.get("faint") end,
            ellipsize = "end",
            text = label,
        },
        g.text {
            size = function() return theme.get("text_size") - 2 end,
            color = function() return theme.get("faint") end,
            text = "▾",
        },
        menu {
            present = function() return open.value end,
            on_close = function() open.value = false end,
            min_width = t.width,
            items = function()
                local items = {}
                local at = read(t.selected)
                for i, text in options() do
                    table.insert(items, {
                        text = if i == at then text .. "  ✓" else text,
                        on_select = function() if t.on_select then t.on_select(i) end end,
                    })
                end
                return items
            end,
        },
    }
end
