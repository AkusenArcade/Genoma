--!strict
-- Tabs: a row of names, one chosen, for pages of which one shows at a time.
--
--   local tabs = require("@genoma/kit/tabs")
--   tabs { items = { "General", "Display", "About" },
--          selected = function() return page.value end,
--          on_select = function(i) page.value = i end }
--
-- items: names, or a function that gives them. selected is a place from
-- 1; the tabs show it and ask on_select(place) to change it. The pages
-- are the shell's: each `present` while its place is selected.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local read = theme.read

return function(t: { [string]: any }): any
    local function items(): { any }
        local out = {}
        for i, name in read(t.items) or {} do
            table.insert(out, { place = i, name = name })
        end
        return out
    end
    local node: { [any]: any } = {
        role = "group",
        name = t.name or "Tabs",
        padding = 3,
        gap = 2,
        radius = function() return theme.get("radius") + 1 end,
        fill = function() return theme.get("control") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,
        g.each {
            gap = 2,
            items = items,
            key = function(entry) return entry.place end,
            row = function(entry: () -> any)
                local hovered = g.signal(false)
                local function chosen(): boolean return read(t.selected) == entry().place end
                return g.capsule {
                    role = "button",
                    name = (function() local e = g.untracked(entry); return tostring(e.name) end)(),
                    cursor = "pointer",
                    height = function() return theme.get("control_height") - 6 end,
                    padding_x = 14,
                    radius = function() return theme.get("radius") - 1 end,
                    fill = function()
                        if chosen() then return theme.get("background") end
                        if hovered.value then return theme.alpha(theme.get("control_hover"), 0.9) end
                        return theme.alpha(theme.get("control"), 0)
                    end,
                    rim_top = function() return if chosen() then theme.get("line") else "#00000000" end,
                    rim_bottom = "#00000000",
                    transition = { fill = "kit_quick", rim_top = "kit_quick" },
                    on_hover = function(on: boolean) hovered.value = on end,
                    on_click = function(b: number)
                        if b == 272 and t.on_select then t.on_select(g.untracked(entry).place) end
                    end,
                    g.text {
                        size = function() return theme.get("text_size") end,
                        weight = function() return if chosen() then 600 else 500 end,
                        color = function() return if chosen() then theme.get("text") else theme.get("faint") end,
                        transition = { color = "kit_quick" },
                        text = function() return tostring(entry().name) end,
                    },
                }
            end,
        },
    }
    for _, k in { "id", "present", "grow" } do
        if t[k] ~= nil then node[k] = t[k] end
    end
    return g.capsule(node)
end
