--!strict
-- A level set by hand: a track, the part travelled in the accent, a handle
-- at its head. A press puts the value where it landed and a drag carries
-- it; the wheel moves it by `step`.
--
--   local slider = require("@genoma/kit/slider")
--   slider { width = 200, value = function() return volume.value end,
--            on_change = function(v) volume.value = v end }
--
-- value: 0 to 1, a value or a function; on_change(v) is asked to change
-- it. step (default 0.05) is what a wheel notch moves, and the value is
-- kept to multiples of it when `snap` is true. name says what it sets.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local read = theme.read

local TRACK, HANDLE = 4, 16

export type Slider = {
    value: (number | () -> number)?,
    on_change: ((value: number) -> ())?,
    width: (number | () -> number)?,
    step: number?,
    snap: boolean?,
    disabled: (boolean | () -> boolean)?,
    name: string?,
    [any]: any,
}

return function(t: Slider): any
    local hovered = g.signal(false)
    local held = g.signal(false)
    local function width(): number return read(t.width) or 200 end
    local function value(): number return math.clamp(read(t.value) or 0, 0, 1) end
    local function disabled(): boolean return read(t.disabled) == true end
    local step = t.step or 0.05

    local function report(v: number)
        v = math.clamp(v, 0, 1)
        if t.snap then v = math.floor(v / step + 0.5) * step end
        if t.on_change then t.on_change(v) end
    end
    -- The handle's centre travels the track inside the handle's own half
    -- at each end, so it never hangs past the slider.
    local function travel(): number return math.max(1, width() - HANDLE) end
    local function at(x: number): number return (x - HANDLE / 2) / g.untracked(travel) end
    local function head(): number return HANDLE / 2 + value() * travel() end

    local node: { [any]: any } = {
        role = "slider",
        name = t.name or "Level",
        cursor = function() return if disabled() then "default" else "pointer" end,
        width = width,
        height = HANDLE,
        opacity = function() return if disabled() then theme.get("disabled_opacity") else 1 end,
        transition = { opacity = "kit_move" },
        on_hover = function(entered: boolean) hovered.value = entered end,
        on_press = function(b: number, x: number)
            if b ~= 272 or g.untracked(disabled) then return end
            held.value = true
            report(at(x))
        end,
        on_drag = function(x: number)
            if g.untracked(function() return held.value end) then report(at(x)) end
        end,
        on_release = function() held.value = false end,
        -- The press is the slider's: it does not reach a node under it.
        on_click = function() end,
        on_wheel = function(_dx: number, dy: number, steps: boolean?)
            if g.untracked(disabled) then return end
            local by = if steps then -dy * step else -dy / g.untracked(travel)
            report(g.untracked(value) + by)
        end,
        g.capsule {
            overlay = true,
            attach = "corner",
            offset_y = (HANDLE - TRACK) / 2,
            width = width,
            height = TRACK,
            radius = TRACK / 2,
            fill = function() return theme.get("control") end,
            rim_top = "#00000000",
            rim_bottom = "#00000000",
        },
        g.capsule {
            overlay = true,
            attach = "corner",
            offset_y = (HANDLE - TRACK) / 2,
            width = head,
            height = TRACK,
            radius = TRACK / 2,
            fill = function() return theme.get("accent") end,
            rim_top = "#00000000",
            rim_bottom = "#00000000",
        },
        g.capsule {
            overlay = true,
            attach = "corner",
            offset_x = function() return head() - HANDLE / 2 end,
            width = HANDLE,
            height = HANDLE,
            radius = HANDLE / 2,
            fill = function() return theme.get("text") end,
            rim_top = "#00000000",
            rim_bottom = "#00000000",
            shadow = "#00000066",
            shadow_blur = function() return if held.value or hovered.value then 8 else 4 end,
            transition = { shadow_blur = "kit_quick" },
        },
    }
    for _, k in { "id", "present", "grow" } do
        if t[k] ~= nil then node[k] = t[k] end
    end
    return g.box(node)
end
