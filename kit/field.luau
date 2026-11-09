--!strict
-- A text field in the kit's look: g.field in a well, its rim lit while it
-- has the keys, a placeholder, and on demand a button that empties it.
--
--   local field = require("@genoma/kit/field")
--   field { width = 260, placeholder = "Search…", text = function() return query.value end,
--           on_change = function(t) query.value = t end, clearable = true }
--
-- text: what it shows, a value or a function; on_change(text) is told every
-- change the person makes, and the shell keeps the text (the field is told
-- back). on_submit(text) is Return; on_key(name, text, mods) the keys the
-- field leaves (Escape, Tab, Up, Down). focused: whether it has the keys,
-- a value or a function; without it the field keeps its own, taken with a
-- press and let go with Escape. secret draws dots. clearable adds the
-- button.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local read = theme.read

export type Field = {
    text: (string | () -> string)?,
    on_change: ((text: string) -> ())?,
    on_submit: ((text: string) -> ())?,
    on_key: ((name: string, text: string?, mods: any) -> ())?,
    placeholder: string?,
    focused: (boolean | () -> boolean)?,
    secret: (boolean | () -> boolean)?,
    clearable: boolean?,
    width: (number | () -> number)?,
    name: string?,
    [any]: any,
}

return function(t: Field): any
    local function text(): string return read(t.text) or "" end
    -- Its own focus when the shell keeps none.
    local own = g.signal(false)
    local function has_keys(): boolean
        if t.focused ~= nil then return read(t.focused) == true end
        return own.value
    end
    local node: { [any]: any } = {
        role = "group",
        name = t.name,
        gap = 8,
        padding_x = 12,
        height = function() return theme.get("control_height") + 4 end,
        width = t.width or 260,
        radius = function() return theme.get("radius") end,
        fill = function() return theme.get("control") end,
        rim_top = function() return if has_keys() then theme.get("accent") else theme.get("line") end,
        rim_bottom = function() return if has_keys() then theme.get("accent") else theme.get("line") end,
        transition = { rim_top = "kit_quick", rim_bottom = "kit_quick" },
        on_press = function() if t.focused == nil then own.value = true end end,
        g.field {
            grow = 1,
            size = function() return theme.get("text_size") end,
            color = function() return theme.get("text") end,
            caret_color = function() return theme.get("accent") end,
            selection_color = function() return theme.get("selection") end,
            placeholder = t.placeholder or "",
            placeholder_color = function() return theme.get("faint") end,
            focused = has_keys,
            secret = function() return read(t.secret) == true end,
            text = text,
            on_change = function(value: string) if t.on_change then t.on_change(value) end end,
            on_submit = function(value: string) if t.on_submit then t.on_submit(value) end end,
            on_key = function(name: string, typed: string?, mods: any)
                if name == "Escape" and t.focused == nil then own.value = false end
                if t.on_key then t.on_key(name, typed, mods) end
            end,
        },
        g.capsule {
            present = function() return t.clearable == true and text() ~= "" end,
            role = "button",
            name = "Clear",
            cursor = "pointer",
            width = 20,
            height = 20,
            radius = 10,
            fill = function() return theme.get("control_hover") end,
            rim_top = "#00000000",
            rim_bottom = "#00000000",
            on_click = function() if t.on_change then t.on_change("") end end,
            g.text { size = 12, color = function() return theme.get("faint") end, text = "×" },
        },
    }
    for _, k in { "id", "present", "grow" } do
        if t[k] ~= nil then node[k] = t[k] end
    end
    return g.capsule(node)
end
