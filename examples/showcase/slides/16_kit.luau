--!strict
-- The kit, and the theme: ready components (tabs, buttons, switches, a
-- slider driving a bar, a dropdown, a month, a colour picker) that take
-- their look from the showcase's own theme, and palettes that change it.
-- Each change of palette is one crossing: the ground, the frame and every
-- control move to it together. Leaving, the showcase's own comes back.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")
local kit = require("@genoma/kit")

local u = style.u
local cap = style.cap
local t = style.t

local PALETTES = {
    { name = "Moss", roles = { background = "#0B0F0A", text = "#F1F6EC", accent = "#B9DE6B", accent2 = "#5ED3C1" } },
    { name = "Ocean", roles = { background = "#080E18", text = "#EAF2FA", accent = "#7AB8FF", accent2 = "#5EE6D0" } },
    { name = "Ember", roles = { background = "#140C0A", text = "#FBEDE6", accent = "#FF9F5A", accent2 = "#FF5E7A" } },
    { name = "Orchid", roles = { background = "#100C16", text = "#F2ECFA", accent = "#C69CFF", accent2 = "#FF7AC6" } },
}

return {
    key = "kit",
    kicker = "Kit & theme",
    title = "Built, not drawn",
    caption = "A kit of components that take a shell's own theme. Change the palette and the whole desktop crosses to it in one move: every colour derived again, once.",
    duration = 12,
    code = [[
local t = theme.new {
    roles = { background = "#0b0f0a", text = "#f1f6ec",
              accent = "#b9de6b", accent2 = "#5ed3c1" },
    derive = {
        panel = function(c)
            return g.colour.lift(c.background, 0.05, c.dark)
        end,
    },
}
kit.theme.follow(t)   -- the kit wears it too

-- One crossing: everything moves together.
t.set { background = "#080e18", accent = "#7ab8ff" }]],

    build = function(ctx: { [string]: any }): any
        local palette = style.cycle(ctx.arrived, 2.6, #PALETTES)
        g.watch(function() return palette.value end, function(i: number)
            if g.untracked(ctx.arrived) then t.set(PALETTES[i].roles) end
        end)
        g.cleanup(function() t.set(PALETTES[1].roles) end)

        local level = g.signal(0.35)
        g.timer { every = 1.3, on_tick = function()
            if g.untracked(ctx.arrived) then level.value = if level.value < 0.5 then 0.82 else 0.28 end
        end }
        local page = g.signal(1)
        local wifi = g.signal(true)
        local bluetooth = g.signal(false)
        local day = g.signal(nil :: any)
        local colour = g.signal("#B9DE6B")

        local function heading(text: string): any
            return style.text("label", { text = text, color = style.c("faint") })
        end

        local chips: { any } = { direction = "row", gap = function() return u(12) end }
        for i, p in PALETTES do
            table.insert(chips, cap {
                overlay = false,
                h = 44,
                padding_x = function() return u(16) end,
                direction = "row",
                align = "center",
                gap = function() return u(10) end,
                fill = function() return if palette.value == i then g.colour.alpha(t.colour("text"), 0.12) else "#00000000" end,
                rim_top = function() return if palette.value == i then g.colour.alpha(t.colour("text"), 0.5) else g.colour.alpha(t.colour("text"), 0.12) end,
                rim_bottom = function() return g.colour.alpha(t.colour("text"), 0.08) end,
                transition = { fill = "fade" },
                cap { overlay = false, w = 18, h = 18, fill = p.roles.accent },
                cap { overlay = false, w = 18, h = 18, fill = p.roles.accent2 },
                style.text("body", { text = p.name, color = style.c("text") }),
            })
        end

        return frame.new {
            number = ctx.number,
            kicker = "Kit & theme",
            title = "Built, not drawn",
            caption = "A kit of components that take a shell's own theme. Change the palette and the whole desktop crosses to it in one move: every colour derived again, once.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                g.box {
                    overlay = true,
                    attach = "corner",
                    direction = "column",
                    align = "start",
                    gap = function() return u(14) end,
                    heading("PALETTES  ·  T.SET"),
                    g.box(chips),
                },
                cap {
                    x = 0, y = 120, w = 1000, h = 660, r = 34,
                    fill = style.c("panel"),
                    rim_top = function() return g.colour.alpha(t.colour("text"), 0.2) end,
                    rim_bottom = style.c("line"),
                    direction = "row",
                    align = "start",
                    padding = function() return u(36) end,
                    gap = function() return u(44) end,
                    g.box {
                        direction = "column",
                        align = "start",
                        gap = function() return u(22) end,
                        width = function() return u(470) end,
                        kit.tabs {
                            items = { "General", "Display", "About" },
                            selected = function() return page.value end,
                            on_select = function(i: number) page.value = i end,
                        },
                        heading("BUTTONS"),
                        g.box {
                            direction = "row",
                            gap = function() return u(12) end,
                            kit.button { text = "Cancel" },
                            kit.button { text = "Save", kind = "accent" },
                            kit.button { text = "Delete", kind = "danger" },
                        },
                        heading("SWITCHES"),
                        g.box {
                            direction = "row",
                            gap = function() return u(30) end,
                            align = "center",
                            kit.toggle { on = function() return wifi.value end, on_toggle = function(on: boolean) wifi.value = on end },
                            style.text("body", { text = "Wi-Fi", color = style.c("text") }),
                            kit.toggle { on = function() return bluetooth.value end, on_toggle = function(on: boolean) bluetooth.value = on end },
                            style.text("body", { text = "Bluetooth", color = style.c("text") }),
                        },
                        heading("SLIDER  ·  PROGRESS"),
                        kit.slider {
                            width = function() return u(420) end,
                            value = function() return level.value end,
                            on_change = function(v: number) level.value = v end,
                        },
                        kit.progress { width = function() return u(420) end, value = function() return level.value end },
                    },
                    g.box {
                        direction = "column",
                        align = "start",
                        gap = function() return u(22) end,
                        heading("CALENDAR"),
                        kit.calendar {
                            date = function() return day.value end,
                            on_select = function(d: any) day.value = d end,
                        },
                        heading("COLOUR"),
                        kit.color_picker {
                            color = function() return colour.value end,
                            on_change = function(hex: string) colour.value = hex end,
                            swatches = { "#B9DE6B", "#5ED3C1", "#7AB8FF", "#FF9F5A", "#C69CFF" },
                        },
                    },
                },
            },
        }
    end,
}
