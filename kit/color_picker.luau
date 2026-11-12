--!strict
-- A colour to choose: saturation and brightness on a field, hue and
-- opacity on strips, the colour's hex to type, and swatches.
--
--   local picker = require("@genoma/kit/color_picker")
--   picker { color = function() return accent.value end,
--            on_change = function(hex) accent.value = hex end,
--            alpha = true, swatches = { "#7aa2f7", "#bb9af7", "#9ece6a" } }
--
--   picker.button { color = …, on_change = … }   -- a swatch that opens one
--
-- color: "#rrggbb" or "#rrggbbaa", a value or a function; on_change(hex)
-- is told every change, "#rrggbbaa" when alpha is on, "#rrggbb" otherwise.
-- The field and strips are dragged, pressed, or turned with the wheel;
-- picker.key(view, name) moves them with the keyboard (the arrows across
-- the field, Page_Up and Page_Down the hue). The button's picker opens
-- below it and takes the keys itself.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")
local field = require("@genoma/kit/field")

local read = theme.read

export type Picker = {
    color: (string | () -> string)?,
    on_change: ((hex: string) -> ())?,
    alpha: boolean?,
    swatches: { string }?,
    width: number?,
    [any]: any,
}

-- Hue, saturation, value and alpha each view holds, so a grey keeps the
-- hue it had and a black its saturation: what the hex alone forgets.
type Hsva = { h: number, s: number, v: number, a: number }

local states: { [any]: any } = setmetatable({}, { __mode = "k" }) :: any

local function hex_of(t: Picker, c: Hsva): string
    local full = g.colour.hsva(c.h, c.s, c.v, c.a)
    return if t.alpha then full else string.sub(full, 1, 7)
end

local function state(t: Picker): any
    local s = states[t]
    if s then return s end
    s = g.lasting(function()
        local hsva = g.signal({ h = 0, s = 0, v = 0, a = 1 } :: Hsva)
        local told: string? = nil
        -- Follows the colour from outside; a colour it told itself (the
        -- same hex coming back) leaves the hue and saturation it holds.
        g.effect(function()
            local c = read(t.color) or "#000000"
            if c == told then return end
            local parsed = g.colour.parse(c)
            if not parsed then return end
            g.untracked(function()
                local h, sa, v, a = g.colour.hsv(c)
                local was = hsva.value
                hsva.value = {
                    h = if h < 0 then was.h else h,
                    s = if v == 0 then was.s else sa,
                    v = v,
                    a = if t.alpha then a else 1,
                }
            end)
        end)
        return {
            hsva = hsva,
            set = function(changes: { [string]: number })
                local next = table.clone(g.untracked(function() return hsva.value end)) :: any
                for k, v in changes do next[k] = math.clamp(v, 0, 1) end
                hsva.value = next
                told = hex_of(t, next)
                if t.on_change then t.on_change(told :: string) end
            end,
        }
    end)
    states[t] = s
    return s
end

local M = {}

-- picker.key(view, name): the keyboard's way over the field and the hue.
function M.key(t: Picker, name: string): boolean
    local s = state(t)
    local c = g.untracked(function() return s.hsva.value end)
    local step = 0.02
    if name == "Left" then s.set { s = c.s - step }
    elseif name == "Right" then s.set { s = c.s + step }
    elseif name == "Up" then s.set { v = c.v + step }
    elseif name == "Down" then s.set { v = c.v - step }
    elseif name == "Page_Up" or name == "Prior" then s.set { h = (c.h + 1 / 72) % 1 }
    elseif name == "Page_Down" or name == "Next" then s.set { h = (c.h - 1 / 72) % 1 }
    else return false end
    return true
end

local HUES = {
    { 0, "#ff0000" }, { 1 / 6, "#ffff00" }, { 2 / 6, "#00ff00" }, { 3 / 6, "#00ffff" },
    { 4 / 6, "#0000ff" }, { 5 / 6, "#ff00ff" }, { 1, "#ff0000" },
}

-- A round handle at a place on its strip or field, ringed so it shows on
-- any colour.
local function handle(left: () -> number, top: () -> number, fill: () -> string, size: number): any
    return g.capsule {
        overlay = true,
        left = function() return left() - size / 2 end,
        top = function() return top() - size / 2 end,
        width = size,
        height = size,
        fill = fill,
        outline = "#ffffff",
        outline_width = 2,
        shadow = "#00000066",
        shadow_blur = 4,
    }
end

local function picker(t: Picker): any
    local s = state(t)
    local width = t.width or 240
    local field_h = math.floor(width * 0.62)
    local strip = 14
    local function c(): Hsva return s.hsva.value end

    -- Saturation across, brightness up: the hue's own colour, white laid
    -- over from the left, black from the foot.
    local sv = g.capsule {
        width = width,
        height = field_h,
        radius = function() return theme.get("radius") end,
        cursor = "crosshair",
        fill = function() return g.colour.hsva(c().h, 1, 1, 1) end,
        on_press = function(_: number, x: number, y: number) s.set { s = x / width, v = 1 - y / field_h } end,
        on_drag = function(x: number, y: number) s.set { s = x / width, v = 1 - y / field_h } end,
        on_wheel = function(_: number, dy: number, steps: boolean)
            s.set { v = c().v - (if steps then dy * 0.05 else dy / 300) }
        end,
        g.capsule {
            overlay = true, left = 0, right = 0, top = 0, bottom = 0,
            radius = function() return theme.get("radius") end,
            fill_stops = { { 0, "#ffffffff" }, { 1, "#ffffff00" } },
            fill_angle = 90,
        },
        g.capsule {
            overlay = true, left = 0, right = 0, top = 0, bottom = 0,
            radius = function() return theme.get("radius") end,
            fill_stops = { { 0, "#00000000" }, { 1, "#000000ff" } },
            fill_angle = 180,
        },
        handle(
            function() return c().s * width end,
            function() return (1 - c().v) * field_h end,
            function() return g.colour.hsva(c().h, c().s, c().v, 1) end,
            14
        ),
    }

    local function bar(which: string, fill: any, value: () -> number, set: (x: number) -> ()): any
        return g.capsule {
            id = t.id and (t.id .. "-" .. which) or nil,
            width = width,
            height = strip,
            cursor = "pointer",
            fill_stops = fill,
            fill_angle = 90,
            on_press = function(_: number, x: number) set(x / width) end,
            on_drag = function(x: number) set(x / width) end,
            on_wheel = function(dx: number, dy: number, steps: boolean)
                local d = if dx ~= 0 then dx else dy
                set(value() + (if steps then d * 0.02 else d / 400))
            end,
            handle(function() return value() * width end, function() return strip / 2 end, function() return "#00000000" end, strip + 4),
        }
    end

    local hue = bar("hue", HUES, function() return c().h end, function(x) s.set { h = x } end)
    local opacity = bar("alpha", function()
        local solid = g.colour.hsva(c().h, c().s, c().v, 1)
        return { { 0, g.colour.alpha(solid, 0) }, { 1, solid } }
    end, function() return c().a end, function(x) s.set { a = x } end)

    -- The hex, typed: taken when it reads as a colour.
    local typed = g.signal(nil :: string?)
    local hex = field {
        width = width - 44,
        text = function() return typed.value or hex_of(t, c()) end,
        on_change = function(text: string) typed.value = text end,
        on_submit = function(text: string)
            local v = if string.sub(text, 1, 1) == "#" then text else "#" .. text
            local p = g.colour.parse(v)
            typed.value = nil
            if not p then return end
            local h, sa, va, a = g.colour.hsv(v)
            local was = g.untracked(c)
            s.set { h = if h < 0 then was.h else h, s = sa, v = va, a = if t.alpha then a else 1 }
        end,
    }

    local swatches = {}
    for _, colour in t.swatches or {} do
        table.insert(swatches, g.capsule {
            role = "button",
            name = colour,
            cursor = "pointer",
            width = 22,
            height = 22,
            radius = 11,
            fill = colour,
            outline = function() return theme.get("line") end,
            outline_width = 1,
            on_click = function()
                local h, sa, v, a = g.colour.hsv(colour)
                local was = g.untracked(c)
                s.set { h = if h < 0 then was.h else h, s = sa, v = v, a = if t.alpha then a else 1 }
            end,
        })
    end

    local node: { [any]: any } = {
        direction = "column",
        align = "start",
        gap = 10,
        sv,
        hue,
        g.box { present = t.alpha == true, opacity },
        g.box {
            gap = 8,
            g.capsule {
                width = 36,
                height = 32,
                radius = function() return theme.get("radius") end,
                fill = function() return g.colour.hsva(c().h, c().s, c().v, c().a) end,
                outline = function() return theme.get("line") end,
                outline_width = 1,
            },
            hex,
        },
        g.box { present = #swatches > 0, gap = 6, flow = true, width = width, table.unpack(swatches) },
    }
    local own = { color = true, on_change = true, alpha = true, swatches = true, width = true }
    for k, v in t do
        if type(k) == "string" and node[k] == nil and not (own :: any)[k] then node[k] = v end
    end
    return g.box(node)
end

-- picker.button { color, on_change, … }: a swatch of the colour; pressed,
-- a picker opens below it, and closes with Escape or a press outside.
function M.button(t: Picker): any
    local open = g.signal(false)
    local function close() open.value = false end
    return g.capsule {
        id = t.id,
        role = "button",
        name = t.name or "Colour",
        cursor = "pointer",
        width = 44,
        height = function() return theme.get("control_height") end,
        radius = function() return theme.get("radius") end,
        fill = function() return read(t.color) or "#000000" end,
        outline = function() return theme.get("line") end,
        outline_width = 1,
        on_click = function() open.value = not g.untracked(function() return open.value end) end,
        g.popup {
            side = "bottom",
            align = "start",
            gap = 6,
            present = function() return open.value end,
            on_dismiss = close,
            g.capsule {
                catch = true,
                on_outside = close,
                on_key = function(name: string)
                    if name == "Escape" then close() else M.key(t, name) end
                end,
                padding = 12,
                radius = function() return theme.get("radius") + 4 end,
                fill = function() return theme.get("background") end,
                rim_top = function() return theme.get("line") end,
                rim_bottom = function() return theme.get("line") end,
                shadow = "#00000066",
                shadow_blur = 18,
                shadow_y = 6,
                picker(t),
            },
        },
    }
end

setmetatable(M, { __call = function(_, t: Picker): any return picker(t) end })

return M
