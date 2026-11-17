--!strict
-- The first slide: a START in the middle of an empty screen; pressed (or
-- → , or A), it goes, and the mark draws itself in its place, then steps
-- aside for the name, what it is, and what it does.
local g = require("@genoma")
local style = require("style")
local logo = require("logo")

local u = style.u

local FEATURES = {
    "Interfaces in Luau, reloaded while they run",
    "Drawn on the GPU as distance fields, through Vulkan",
    "Reactive to the core: at rest, it costs nothing",
    "Springs, depth, morphing, particles and shaders",
    "Twenty-six system services, built in, never polled",
    "Layer shell, lock screen, popups and windows",
}

local LOGO_H = 600
local GAP = 140
local COLUMN = 820

return {
    key = "title",
    kicker = "Genoma",
    title = "A Living Toolkit",
    duration = 12,
    code = [[
-- The mark draws itself: a path whose
-- stroke runs from 0 to 1 of its length.
g.path {
    d = capsule,
    stroke = 1.5 * scale,
    cap = "round",
    stroke_stops = {
        { 0, "#dbf4a7" },
        { 1, "#94bc41" },
    },
    trim_end = function()
        return if drawn.value then 1 else 0
    end,
    transition = { trim_end = "draw" },
}]],

    build = function(ctx: { [string]: any }): any
        -- 0: START waits; 1: the mark draws; 2: it steps aside and the name
        -- comes; 3: the list.
        local phase = g.signal(0)
        local started = g.signal(false)
        local function start()
            if g.untracked(function() return started.value end) then return end
            started.value = true
            g.after(0.5, function() phase.value = 1 end)
            g.after(2.4, function() phase.value = 2 end)
            g.after(3.4, function() phase.value = 3 end)
        end
        -- The deck's next key starts it, once; after that it goes on.
        ctx.steps {
            next = function(): boolean
                if g.untracked(function() return started.value end) then return false end
                start()
                return true
            end,
            waiting = function(): boolean return not g.untracked(function() return started.value end) end,
        }

        local function at(n: number): () -> boolean return function() return phase.value >= n end end

        -- The mark sits in the middle of the screen until the name comes,
        -- then moves to the left of the composition, which is centred whole.
        local function logo_shift(): number
            if phase.value >= 2 then return 0 end
            return u(GAP + COLUMN) / 2
        end

        local function rise(n: number, shown: () -> boolean, node: any): any
            return g.box {
                opacity = function() return if shown() then 1 else 0 end,
                offset_y = function() return if shown() then 0 else u(30) end,
                transition = { opacity = "rise_" .. n, offset_y = "rise_" .. n },
                node,
            }
        end

        -- The name, a letter at a time, each from a little lower and further
        -- apart than it rests.
        local function name(): any
            local row = { direction = "row" }
            for i = 1, #"GENOMA" do
                local shown = at(2)
                table.insert(row, g.box {
                    -- Not quite 0 while hidden: their blur is made out of sight.
                    opacity = function() return if shown() then 1 else 0.002 end,
                    offset_y = function() return if shown() then 0 else u(40) end,
                    offset_x = function() return if shown() then 0 else u((i - 3.5) * 18) end,
                    blur_radius = function() return if shown() then 0 else u(6) end,
                    transition = { opacity = "rise_" .. i, offset_y = "rise_" .. i, offset_x = "rise_" .. i, blur_radius = "rise_" .. i },
                    style.text("hero", { text = string.sub("GENOMA", i, i), color = style.c("text") }),
                })
            end
            return g.box(row)
        end

        local list = {
            direction = "column",
            align = "start",
            gap = function() return u(18) end,
        }
        for i, f in FEATURES do
            table.insert(list, rise(i + 2, at(3), g.box {
                direction = "row",
                align = "center",
                gap = function() return u(20) end,
                g.capsule {
                    width = function() return u(26) end,
                    height = function() return u(3) end,
                    fill_stops = style.accent_stops,
                    fill_across = true,
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                },
                style.text("body", { text = f, color = style.c("text") }),
            }))
        end

        -- START: a pill of glass in the accent's light, with a play mark;
        -- pressed, it narrows to nothing as it fades, where the mark begins.
        local hovered = g.signal(false)
        local function scaled(d: string): string
            local k = style.k.value
            return (string.gsub(d, "%-?%d+%.?%d*", function(n: string) return string.format("%.2f", tonumber(n) :: number * k) end))
        end
        local start_button = g.capsule {
            overlay = true,
            attach = "center",
            role = "button",
            name = "Start",
            cursor = "pointer",
            width = function() return u(if started.value then 84 else 280) end,
            height = function() return u(84) end,
            opacity = function() return if started.value then 0 else 1 end,
            fill = function() return g.colour.alpha(style.t.colour("accent"), if hovered.value then 0.22 else 0.12) end,
            rim_top = function() return g.colour.alpha(style.t.colour("glow"), 0.9) end,
            rim_bottom = function() return g.colour.alpha(style.t.colour("accent"), 0.25) end,
            shadow = function() return g.colour.alpha(style.t.colour("accent"), if hovered.value then 0.6 else 0.35) end,
            shadow_blur = function() return u(48) end,
            transition = { width = "fade", opacity = "fade", fill = "fade", shadow = "fade" },
            direction = "row",
            align = "center",
            justify = "center",
            gap = function() return u(18) end,
            on_hover = function(on: boolean) hovered.value = on end,
            on_click = start,
            g.path {
                width = function() return u(18) end,
                height = function() return u(22) end,
                d = function() return scaled("M 2 1 L 17 11 L 2 21 Z") end,
                fill = style.c("text"),
                join = "round",
                opacity = function() return if started.value then 0 else 1 end,
                transition = { opacity = "fade" },
            },
            g.text {
                text = "START",
                font = style.display,
                weight = 600,
                size = function() return math.floor(u(26)) end,
                tracking = 0.2,
                opacity = function() return if started.value then 0 else 1 end,
                transition = { opacity = "fade" },
                color = style.c("text"),
            },
        }

        return g.box {
            width = function() return u(style.W) end,
            height = function() return u(style.H) end,
            align = "center",
            justify = "center",
            direction = "row",
            gap = function() return u(GAP) end,
            start_button,
            logo.new {
                height = function() return u(LOGO_H) end,
                drawn = at(1),
                box = {
                    offset_x = logo_shift,
                    transition = { offset_x = "rise" },
                },
            },
            g.box {
                direction = "column",
                align = "start",
                width = function() return u(COLUMN) end,
                gap = function() return u(10) end,
                name(),
                rise(4, at(2), style.text("title", {
                    text = "A Living Toolkit",
                    italic = true,
                    color = style.c("glow"),
                })),
                rise(5, at(2), style.text("lead", {
                    text = "A Wayland shell toolkit, written in Rust and scripted in Luau.",
                    color = style.c("muted"),
                })),
                g.box { height = function() return u(34) end },
                g.box(list),
            },
        }
    end,
}
