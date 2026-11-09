--!strict
-- A button: a label, an icon, or both, that does something when clicked.
--
--   local button = require("@genoma/kit/button")
--   button { text = "Save", kind = "accent", on_click = save }
--
-- kind: "plain" (the default), "accent" for the thing to do, "danger" for
-- what cannot be undone, "flat" for one drawn only under the pointer.
-- disabled (a value or a function) leaves it seen but deaf. Anything else
-- given (id, width, grow, name, a g.popup child…) goes to the button's
-- node; a child node replaces the label.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local read = theme.read

return function(t: { [string]: any }): any
    local hovered = g.signal(false)
    local pressed = g.signal(false)
    local function disabled(): boolean return read(t.disabled) == true end
    local function kind(): string return read(t.kind) or "plain" end

    local function fill(): string
        local k = kind()
        if k == "accent" or k == "danger" then
            local base = if k == "accent" then theme.get("accent") else theme.get("danger")
            if disabled() then return base end
            if pressed.value then return theme.alpha(base, 0.8) end
            if hovered.value then return theme.alpha(base, 0.9) end
            return base
        end
        if disabled() then return if k == "flat" then theme.alpha(theme.get("control"), 0) else theme.get("control") end
        if pressed.value then return theme.get("control_press") end
        if hovered.value then return theme.get("control_hover") end
        return if k == "flat" then theme.alpha(theme.get("control"), 0) else theme.get("control")
    end
    local function ink(): string
        local k = kind()
        if k == "accent" or k == "danger" then return theme.get("on_accent") end
        return theme.get("text")
    end
    local function rim(): string
        local k = kind()
        if k == "plain" then return theme.get("line") end
        return "#00000000"
    end

    local node: { [any]: any } = {
        role = "button",
        name = t.name or (if type(t.text) == "string" then t.text else nil),
        cursor = function() return if disabled() then "default" else "pointer" end,
        height = t.height or function() return theme.get("control_height") end,
        padding_x = t.padding_x or function() return if t.text then 14 else 8 end,
        gap = 8,
        justify = "center",
        radius = function() return theme.get("radius") end,
        fill = fill,
        rim_top = rim,
        rim_bottom = rim,
        opacity = function() return if disabled() then theme.get("disabled_opacity") else 1 end,
        transition = { fill = "kit_quick", rim_top = "kit_move", rim_bottom = "kit_move", opacity = "kit_move" },
        on_hover = function(on: boolean) hovered.value = on end,
        on_press = function(b: number) if b == 272 then pressed.value = true end end,
        on_release = function() pressed.value = false end,
        on_click = function(b: number)
            if b ~= 272 or g.untracked(disabled) then return end
            if t.on_click then t.on_click() end
        end,
    }
    local taken = { text = true, icon = true, kind = true, disabled = true, on_click = true, name = true, height = true, padding_x = true }
    for k, v in t do
        if type(k) == "string" and not taken[k] then node[k] = v end
    end

    -- Its content: the children given, or an icon and a label.
    local has_children = false
    for _, child in ipairs(t) do
        -- A popup beside the button is not its content.
        if not child.__popup then has_children = true end
        table.insert(node, child)
    end
    if not has_children then
        if t.icon then
            table.insert(node, g.icon {
                icon = t.icon,
                width = 16,
                height = 16,
                color = ink,
                transition = { color = "kit_quick" },
            })
        end
        if t.text then
            table.insert(node, g.text {
                size = function() return theme.get("text_size") end,
                weight = 500,
                color = ink,
                transition = { color = "kit_quick" },
                text = t.text,
            })
        end
    end
    return g.capsule(node)
end
