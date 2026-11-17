--!strict
-- What a shell is made of besides drawing: the surfaces it can open, the
-- tools that check and show it, and what makes it usable by everyone.
-- Then the end: the rest goes, and the mark draws itself once more, with
-- where to find it.
local g = require("@genoma")
local style = require("style")
local logo = require("logo")

local u = style.u
local cap = style.cap
local t = style.t

-- A screen in miniature, with what a surface of that kind puts on it.
local function screen(kind: string): any
    local W, H = 250, 150
    local parts: { any } = {}
    local function piece(props: { [any]: any }): any
        props.fill = props.fill or function() return g.colour.alpha(t.colour("accent"), 0.85) end
        return cap(props)
    end
    if kind == "layer" then
        table.insert(parts, piece { x = 10, y = 8, w = W - 20, h = 14, r = 7 })
        table.insert(parts, piece { x = 80, y = H - 24, w = 90, h = 14, r = 7, fill = function() return g.colour.alpha(t.colour("accent2"), 0.85) end })
    elseif kind == "popup" then
        table.insert(parts, piece { x = 10, y = 8, w = W - 20, h = 14, r = 7 })
        table.insert(parts, piece { x = 150, y = 30, w = 80, h = 70, r = 10, fill = function() return g.colour.alpha(t.colour("accent2"), 0.85) end })
    elseif kind == "window" then
        table.insert(parts, piece { x = 30, y = 24, w = 130, h = 100, r = 8, fill = function() return g.colour.alpha(t.colour("text"), 0.18) end })
        table.insert(parts, piece { x = 30, y = 24, w = 130, h = 14, r = 7 })
    elseif kind == "lock" then
        table.insert(parts, piece { x = 0, y = 0, w = W, h = H, r = 14, fill = function() return g.colour.alpha(t.colour("accent"), 0.25) end })
        table.insert(parts, piece { x = 95, y = 50, w = 60, h = 60, fill = function() return g.colour.alpha(t.colour("text"), 0.85) end })
    end
    local box = {
        width = function() return u(W) end,
        height = function() return u(H) end,
        g.capsule {
            overlay = true,
            attach = "corner",
            width = function() return u(W) end,
            height = function() return u(H) end,
            radius = function() return u(14) end,
            fill = function() return g.colour.alpha(t.colour("text"), 0.05) end,
            rim_top = function() return g.colour.alpha(t.colour("text"), 0.3) end,
            rim_bottom = function() return g.colour.alpha(t.colour("text"), 0.08) end,
        },
    }
    for _, p in parts do table.insert(box, p) end
    return g.box(box)
end

local TOOLS = {
    { "genoma check", "every file, type-checked" },
    { "genoma types", "the API as .d.luau" },
    { "genoma snapshot", "a shell drawn offscreen" },
    { "genoma ipc", "call into a running shell" },
    { "genoma reference", "every node and property" },
}
local ACCESS = {
    "AccessKit: roles and names for screen readers",
    "Keyboard walk, with a focus ring",
    "Input methods for every script",
    "Touch, pen and touchpad gestures",
}

return {
    key = "closing",
    kicker = "And more",
    title = "A toolkit for shells",
    caption = "",
    duration = 9,
    code = [[
-- A whole shell, in one file.
local g = require("@genoma")
local clock = require("@genoma/clock")

return g.surface {
    anchor = "top",
    g.capsule {
        padding_x = 16, height = 36,
        g.text { text = function()
            return string.format("%02d:%02d",
                clock.hour, clock.minute)
        end },
    },
}

-- genoma shell.luau]],

    build = function(ctx: { [string]: any }): any
        -- 1: the three columns; 2: they go and the mark comes back.
        local phase = g.signal(0)
        g.watch(ctx.arrived, function(on: boolean)
            if on and g.untracked(function() return phase.value end) == 0 then phase.value = 1 end
        end)
        -- The ending comes when asked for: the next key or click (or the
        -- deck playing by itself) fades the columns and draws the mark.
        local timers: { any } = {}
        local function stop() for _, tm in timers do tm:stop() end timers = {} end
        ctx.steps {
            next = function(): boolean
                if g.untracked(function() return phase.value end) ~= 1 then return false end
                stop()
                phase.value = 2
                table.insert(timers, g.after(0.8, function()
                    phase.value = 3
                    style.quiet.value = true
                end))
                table.insert(timers, g.after(2.7, function() phase.value = 4 end))
                return true
            end,
            previous = function(): boolean
                if g.untracked(function() return phase.value end) < 2 then return false end
                stop()
                style.quiet.value = false
                phase.value = 1
                return true
            end,
        }
        g.cleanup(function() style.quiet.value = false end)
        local function columns(): boolean return phase.value == 1 end

        local function rise(n: number, node: any): any
            return g.box {
                opacity = function() return if columns() then 1 else 0 end,
                offset_y = function() return if columns() then 0 else u(if phase.value == 0 then 30 else -20) end,
                blur_radius = function() return if columns() then 0 else u(if phase.value == 0 then 0 else 8) end,
                transition = {
                    opacity = function() return if phase.value <= 1 then "rise_" .. n else "fade" end,
                    offset_y = function() return if phase.value <= 1 then "rise_" .. n else "fade" end,
                    blur_radius = "fade",
                },
                node,
            }
        end

        local function heading(text: string): any
            return style.text("kicker", { text = text, color = style.c("accent") })
        end

        local surfaces = { direction = "column", align = "start", gap = function() return u(20) end, heading("SURFACES") }
        for _, s in { { "layer", "Layer shell: bars, docks, overlays" }, { "popup", "Popups placed by the compositor" }, { "window", "Windows it manages, with a title bar" }, { "lock", "The session lock, and a greeter" } } do
            table.insert(surfaces, g.box {
                direction = "row",
                align = "center",
                gap = function() return u(20) end,
                screen(s[1]),
                style.text("body", { text = s[2], wrap = true, max_width = function() return u(200) end, color = style.c("muted") }),
            })
        end

        local tools = { direction = "column", align = "start", gap = function() return u(18) end, heading("TOOLS") }
        for _, tool in TOOLS do
            table.insert(tools, g.box {
                direction = "column",
                align = "start",
                gap = function() return u(4) end,
                style.text("code", { text = "$ " .. tool[1], color = style.c("glow") }),
                style.text("body", { text = tool[2], color = style.c("muted") }),
            })
        end

        local access = { direction = "column", align = "start", gap = function() return u(18) end, heading("FOR EVERYONE") }
        for _, a in ACCESS do
            table.insert(access, g.box {
                direction = "row",
                align = "center",
                gap = function() return u(14) end,
                cap { overlay = false, w = 22, h = 3, fill_stops = style.accent_stops, fill_across = true },
                style.text("body", { text = a, wrap = true, max_width = function() return u(380) end, color = style.c("text") }),
            })
        end

        local function closing(n: number): boolean return phase.value >= n end

        return g.box {
            width = function() return u(style.W) end,
            height = function() return u(style.H) end,
            -- The columns.
            g.box {
                overlay = true,
                attach = "corner",
                offset_x = function() return u(120) end,
                offset_y = function() return u(130) end,
                direction = "column",
                align = "start",
                gap = function() return u(50) end,
                rise(1, g.box {
                    direction = "column",
                    align = "start",
                    gap = function() return u(14) end,
                    style.text("kicker", { text = "18  ·  AND MORE", color = style.c("accent") }),
                    style.text("title", { text = "A toolkit for shells" }),
                }),
                g.box {
                    direction = "row",
                    align = "start",
                    gap = function() return u(90) end,
                    rise(2, g.box(surfaces)),
                    rise(3, g.box(tools)),
                    rise(4, g.box(access)),
                },
            },
            -- The end: the mark again, and where it lives.
            g.box {
                overlay = true,
                attach = "corner",
                width = function() return u(style.W) end,
                height = function() return u(style.H) end,
                direction = "column",
                align = "center",
                justify = "center",
                gap = function() return u(30) end,
                opacity = function() return if closing(3) then 1 else 0 end,
                transition = { opacity = "fade" },
                logo.new {
                    height = function() return u(380) end,
                    drawn = function() return closing(3) end,
                },
                g.box {
                    direction = "column",
                    align = "center",
                    gap = function() return u(12) end,
                    opacity = function() return if closing(4) then 1 else 0 end,
                    offset_y = function() return if closing(4) then 0 else u(20) end,
                    transition = { opacity = "rise", offset_y = "rise" },
                    style.text("hero", { text = "GENOMA", size = nil }),
                    style.text("lead", { text = "github.com/AkusenArcade/Genoma", color = style.c("glow") }),
                    style.text("label", { text = "AUR  ·  genoma", color = style.c("muted") }),
                },
            },
        }
    end,
}
