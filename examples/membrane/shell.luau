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
    -- An expansion's cascade (Cell.qml opening and closing): Timing.grow 100
    -- on easeOpenFlat, the panel 2 × stagger later; closing on easeClose,
    -- Timing.close 150, the thread 2 × stagger later.
    thread_open = { duration = 0.10, curve = { 0.2, 0.9, 0.3, 1.0 } },
    thread_close = { duration = 0.118, delay = 0.032, curve = { 0.5, 0.0, 0.9, 0.5 } },
    panel_open = { duration = 0.10, delay = 0.032, curve = { 0.2, 0.9, 0.3, 1.0 } },
    panel_close = { duration = 0.15, curve = { 0.5, 0.0, 0.9, 0.5 } },
    content_open = { duration = 0.12, delay = 0.132 },
    content_close = { duration = 0.06 },
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
-- At rest the mark and the name; a click opens the list of this output's
-- workspaces (WorkspaceList.qml), grown out of the cell as Cell.qml grows
-- an expansion: the thread first, the panel from the node at its tip, the
-- content once the panel has its size.
local open = g.state("workspaces.open", false)
local mark = g.state("workspaces.mark", 1)
local GAP = 14 -- marginEdge 12 + tissuePadding 2
local ROW, BUTTON, ROW_PAD, WIDTH = 44, 36, 8, 216
local MAX_ROWS = 10

local function list()
    return niri:workspaces(OUTPUT)
end

local function activeIndex(): number
    for i, w in list() do
        if w.active then return i end
    end
    return 1
end

local function setOpen(value: boolean)
    if value then mark.value = activeIndex() end
    open.value = value
end

local function jump(i: number)
    local w = list()[i]
    if w then niri:focus_workspace(w.id) end
    setOpen(false)
end

local function step(delta: number)
    local l = list()
    local i = math.clamp(activeIndex() + delta, 1, #l)
    if l[i] then niri:focus_workspace(l[i].id) end
end

local function row(i: number)
    local function ws() return list()[i] end
    return g.capsule {
        role = "button",
        width = WIDTH,
        height = ROW,
        radius = 14,
        present = function() return ws() ~= nil end,
        -- The keyboard's mark: the primary at 12% (KeyMark.qml).
        fill = function() return if mark.value == i then theme.primary12 else "#00000000" end,
        justify = "start",
        padding_left = ROW_PAD,
        gap = 12,
        on_click = function() jump(i) end,
        on_hover = function(inside) if inside then mark.value = i end end,
        -- The button: filled where you are, a ring where something is, a
        -- fainter ring for the free place (Bioma draws that one dashed).
        g.capsule {
            width = BUTTON,
            height = BUTTON,
            fill = function() local w = ws(); return if w and w.active then theme.primaryTop else "#00000000" end,
            fill_bottom = function() local w = ws(); return if w and w.active then theme.primaryBottom else "#00000000" end,
            rim_top = function() local w = ws(); return if w and w.active then "#00000000" elseif w and w.vacant then theme.lineFaint else theme.line end,
            rim_bottom = function() local w = ws(); return if w and w.active then "#00000000" elseif w and w.vacant then theme.lineFaint else theme.line end,
            g.text {
                font = "Orbitron",
                size = 14,
                tabular = true,
                color = function()
                    local w = ws()
                    return if w and w.active then theme.background elseif w and w.vacant then theme.text30 else theme.text72
                end,
                text = function() local w = ws(); return if w then tostring(w.idx) else "" end,
            },
        },
        g.text {
            font = "Spectral",
            size = 14,
            max_width = WIDTH - ROW_PAD * 2 - BUTTON - 12,
            color = function()
                local w = ws()
                return if w and w.name then (if w.active then theme.text else theme.text50) else theme.text28
            end,
            text = function() local w = ws(); return if w then (w.name or "empty") else "" end,
        },
    }
end

local rows: { [any]: any } = {}
for i = 1, MAX_ROWS do
    rows[i] = row(i)
end

local panelHeight = function(): number
    return math.min(math.max(#list(), 1), 5) * ROW + 20
end

local workspaces = cell {
    role = "status",
    name = "Workspace",
    padding_x = 16,
    gap = 11,
    min_width = 96,
    on_click = function() setOpen(not open.value) end,
    on_wheel = function(_, dy) step(if dy > 0 then 1 else -1) end,
    -- The mark (components/WorkspaceBars.qml): bars 16 × 7.5 of a 24 grid,
    -- radius 3, stroke 1.4 crisped, snapped to the pixel, 2 px apart; at
    -- 16 that is 11 × 5 at y 2 and 9, inside a 16 square.
    g.box {
        width = 16,
        height = 16,
        g.capsule {
            overlay = true, width = 11, height = 5, radius = 2, stroke = 1,
            rim_top = theme.primaryTop, rim_bottom = theme.primaryMid,
            offset_x = 0.5, offset_y = -3.5,
        },
        g.capsule {
            overlay = true, width = 11, height = 5, radius = 2, stroke = 1,
            rim_top = theme.primaryMid, rim_bottom = theme.primaryBottom,
            offset_x = 0.5, offset_y = 3.5,
        },
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
    -- The thread (Thread.qml): 1.3, from the cell's edge down the gap.
    g.capsule {
        overlay = true,
        attach = "below-center",
        width = 1.3,
        height = GAP,
        radius = 0,
        fill = theme.line,
        reach = function() return if open.value then 1 else 0 end,
        transition = { reach = function() return if open.value then "thread_open" else "thread_close" end },
    },
    -- The node at its tip.
    g.capsule {
        overlay = true,
        attach = "below-center",
        width = 5,
        height = 5,
        fill = theme.node,
        offset_y = function() return if open.value then GAP - 2.5 else -2.5 end,
        opacity = function() return if open.value then 1 else 0 end,
        transition = {
            offset_y = function() return if open.value then "thread_open" else "thread_close" end,
            opacity = function() return if open.value then "thread_open" else "thread_close" end,
        },
    },
    -- The panel, from the node outward; its content once it is grown.
    g.capsule {
        overlay = true,
        attach = "below",
        offset_y = GAP,
        width = WIDTH + 20,
        height = panelHeight,
        radius = 20,
        padding = 10,
        fill = theme.cellFill,
        rim_top = theme.rim,
        rim_bottom = theme.line,
        blur = true,
        growth = function() return if open.value then 1 else 0 end,
        transition = { growth = function() return if open.value then "panel_open" else "panel_close" end },
        catch = function() return open.value end,
        on_outside = function() setOpen(false) end,
        on_key = function(key: string)
            local n = #list()
            if key == "Escape" then
                setOpen(false)
            elseif key == "Down" then
                mark.value = math.min(mark.value + 1, n)
            elseif key == "Up" then
                mark.value = math.max(mark.value - 1, 1)
            elseif key == "Return" or key == "KP_Enter" or key == "space" then
                jump(mark.value)
            end
        end,
        -- The well (Well.qml), rows inside it.
        g.capsule {
            radius = 10,
            fill = theme.well,
            direction = "column",
            justify = "start",
            opacity = function() return if open.value then 1 else 0 end,
            transition = { opacity = function() return if open.value then "content_open" else "content_close" end },
            table.unpack(rows),
        },
    },
}

-- ---- Window title (cells/window_title/WindowTitle.qml) --------------------
-- The focused window on any output, as Niri.focusedWindow is.
-- A spinner glyph at the start of a title means the window is working: it
-- is taken out and the loader is shown instead.
local function spins(code: number): boolean
    return (code >= 0x25D0 and code <= 0x25D3) or (code >= 0x25F4 and code <= 0x25F7)
        or (code >= 0x2800 and code <= 0x28FF) or (code >= 0x1F550 and code <= 0x1F55B)
        or (code >= 0x1F311 and code <= 0x1F318)
end

local function reading(): (boolean, string)
    local w = niri.focused
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
    -- paddingLeading 5, paddingTrailing 16.
    padding_left = 5,
    padding_right = 16,
    gap = 10,
    present = function() return niri.focused ~= nil end,
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
                local w = niri.focused
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
-- Qt's curve renderer antialiases a small disc about half a pixel wider on
-- each side than an exact distance field does: measured on the satellite,
-- 2026-10-28. A pixel more of diameter draws Bioma's disc.
local QT_AA = 1
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
    padding_x = 14,
    gap = 10,
    min_width = 90,
    -- The processor: a dot that beats, 45 a minute at idle, 140 at full load.
    ring(cpu, {
        g.capsule {
            width = 16 * UNIT + QT_AA,
            height = 16 * UNIT + QT_AA,
            fill = function() return theme.gradTop(theme.stateFor(cpu())) end,
            fill_bottom = function() return theme.gradBottom(theme.stateFor(cpu())) end,
            -- The beat is the clock, not the load (cpuClockFraction).
            pulse = function() return 45 + vitals.cpu_clock * 95 end,
        },
    }),
    -- The memory: a liquid at its level, its surface a travelling wave.
    ring(ram, {
        g.capsule {
            width = VESSEL,
            height = VESSEL,
            fill = function() return theme.stateFor(ram()) end,
            level = ram,
            -- Quadratic arcs with their control 6 above and below reach half of
            -- it: the crest stands 3 off the level.
            wave = 3 * UNIT,
            wave_period = 46 * UNIT,
            wave_time = 2.4,
            transition = { level = "reflow" },
        },
    }),
    -- The graphics card: a satellite on the ring, its period the clock's.
    ring(gpu, {
        g.capsule {
            width = 12 * UNIT + QT_AA,
            height = 12 * UNIT + QT_AA,
            fill = function() return theme.gradTop(theme.stateFor(gpu())) end,
            fill_bottom = function() return theme.gradBottom(theme.stateFor(gpu())) end,
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
    -- paddingLeading 16, paddingTrailing 12.
    padding_left = 16,
    padding_right = 12,
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
    -- appearance.edge 12 is to the cells; each tissue reaches 2 beyond them.
    -- Measured against Bioma, 2026-10-28: the first cell's rim at x 12, the
    -- last at 1907, and the rims' top at y 18 in both.
    margin = { top = 12 },
    reserve = false,

    g.box {
        grow = 1,
        height = 44,
        padding_x = 10,
        justify = "between",
        tissue { workspaces, title },
        tissue { indicators, time },
    },
}
