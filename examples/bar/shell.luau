--!strict
-- A classic top bar, the shape most shells start from, written against
-- Genoma alone: workspaces on the left, the focused window's title in the
-- middle, system readings and the time on the right. It reserves its height,
-- so windows tile below it. A press on the time opens the date beside it, a
-- popup the compositor keeps on screen; a press elsewhere or Escape closes it.
--
--   GENOMA_OUTPUT=DP-1 genoma examples/bar
--
-- Nothing here is Bioma's: a neutral palette, the system font, text only.
-- Every value on screen is a binding on a service, so the bar redraws only
-- when one of them changes.
local g = require("@genoma")
local clock = require("@genoma/clock")
-- niri, Hyprland, Umbriel or any other: the same calls on each.
local compositor = require("@genoma/compositor")
local vitals = require("@genoma/vitals")
local power = require("@genoma/power")
local audio = require("@genoma/audio")
local theme = require("theme")

-- Every duration the interface uses is named once, here.
g.timing {
    reflow = { duration = 0.18, curve = { 0.2, 0.0, 0.0, 1.0 } },
    theme = { duration = 0.2, curve = { 0.4, 0.0, 0.2, 1.0 } },
}

local HEIGHT = 32
local OUTPUT = g.env("GENOMA_OUTPUT") or compositor.focused_output

-- ---- Workspaces ----------------------------------------------------------

local function workspaces()
    return compositor:workspaces(OUTPUT)
end

local function step(delta: number)
    local list = workspaces()
    for i, w in list do
        if w.active then
            local next = list[math.clamp(i + delta, 1, #list)]
            if next then compositor:focus_workspace(next.id) end
            return
        end
    end
end

-- One button per workspace, kept by its id: a workspace that appears or
-- goes adds or removes only its own button.
local function button(ws: () -> any, _index: () -> number)
    return g.capsule {
        role = "button",
        name = "Workspace",
        min_width = 26,
        height = 22,
        padding_x = 8,
        radius = 6,
        fill = function() return if ws().active then theme.accent else theme.none end,
        on_click = function() compositor:focus_workspace(ws().id) end,
        g.text {
            size = 13,
            weight = 600,
            tabular = true,
            color = function()
                local w = ws()
                if w.active then return theme.on_accent end
                return if w.vacant then theme.faint else theme.text
            end,
            text = function()
                local w = ws()
                return if w.name and w.name ~= "" then w.name else tostring(w.idx)
            end,
        },
    }
end

local left = g.each {
    direction = "row",
    gap = 4,
    items = workspaces,
    key = function(w) return w.id end,
    row = button,
    on_wheel = function(_, dy) step(if dy > 0 then 1 else -1) end,
}

-- ---- Window title --------------------------------------------------------

local middle = g.text {
    size = 13,
    color = theme.text,
    ellipsize = "end",
    max_width = 640,
    text = function()
        local w = if OUTPUT then compositor:focused_on(OUTPUT) else compositor.focused
        return if w then w.title or "" else ""
    end,
}

-- ---- Readings ------------------------------------------------------------

local function percent(x: number): string
    return string.format("%d%%", math.floor(x * 100 + 0.5))
end

-- A reading: a dim label and its value, e.g. "CPU 12%".
local function reading(label: string, value: () -> string, extra: { [any]: any }?)
    local t: { [any]: any } = {
        gap = 5,
        g.text { size = 12, weight = 600, tracking = 0.06, color = theme.faint, text = label },
        g.text { size = 13, tabular = true, color = theme.text, text = value },
    }
    for k, v in extra or {} do
        t[k] = v
    end
    return g.box(t)
end

-- The default output device, as PipeWire names it.
local function sink(): any
    for _, node in audio.nodes do
        if node.name == audio.default_sink then return node end
    end
    return nil
end

local volume = reading("VOL", function()
    local s = sink()
    if not s then return "–" end
    return if s.muted then "mute" else percent(s.volume)
end, {
    role = "slider",
    name = "Volume",
    present = function() return sink() ~= nil end,
    -- The wheel moves the volume 5% a notch; a click mutes.
    on_wheel = function(_, dy)
        local s = sink()
        if s then audio:set_volume(s.id, math.clamp(s.volume - dy * 0.05, 0, 1)) end
    end,
    on_click = function()
        local s = sink()
        if s then audio:set_mute(s.id, not s.muted) end
    end,
})

local battery = reading("BAT", function()
    local b = power.battery
    local charging = b.state == "charging" or b.state == "full"
    return percent(b.level) .. (if charging then "+" else "")
end, {
    role = "status",
    name = "Battery",
    present = function() return power.battery.present end,
})

local function two(n: number): string
    return string.format("%02d", n)
end

-- Whether the date is open beside the clock.
local dated = g.signal(false)

local right = g.box {
    gap = 18,
    reading("CPU", function() return percent(vitals.cpu) end, { role = "status", name = "Processor" }),
    reading("RAM", function() return percent(vitals.ram) end, { role = "status", name = "Memory" }),
    volume,
    battery,
    g.text {
        id = "clock",
        role = "timer",
        name = "Clock",
        cursor = "pointer",
        on_click = function() dated.value = not dated.value end,
        size = 13,
        weight = 600,
        tabular = true,
        color = theme.text,
        text = function() return two(clock.hour) .. ":" .. two(clock.minute) end,
    },
}

-- ---- The date ------------------------------------------------------------

-- A popup beside the node whose id is "clock": below it, its right edge on
-- the clock's, turned or slid by the compositor if it would leave the screen.
-- Opened by a press, it takes the keyboard, and a press anywhere else closes
-- it (on_dismiss), as a menu does.
local date = g.popup {
    anchor = "clock",
    side = "bottom",
    align = "end",
    gap = 8,
    present = function() return dated.value end,
    on_dismiss = function() dated.value = false end,

    g.capsule {
        -- Catching, it hears the keys.
        catch = true,
        on_key = function(name: string)
            if name == "Escape" then dated.value = false end
        end,
        direction = "column",
        gap = 2,
        padding = 14,
        radius = 10,
        fill = theme.background,
        rim_top = theme.line,
        g.text {
            size = 12,
            weight = 600,
            tracking = 0.06,
            color = theme.faint,
            -- Read again each hour: the day changes on one of them.
            text = function() return clock.hour >= 0 and string.upper(os.date("%A")) or "" end,
        },
        g.text {
            size = 15,
            weight = 600,
            color = theme.text,
            text = function() return clock.hour >= 0 and os.date("%d %B %Y") or "" end,
        },
    },
}

-- ---- The bar -------------------------------------------------------------

local bar = g.surface {
    anchor = { "top", "left", "right" },
    namespace = "genoma-bar",
    reserve = true,

    g.capsule {
        grow = 1,
        height = HEIGHT,
        radius = 0,
        padding_x = 10,
        fill = theme.background,
        rim_bottom = theme.line,
        justify = "between",
        left,
        middle,
        right,
    },
}

return { bar, date }
