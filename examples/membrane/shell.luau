--!strict
-- Milestone 2: the content of Bioma's top membrane, for the side-by-side
-- measurement (PRD §11). Workspaces and the window title on the left, the
-- vitals and the clock on the right, each a 40-unit cell with the lit rim.
local g = require("@genoma")
local clock = require("@genoma/clock")
local niri = require("@genoma/niri")
local vitals = require("@genoma/vitals")
local theme = require("theme")

g.timing {
    reflow = { duration = 0.22, curve = { 0.2, 0.0, 0.0, 1.0 } },
    theme = { duration = 0.25, curve = { 0.4, 0.0, 0.2, 1.0 } },
}

-- The output this membrane is on; GENOMA_OUTPUT places the surface.
local OUTPUT = "HDMI-A-1"

local function state(load: number): string
    return if load >= 0.8 then theme.alert elseif load >= 0.3 then theme.active else theme.calm
end

local function cell(t: { [any]: any })
    t.height = 40
    t.fill = theme.glass
    t.rim_top = theme.rim
    t.rim_bottom = theme.line
    return g.capsule(t)
end

-- An indicator ring, 26 units, its colour the domain's state.
local function ring(load: () -> number, inner: { [any]: any })
    return g.capsule {
        width = 26,
        height = 26,
        rim_top = function() return state(load()) end,
        rim_bottom = function() return state(load()) end,
        transition = { rim_top = "theme", rim_bottom = "theme" },
        g.capsule(inner),
    }
end

local cpu = function(): number return vitals.cpu end
local ram = function(): number return vitals.ram end
local gpu = function(): number return vitals.gpu or 0 end

local workspaces = cell {
    role = "status",
    name = "Workspace",
    padding_x = 16,
    gap = 10,
    min_width = 96,
    -- The two stacked bars.
    g.box {
        direction = "column",
        gap = 3,
        g.capsule { width = 14, height = 3, fill = theme.primary },
        g.capsule { width = 14, height = 3, fill = theme.line },
    },
    g.text {
        size = 14.5,
        color = theme.text,
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

local title = cell {
    role = "label",
    name = "Window title",
    padding_x = 16,
    present = function() return niri.title ~= nil end,
    g.text {
        font = "Spectral",
        size = 14.5,
        color = theme.text,
        max_width = 520,
        text = function() return niri.title or "" end,
    },
}

local indicators = cell {
    role = "group",
    name = "Vitals",
    padding_x = 12,
    gap = 10,
    min_width = 90,
    -- The processor: a dot that beats, 45 a minute at idle, 140 at full load.
    ring(cpu, {
        width = 8,
        height = 8,
        fill = function() return state(cpu()) end,
        pulse = function() return 45 + cpu() * 95 end,
    }),
    -- The memory: a liquid at the level of what is in use.
    ring(ram, {
        width = 20,
        height = 20,
        fill = function() return state(ram()) .. "b0" end,
        level = ram,
        transition = { level = "reflow" },
    }),
    -- The graphics card: a satellite that never stops, faster with load.
    ring(gpu, {
        width = 6,
        height = 6,
        fill = function() return state(gpu()) end,
        orbit = function() return 1 / (6 - gpu() * 4.8) end,
        orbit_radius = 13,
    }),
}

local function two(n: number): string
    return string.format("%02d", n)
end

local time = cell {
    role = "timer",
    name = "Clock",
    padding_x = 18,
    g.text {
        font = "Orbitron",
        size = 15,
        color = theme.text,
        text = function() return two(clock.hour) .. ":" .. two(clock.minute) end,
    },
}

return g.surface {
    anchor = { "top", "left", "right" },
    -- Below Bioma's own membrane while the two run side by side.
    margin = { top = 56 },
    reserve = false,

    g.box {
        grow = 1,
        height = 52,
        padding_x = 12,
        justify = "between",
        g.box { gap = 8, workspaces, title },
        g.box { gap = 8, indicators, time },
    },
}
