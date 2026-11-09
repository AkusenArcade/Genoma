--!strict
-- A switch: on or off, the knob travelling between them.
--
--   local toggle = require("@genoma/kit/toggle")
--   toggle { on = function() return wifi.value end, on_toggle = function(on) wifi.value = on end }
--
-- on: a value or a function; the toggle shows it and asks on_toggle(new)
-- to change it, so what it shows is always what the shell holds. name
-- says what it switches, for assistive technology. disabled leaves it
-- seen but deaf.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local read = theme.read

local WIDTH, HEIGHT, KNOB = 40, 22, 16

export type Toggle = {
    on: (boolean | () -> boolean)?,
    on_toggle: ((on: boolean) -> ())?,
    disabled: (boolean | () -> boolean)?,
    name: string?,
    [any]: any,
}

return function(t: Toggle): any
    local hovered = g.signal(false)
    local function on(): boolean return read(t.on) == true end
    local function disabled(): boolean return read(t.disabled) == true end
    local inset = (HEIGHT - KNOB) / 2

    local node: { [any]: any } = {
        role = "button",
        name = t.name or "Switch",
        cursor = function() return if disabled() then "default" else "pointer" end,
        width = WIDTH,
        height = HEIGHT,
        radius = HEIGHT / 2,
        fill = function()
            if on() then return theme.get("accent") end
            return if hovered.value and not disabled() then theme.get("control_hover") else theme.get("control")
        end,
        rim_top = function() return if on() then "#00000000" else theme.get("line") end,
        rim_bottom = function() return if on() then "#00000000" else theme.get("line") end,
        opacity = function() return if disabled() then theme.get("disabled_opacity") else 1 end,
        transition = { fill = "kit_move", rim_top = "kit_move", rim_bottom = "kit_move", opacity = "kit_move" },
        on_hover = function(entered: boolean) hovered.value = entered end,
        on_click = function(b: number)
            if b ~= 272 or g.untracked(disabled) then return end
            if t.on_toggle then t.on_toggle(not g.untracked(on)) end
        end,
        g.capsule {
            overlay = true,
            attach = "corner",
            width = KNOB,
            height = KNOB,
            radius = KNOB / 2,
            offset_x = function() return if on() then WIDTH - KNOB - inset else inset end,
            offset_y = inset,
            fill = function() return if on() then theme.get("on_accent") else theme.get("faint") end,
            rim_top = "#00000000",
            rim_bottom = "#00000000",
            transition = { offset_x = "kit_move", fill = "kit_move" },
        },
    }
    for _, k in { "id", "present" } do
        if t[k] ~= nil then node[k] = t[k] end
    end
    return g.capsule(node)
end
