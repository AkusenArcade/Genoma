--!strict
-- A title bar for a g.window: its title, and a button that closes it.
-- Pressed anywhere but on a button, it moves the window; pressed twice, it
-- maximizes it or gives back its size (`move_window`).
--
--   local titlebar = require("@genoma/kit/titlebar")
--   g.window {
--       title = "Settings",
--       on_close = function() open.value = false end,
--       g.box { direction = "column", grow = 1,
--           titlebar { title = "Settings", on_close = function() open.value = false end },
--           …
--       },
--   }
--
-- A compositor that decorates windows draws its own bar as well: niri
-- does not, so a window there has only this one. Children given are drawn
-- between the title and the close button (a menu, a search field).
local g = require("@genoma")
local theme = require("@genoma/kit/theme")
local button = require("@genoma/kit/button")

local read = theme.read

export type Titlebar = {
    title: (string | () -> string)?,
    -- Called when the close button is pressed; no button without it.
    on_close: (() -> ())?,
    height: number?,
    [any]: any,
}

return function(t: Titlebar): any
    local height = t.height or 40
    local node: { [any]: any } = {
        height = height,
        direction = "row",
        align = "center",
        gap = 8,
        padding_left = 14,
        padding_right = 6,
        move_window = true,
        g.text {
            grow = 1,
            text = function() return read(t.title) or "" end,
            size = function() return theme.get("text_size") end,
            weight = 600,
            color = function() return theme.get("text") end,
        },
    }
    local taken = { title = true, on_close = true, height = true }
    for k, v in t do
        if type(k) == "string" and not taken[k] then node[k] = v end
    end
    for _, child in ipairs(t) do table.insert(node, child) end
    if t.on_close then
        local on_close = t.on_close
        local side = height - 12
        table.insert(node, button {
            kind = "flat",
            name = "Close",
            width = side,
            height = side,
            padding_x = 0,
            on_click = on_close,
            g.path {
                width = 10,
                height = 10,
                d = "M 1 1 L 9 9 M 9 1 L 1 9",
                stroke = 1.6,
                cap = "round",
                color = function() return theme.get("text") end,
            },
        })
    end
    return g.box(node)
end
