--!strict
-- An application launcher on @genoma/apps: type to find (the name, its
-- words, what else describes it, letters in order), the ones used most
-- rising; Up and Down to choose, Return or a click to start, Escape to
-- leave. With nothing typed, the most used come first.
--
--   genoma examples/launcher
local g = require("@genoma")
local apps = require("@genoma/apps")
local kit = require("@genoma/kit")

local theme = kit.theme
local ROW = 44
local SHOWN = 8

local query = g.signal("")
local chosen = g.signal(1)
local found = g.computed(function() return apps:search(query.value, 200) end)
g.watch(function() return query.value end, function() chosen.value = 1 end)

local function choose(by: number)
    local count = #found.value
    if count > 0 then chosen.value = math.clamp(chosen.value + by, 1, count) end
end

local function start(app: any?)
    if not app then return end
    apps:launch(app.id)
    g.quit()
end

return g.surface {
    anchor = {},
    keyboard = "exclusive",
    namespace = "genoma-launcher",

    g.capsule {
        direction = "column",
        align = "stretch",
        gap = 10,
        padding = 14,
        radius = 16,
        width = 480,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,

        g.field {
            size = 17,
            color = function() return theme.get("text") end,
            caret_color = function() return theme.get("accent") end,
            selection_color = function() return theme.get("selection") end,
            placeholder = "Find an application…",
            placeholder_color = function() return theme.get("faint") end,
            focused = true,
            text = function() return query.value end,
            on_change = function(text: string) query.value = text end,
            on_submit = function() start(found.value[chosen.value]) end,
            on_key = function(name: string)
                if name == "Escape" then g.quit()
                elseif name == "Down" then choose(1)
                elseif name == "Up" then choose(-1)
                elseif name == "Next" then choose(SHOWN - 1)
                elseif name == "Prior" then choose(-(SHOWN - 1))
                end
            end,
        },

        g.list {
            height = ROW * SHOWN,
            row_height = ROW,
            items = function() return found.value end,
            key = function(app: any) return app.id end,
            reveal = function() return chosen.value end,
            row = function(app: () -> any, place: () -> number)
                local function lit(): boolean return place() == chosen.value end
                return g.capsule {
                    role = "button",
                    cursor = "pointer",
                    gap = 12,
                    padding_x = 10,
                    radius = 10,
                    justify = "start",
                    fill = function() return if lit() then theme.get("selection") else theme.alpha(theme.get("selection"), 0) end,
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                    on_click = function() start(app()) end,
                    g.icon {
                        width = 28,
                        height = 28,
                        -- The theme's file for the icon's name: drawn in its
                        -- own colours, as an absolute path is.
                        icon = function() return g.icon_path(app().icon) or app().icon end,
                    },
                    g.box {
                        direction = "column",
                        align = "start",
                        gap = 1,
                        g.text {
                            size = 14,
                            weight = 500,
                            max_width = 380,
                            ellipsize = "end",
                            color = function() return theme.get("text") end,
                            text = function() return app().name end,
                        },
                        g.text {
                            size = 11,
                            max_width = 380,
                            ellipsize = "end",
                            present = function() return (app().generic_name or app().comment or "") ~= "" end,
                            color = function() return theme.get("faint") end,
                            text = function()
                                local a = app()
                                return if a.generic_name ~= "" then a.generic_name else a.comment
                            end,
                        },
                    },
                }
            end,
        },

        g.text {
            size = 11,
            color = function() return theme.get("faint") end,
            text = function() return `{#found.value} of {#apps.list}` end,
        },
    },
}
