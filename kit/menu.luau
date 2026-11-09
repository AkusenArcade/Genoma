--!strict
-- A menu: a list of things to do, opened beside a node, over everything.
--
--   local open = g.signal(false)
--   button { text = "File", on_click = function() open.value = true end,
--       menu {
--           present = function() return open.value end,
--           on_close = function() open.value = false end,
--           items = {
--               { text = "Open…", on_select = open_file },
--               { text = "Save", on_select = save, disabled = function() return not dirty.value end },
--               "-",
--               { text = "Delete", on_select = delete, danger = true },
--           },
--       },
--   }
--
-- Written inside a node, it opens beside that node (or name another with
-- `anchor`, a node's id). side and align place it as g.popup does: below,
-- starting at the node's start, by default. items is a list, or a function
-- that gives one; "-" draws a separator. Choosing an item calls its
-- on_select and then on_close; so does a press outside the menu, and
-- Escape. The keyboard moves through the items with Up and Down, Home and
-- End, and Return chooses.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local read = theme.read

local ROW = 30

return function(t: { [string]: any }): any
    -- The item under the pointer or the keyboard, by place.
    local hot = g.signal(nil :: number?)
    local function items(): { any }
        local list = read(t.items) or {}
        local out = {}
        for i, item in list do
            table.insert(out, { place = i, item = item })
        end
        return out
    end
    local function usable(item: any): boolean
        return type(item) == "table" and read(item.disabled) ~= true
    end
    local function close()
        hot.value = nil
        if t.on_close then t.on_close() end
    end
    local function choose(item: any)
        if not usable(item) then return end
        close()
        if item.on_select then item.on_select() end
    end
    -- The next usable item from `from`, going `by`, wrapping round.
    local function move(by: number)
        local list = g.untracked(items)
        local count = #list
        if count == 0 then return end
        local at = g.untracked(function() return hot.value end) or (if by > 0 then 0 else count + 1)
        for _ = 1, count do
            at = ((at + by - 1) % count) + 1
            if usable(list[at].item) then
                hot.value = at
                return
            end
        end
    end
    local function edge(first: boolean)
        hot.value = nil
        move(if first then 1 else -1)
    end

    local popup: { [any]: any } = {
        anchor = t.anchor,
        side = t.side or "bottom",
        align = t.align or "start",
        gap = t.gap or 4,
        present = t.present,
        on_dismiss = close,

        g.capsule {
            catch = true,
            on_key = function(name: string)
                if name == "Escape" then close()
                elseif name == "Down" or name == "Tab" then move(1)
                elseif name == "Up" or name == "ISO_Left_Tab" then move(-1)
                elseif name == "Home" then edge(true)
                elseif name == "End" then edge(false)
                elseif name == "Return" or name == "KP_Enter" or name == "space" then
                    local at = g.untracked(function() return hot.value end)
                    local list = g.untracked(items)
                    if at and list[at] then choose(list[at].item) end
                end
            end,
            direction = "column",
            align = "stretch",
            min_width = t.min_width or 180,
            padding = 4,
            radius = function() return theme.get("radius") + 2 end,
            fill = function() return theme.get("background") end,
            rim_top = function() return theme.get("line") end,
            rim_bottom = function() return theme.get("line") end,
            shadow = "#00000059",
            shadow_blur = 18,
            shadow_y = 6,

            g.each {
                direction = "column",
                align = "stretch",
                items = items,
                key = function(entry) return entry.place end,
                row = function(entry: () -> any, place: () -> number)
                    local function item(): any return entry().item end
                    local function separator(): boolean return item() == "-" end
                    local function disabled(): boolean return not separator() and not usable(item()) end
                    local function lit(): boolean return hot.value == place() and not disabled() end
                    local function ink(): string
                        local it = item()
                        if type(it) == "table" and it.danger then return theme.get("danger") end
                        return theme.get("text")
                    end
                    return g.capsule {
                        role = if g.untracked(separator) then "group" else "button",
                        name = (function() local it = g.untracked(item); return if type(it) == "table" then it.text else nil end)(),
                        cursor = function() return if separator() or disabled() then "default" else "pointer" end,
                        height = function() return if separator() then 9 else ROW end,
                        padding_x = 10,
                        gap = 12,
                        justify = "between",
                        radius = function() return theme.get("radius") - 2 end,
                        fill = function() return if lit() then theme.get("selection") else theme.alpha(theme.get("selection"), 0) end,
                        rim_top = "#00000000",
                        rim_bottom = "#00000000",
                        opacity = function() return if disabled() then theme.get("disabled_opacity") else 1 end,
                        transition = { fill = "kit_quick" },
                        on_hover = function(entered: boolean)
                            if entered and not g.untracked(disabled) and not g.untracked(separator) then
                                hot.value = g.untracked(place)
                            elseif not entered and g.untracked(function() return hot.value end) == g.untracked(place) then
                                hot.value = nil
                            end
                        end,
                        on_click = function(b: number)
                            if b == 272 then choose(g.untracked(item)) end
                        end,
                        -- A separator: a hairline across the row.
                        g.capsule {
                            present = separator,
                            grow = 1,
                            height = 1,
                            radius = 0,
                            fill = function() return theme.get("line") end,
                            rim_top = "#00000000",
                            rim_bottom = "#00000000",
                        },
                        g.text {
                            present = function() return not separator() end,
                            size = function() return theme.get("text_size") end,
                            color = ink,
                            text = function() local it = item(); return if type(it) == "table" then it.text or "" else "" end,
                        },
                        -- A shortcut's keys, said at the right.
                        g.text {
                            present = function() local it = item(); return type(it) == "table" and it.keys ~= nil end,
                            size = function() return theme.get("text_size") - 2 end,
                            color = function() return theme.get("faint") end,
                            text = function() local it = item(); return if type(it) == "table" then it.keys or "" else "" end,
                        },
                    }
                end,
            },
        },
    }
    return g.popup(popup)
end
