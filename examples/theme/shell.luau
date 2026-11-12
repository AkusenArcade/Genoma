--!strict
-- A shell's own theme (@genoma/theme): a palette of four roles, colours
-- derived from them, sizes on a density step, a type scale and a timing
-- set, every node bound to it. Press a palette and the whole panel crosses
-- to it at once, the kit's controls with it (kit.theme.follow). "File"
-- follows palette.json beside this file: edit it and save.
--
--   genoma examples/theme
local g = require("@genoma")
local kit = require("@genoma/kit")
local theme = require("@genoma/theme")

local t = theme.new {
    roles = { background = "#12160f", text = "#eaf1e6", primary = "#b9de6b", secondary = "#3fbfa0" },
    -- Say a state, whatever the palette: the same three on every palette,
    -- one set for a dark theme and one for a light.
    fixed = {
        calm = { dark = "#6ee7a6", light = "#2e9c6a" },
        active = { dark = "#f2c14e", light = "#b37414" },
        alert = { dark = "#ff6b5d", light = "#c43a2e" },
    },
    derive = {
        surface = function(c) return g.colour.lift(c.background, 0.045, c.dark) end,
        well = function(c) return g.colour.lift(c.background, 0.09, c.dark) end,
        line = function(c) return g.colour.mix(c.background, c.text, if c.dark then 0.16 else 0.22) end,
        muted = function(c) return g.colour.alpha(c.text, 0.7) end,
        on_primary = function(c)
            return if g.colour.contrast(c.background, c.primary) >= g.colour.contrast(c.text, c.primary)
                then c.background
                else c.text
        end,
    },
    sizes = { radius = 18, radius_well = 10, gap = 14, row = 32, padding = 20, hairline = 1 },
    unscaled = { "hairline" },
    steps = { compact = 0.85, normal = 1, comfortable = 1.15 },
    fonts = { body = "sans-serif" },
    type = {
        title = { font = "body", size = 20, weight = 700 },
        label = { font = "body", size = 12, weight = 600, tracking = 0.06, upper = true },
        body = { font = "body", size = 14 },
        chip = { font = "body", size = 13, weight = 600 },
    },
    timing = {
        open = { duration = 0.3, curve = { 0.2, 0.9, 0.3, 1.1 } },
        close = { duration = 0.18, curve = { 0.5, 0, 0.9, 0.5 } },
        theme = { duration = 0.4, curve = { 0.455, 0.03, 0.515, 0.955 } },
    },
}

-- The kit's controls take their look from the same theme.
kit.theme.follow(t)

local palettes = {
    { name = "Moss", roles = { background = "#12160f", text = "#eaf1e6", primary = "#b9de6b", secondary = "#3fbfa0" } },
    { name = "Dusk", roles = { background = "#1a1726", text = "#ece6f5", primary = "#c3a6ff", secondary = "#ff9e7a" } },
    { name = "Ember", roles = { background = "#1f120e", text = "#f6e9e1", primary = "#ff8a4c", secondary = "#ffd166" } },
    { name = "Paper", roles = { background = "#f3f0e8", text = "#22201b", primary = "#3a6ea5", secondary = "#c0532b" } },
}
local chosen = g.signal("Moss")

local function colour(name: string): () -> string
    return function() return t.colour(name) end
end
local function size(name: string): () -> number
    return function() return t.size(name) end
end

local function label(text: string): any
    return g.text(t.text("label", { color = colour("muted"), text = text }))
end

-- A palette's chip, drawn in its own colours: where the palette goes, not
-- the crossing on its way there.
local function chip(p: { [string]: any }): any
    return g.capsule {
        id = "palette-" .. string.lower(p.name),
        height = size("row"),
        padding_x = function() return 14 * t.factor() end,
        gap = 8,
        radius = function() return t.size("row") / 2 end,
        fill = p.roles.background,
        outline = function() return if chosen.value == p.name then p.roles.primary else p.roles.text .. "33" end,
        outline_width = function() return if chosen.value == p.name then 2 else t.size("hairline") end,
        on_click = function()
            chosen.value = p.name
            t.source(nil)
            t.set(p.roles)
        end,
        g.capsule { width = 10, height = 10, radius = 5, fill = p.roles.primary },
        g.capsule { width = 10, height = 10, radius = 5, fill = p.roles.secondary },
        g.text(t.text("chip", { color = p.roles.text, text = p.name })),
    }
end

local chips = {}
for _, p in palettes do table.insert(chips, chip(p)) end
table.insert(chips, kit.button {
    id = "palette-file",
    text = "File",
    on_click = function()
        chosen.value = "File"
        t.source { path = g.dir .. "/palette.json" }
    end,
})

local steps = { "compact", "normal", "comfortable" }
local opened = g.signal(false)

local function state(name: string, says: string): any
    return g.box {
        gap = 8,
        g.capsule { width = 10, height = 10, radius = 5, fill = colour(name) },
        g.text(t.text("body", { color = colour("text"), text = says })),
    }
end

return g.surface {
    anchor = "top",
    margin = { top = 100 },
    namespace = "genoma-theme",

    g.capsule {
        direction = "column",
        align = "start",
        gap = size("gap"),
        padding = size("padding"),
        radius = size("radius"),
        min_width = function() return 520 * t.factor() end,
        fill = colour("surface"),
        outline = colour("line"),
        outline_width = size("hairline"),

        g.text(t.text("title", { color = colour("text"), text = "A theme of one's own" })),
        g.text(t.text("body", {
            color = colour("muted"),
            wrap = true,
            max_width = function() return 480 * t.factor() end,
            text = "Every colour, size and time here is a token. Press a palette: the panel and the kit's controls cross to it together.",
        })),

        label("Palette"),
        g.box { gap = 8, table.unpack(chips) },

        label("Density"),
        kit.tabs {
            items = { "Compact", "Normal", "Comfortable" },
            selected = function() return table.find(steps, t.step()) or 2 end,
            on_select = function(i) t.step(steps[i]) end,
        },

        label("Speed"),
        g.box {
            gap = 12,
            kit.slider {
                name = "Speed",
                width = 260,
                value = function() return (t.speed() - 0.25) / 1.75 end,
                on_change = function(v) t.speed(0.25 + v * 1.75) end,
            },
            g.text(t.text("body", {
                color = colour("muted"),
                tabular = true,
                text = function() return string.format("%.2f×  open %.0f ms", t.speed(), t.seconds("open") * 1000) end,
            })),
        },

        label("State"),
        g.box { gap = 18, state("calm", "Calm"), state("active", "Active"), state("alert", "Alert") },

        label("Times"),
        g.capsule {
            id = "times",
            direction = "column",
            align = "start",
            gap = 8,
            padding = 14,
            radius = size("radius_well"),
            width = function() return (if opened.value then 480 else 200) * t.factor() end,
            height = function() return (if opened.value then 120 else 44) * t.factor() end,
            clip = true,
            transition = { width = "open", height = "open" },
            fill = colour("well"),
            on_click = function() opened.value = not opened.value end,
            g.text(t.text("body", {
                color = colour("text"),
                text = function() return if opened.value then "Press again to close." else "Press to open" end,
            })),
            g.capsule {
                present = function() return opened.value end,
                height = size("row"),
                padding_x = 14,
                radius = function() return t.size("row") / 2 end,
                fill = colour("primary"),
                g.text(t.text("chip", { color = colour("on_primary"), text = "Grown over `open`" })),
            },
        },
    },
}
