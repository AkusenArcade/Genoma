--!strict
-- A list to choose from: rows of a label and an optional detail, the
-- chosen one lit, only the rows in view mounted (g.list), however long.
--
--   local list = require("@genoma/kit/list")
--   list { height = 320, items = function() return files.value end,
--          key = function(f) return f.path end,
--          label = function(f) return f.name end,
--          detail = function(f) return f.size end,
--          selected = function() return chosen.value end,
--          on_select = function(i) chosen.value = i end,
--          on_activate = function(i) open(files.value[i]) end }
--
-- selected is a place from 1 (or nil), kept in view as it changes, so the
-- keyboard can move it: a shell's on_key calls on_select with the next
-- place. A click selects a row; a click on the selected one activates it.
-- row_height defaults to the theme's control height; key defaults to the
-- label.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local read = theme.read

export type List = {
    items: () -> { any },
    label: (item: any) -> any,
    detail: ((item: any) -> any)?,
    key: ((item: any) -> string | number)?,
    selected: (number | () -> number?)?,
    on_select: ((place: number) -> ())?,
    on_activate: ((place: number) -> ())?,
    row_height: number?,
    [any]: any,
}

return function(t: List): any
    local row_height = t.row_height or g.untracked(function() return theme.get("control_height") end)
    local function selected(): number? return read(t.selected) end
    local node: { [any]: any } = {
        row_height = row_height,
        items = t.items,
        key = t.key or t.label,
        reveal = selected,
        row = function(item: () -> any, place: () -> number)
            local function lit(): boolean return selected() == place() end
            local hovered = g.signal(false)
            return g.capsule {
                role = "button",
                cursor = "pointer",
                padding_x = 10,
                gap = 12,
                justify = "between",
                radius = function() return theme.get("radius") - 2 end,
                fill = function()
                    if lit() then return theme.get("selection") end
                    if hovered.value then return theme.alpha(theme.get("control_hover"), 0.6) end
                    return theme.alpha(theme.get("control_hover"), 0)
                end,
                rim_top = "#00000000",
                rim_bottom = "#00000000",
                transition = { fill = "kit_quick" },
                on_hover = function(entered: boolean) hovered.value = entered end,
                on_click = function(b: number)
                    if b ~= 272 then return end
                    local at = g.untracked(place)
                    if g.untracked(selected) == at then
                        if t.on_activate then t.on_activate(at) end
                    elseif t.on_select then
                        t.on_select(at)
                    end
                end,
                g.text {
                    grow = 1,
                    size = function() return theme.get("text_size") end,
                    color = function() return if lit() then theme.get("text") else theme.get("faint") end,
                    ellipsize = "end",
                    text = function() return tostring(t.label(item())) end,
                },
                g.text {
                    present = t.detail ~= nil,
                    size = function() return theme.get("text_size") - 2 end,
                    color = function() return theme.get("faint") end,
                    text = function() return if t.detail then tostring(t.detail(item()) or "") else "" end,
                },
            }
        end,
    }
    for k, v in t do
        if type(k) == "string" and node[k] == nil and not ({ label = true, detail = true, selected = true, on_select = true, on_activate = true } :: any)[k] then
            node[k] = v
        end
    end
    return g.list(node)
end
