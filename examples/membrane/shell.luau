--!strict
-- Milestone 2: Bioma's top membrane as Bioma draws it now (2026-10-28), for
-- the side-by-side measurement. Every measure below comes from Bioma's
-- current QML; docs/m2-parity.md names the file for each.
local g = require("@genoma")
local clock = require("@genoma/clock")
local niri = require("@genoma/niri")
local vitals = require("@genoma/vitals")
local theme = require("theme")

g.icon_dir("~/Documents/Development/Bioma/assets/icons")
g.timing {
    reflow = { duration = 0.22, curve = { 0.2, 0.0, 0.0, 1.0 } },
    theme = { duration = 0.25, curve = { 0.4, 0.0, 0.2, 1.0 } },
    -- Timing.transition, OutQuad.
    transition = { duration = 0.18, curve = { 0.25, 0.46, 0.45, 0.94 } },
    -- The trail follows the continuous hand across each second.
    second = { duration = 1.0 },
}

local OUTPUT = "HDMI-A-1"
local FONT_TITLE = 15 -- round(14.5 × factor), core/Metrics.qml
local FONT_LABEL = 13

-- A cell: 40 tall, the cell fill at cell.opacity, the lit rim, blurred.
local function cell(t: { [any]: any })
    t.height = 40
    t.fill = theme.cellFill
    t.rim_top = theme.rim
    t.rim_bottom = theme.line
    t.blur = true
    return g.capsule(t)
end

-- A tissue: a pill of the elevated colour at tissue.opacity, drawn only
-- around its cells; padding 2, gap twice the padding.
local function tissue(cells: { any })
    local t: { [any]: any } = { fill = theme.tissueFill, hollow = true, padding = 2, gap = 4 }
    for i, c in cells do
        t[i] = c
    end
    return g.capsule(t)
end

-- ---- Workspaces (cells/workspaces/Workspaces.qml) -------------------------
local workspaces = cell {
    role = "status",
    name = "Workspace",
    padding_x = 16,
    gap = 11,
    min_width = 96,
    g.icon {
        icon = "workspaces",
        width = 16,
        height = 16,
        gradient_top = theme.primaryTop,
        gradient_bottom = theme.primaryBottom,
    },
    g.text {
        color = theme.text,
        size = function()
            local w = niri:active(OUTPUT)
            return if w and w.name then FONT_TITLE else FONT_LABEL
        end,
        font = function()
            local w = niri:active(OUTPUT)
            return if w and w.name then "Spectral" else "Orbitron"
        end,
        text = function()
            local w = niri:active(OUTPUT)
            return if w == nil then "" elseif w.name then w.name else tostring(w.idx)
        end,
    },
}

-- ---- Window title (cells/window_title/WindowTitle.qml) --------------------
-- A spinner glyph at the start of a title means the window is working: it
-- is taken out and the loader is shown instead.
local function spins(code: number): boolean
    return (code >= 0x25D0 and code <= 0x25D3) or (code >= 0x25F4 and code <= 0x25F7)
        or (code >= 0x2800 and code <= 0x28FF) or (code >= 0x1F550 and code <= 0x1F55B)
        or (code >= 0x1F311 and code <= 0x1F318)
end

local function reading(): (boolean, string)
    local w = niri:focused_on(OUTPUT)
    local title = if w then w.title or "" else ""
    if title == "" then
        return false, ""
    end
    local first = utf8.codepoint(title, 1)
    if not spins(first) then
        return false, title
    end
    local rest = string.sub(title, utf8.offset(title, 2) :: number)
    return true, (string.gsub(rest, "^[%s\u{a0}]+", ""))
end

-- The loader (components/Sweep.qml), 16: a capsule outline 18 × 7 of a 24
-- grid, and a segment sliding inside it, there and back in Timing.loader.
local LOADER = 16 / 24
local title = cell {
    role = "label",
    name = "Window title",
    padding_x = 5,
    gap = 10,
    present = function() return niri:focused_on(OUTPUT) ~= nil end,
    g.capsule {
        width = 30,
        height = 30,
        fill = theme.iconDisc,
        rim_top = theme.textHalf,
        rim_bottom = theme.textHalf,
        g.icon {
            width = 30 * 0.62,
            height = 30 * 0.62,
            color = theme.textMuted,
            icon = function()
                local w = niri:focused_on(OUTPUT)
                return (w and g.app_icon(w.app_id)) or "app-fallback"
            end,
        },
    },
    g.box {
        width = 16,
        height = 16,
        present = function() return (reading()) end,
        g.capsule {
            width = 18 * LOADER,
            height = 7 * LOADER,
            stroke = 1.5 * LOADER,
            rim_top = theme.textMuted,
            rim_bottom = theme.textMuted,
            justify = "start",
            padding_x = 1.2 * LOADER,
            g.capsule {
                width = 6 * LOADER,
                height = 2.6 * LOADER,
                fill = theme.textMuted,
                sway = 1.2,
                sway_range = 7.6 * LOADER,
            },
        },
    },
    g.text {
        font = "Spectral",
        size = FONT_TITLE,
        color = theme.text,
        max_width = 1920 * 0.28,
        text = function()
            local _, text = reading()
            return text
        end,
    },
}

-- ---- Vitals (components/Vital.qml): a 64-unit grid in a 26 box ------------
local UNIT = 26 / 64
local RING_RADIUS = 24 * UNIT
local RING_STROKE = math.max(1, 1.5 * UNIT)

local function ring(load: () -> number, inner: { any })
    local t: { [any]: any } = {
        width = 2 * RING_RADIUS + RING_STROKE,
        height = 2 * RING_RADIUS + RING_STROKE,
        stroke = RING_STROKE,
        rim_top = function() return theme.gradTop(theme.stateFor(load())) end,
        rim_bottom = function() return theme.gradBottom(theme.stateFor(load())) end,
        transition = { rim_top = "theme", rim_bottom = "theme" },
    }
    for i, c in inner do
        t[i] = c
    end
    return g.box { width = 26, height = 26, justify = "center", g.capsule(t) }
end

local cpu = function(): number return vitals.cpu end
local ram = function(): number return vitals.ram end
local gpu = function(): number return vitals.gpu or 0 end
local VESSEL = (RING_RADIUS - RING_STROKE) * 2

local indicators = cell {
    role = "group",
    name = "Vitals",
    padding_x = 12,
    gap = 10,
    min_width = 90,
    -- The processor: a dot that beats, 45 a minute at idle, 140 at full load.
    ring(cpu, {
        g.capsule {
            width = 16 * UNIT,
            height = 16 * UNIT,
            fill = function() return theme.stateFor(cpu()) end,
            pulse = function() return 45 + cpu() * 95 end,
        },
    }),
    -- The memory: a liquid at its level, its surface a travelling wave.
    ring(ram, {
        g.capsule {
            width = VESSEL,
            height = VESSEL,
            fill = function() return theme.stateFor(ram()) end,
            level = ram,
            wave = 6 * UNIT,
            wave_period = 46 * UNIT,
            wave_time = 2.4,
            transition = { level = "reflow" },
        },
    }),
    -- The graphics card: a satellite on the ring, its period the clock's.
    ring(gpu, {
        g.capsule {
            width = 12 * UNIT,
            height = 12 * UNIT,
            fill = function() return theme.stateFor(gpu()) end,
            orbit = function() return 1 / (6 - vitals.gpu_clock * 4.8) end,
            orbit_radius = RING_RADIUS,
        },
    }),
}

-- ---- Clock (cells/clock/Clock.qml, components/Dial.qml) -------------------
local DIAL = 26 / 64
local STROKE = math.max(1, 1.5 * DIAL)
local function two(n: number): string
    return string.format("%02d", n)
end

local time = cell {
    role = "timer",
    name = "Clock",
    padding_x = 14,
    gap = 8,
    g.text {
        font = "Spectral",
        size = 19,
        tabular = true,
        color = theme.text,
        text = function() return two(clock.hour) .. ":" .. two(clock.minute) end,
    },
    g.box {
        width = 26,
        height = 26,
        justify = "center",
        -- The track, the line at 0.4.
        g.capsule {
            overlay = true,
            width = 2 * 24 * DIAL + STROKE,
            height = 2 * 24 * DIAL + STROKE,
            stroke = STROKE,
            rim_top = theme.trackLine,
            rim_bottom = theme.trackLine,
        },
        -- One light over the whole face, the primary's 165° gradient,
        -- revealed by the trail, the sector and the disc (Dial.qml).
        -- The trail the disc draws behind it since twelve.
        g.capsule {
            overlay = true,
            width = 2 * 24 * DIAL + STROKE,
            height = 2 * 24 * DIAL + STROKE,
            stroke = STROKE,
            rim_top = theme.primaryTop,
            rim_bottom = theme.primaryBottom,
            arc = function() return clock.second / 60 end,
            arc_rate = 1 / 60,
        },
        -- The hour so far, a sector of radius 13, eased as Dial's Follower.
        g.capsule {
            overlay = true,
            width = 26 * DIAL,
            height = 26 * DIAL,
            fill = theme.primaryTop,
            fill_bottom = theme.primaryBottom,
            -- Dial eases it over Timing.transition, but a second moves it by
            -- 1/3600 of a turn, a fiftieth of a pixel: the easing cannot be
            -- seen, and here it would only cost frames.
            arc = function() return (clock.minute * 60 + clock.second) / 3600 end,
        },
        -- The second hand, continuous (MinuteHand.qml): one turn a minute,
        -- set against the wall clock on every second.
        g.capsule {
            overlay = true,
            width = 12 * DIAL,
            height = 12 * DIAL,
            fill = theme.primaryTop,
            fill_bottom = theme.primaryBottom,
            orbit = 1 / 60,
            orbit_radius = 24 * DIAL,
            angle = function() return clock.second / 60 end,
        },
    },
}

return g.surface {
    anchor = { "top", "left", "right" },
    margin = { top = 12 },
    reserve = false,

    g.box {
        grow = 1,
        height = 44,
        padding_x = 12,
        justify = "between",
        tissue { workspaces, title },
        tissue { indicators, time },
    },
}
