--!strict
-- A tooltip: a few words beside a node, once the pointer has rested on it.
--
--   local tooltip = require("@genoma/kit/tooltip")
--   tooltip { text = "Save the file",
--       button { icon = "document-save", on_click = save },
--   }
--
-- Wraps the one node it is given and returns it, listening to its hover
-- (an on_hover of its own is still called) and with the tooltip written
-- inside it. text may be a function. side and align place it, below and
-- centred by default; it takes neither the pointer nor the keyboard, and a
-- press on the node hides it.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

-- How long the pointer rests before it shows: long enough that passing
-- over a row of buttons does not light them all.
local REST = 0.6

export type Tooltip = {
    text: string | () -> string,
    side: string?,
    align: string?,
    [any]: any,
}

return function(t: Tooltip): any
    local node = t[1]
    assert(type(node) == "table", "tooltip wraps one node")
    local shown = g.signal(false)
    local waiting: any = nil
    local function hide()
        if waiting then waiting:stop(); waiting = nil end
        shown.value = false
    end

    local their_hover = node.on_hover
    node.on_hover = function(entered: boolean)
        if their_hover then their_hover(entered) end
        if entered then
            if waiting then waiting:stop() end
            waiting = g.after(REST, function()
                waiting = nil
                shown.value = true
            end)
        else
            hide()
        end
    end
    local their_press = node.on_press
    node.on_press = function(...)
        hide()
        if their_press then their_press(...) end
    end

    table.insert(node, g.popup {
        side = t.side or "bottom",
        align = t.align or "center",
        gap = 6,
        grab = false,
        present = function() return shown.value end,
        g.capsule {
            padding_x = 8,
            padding_y = 5,
            radius = function() return theme.get("radius") - 2 end,
            fill = function() return theme.get("background") end,
            rim_top = function() return theme.get("line") end,
            rim_bottom = function() return theme.get("line") end,
            g.text {
                size = function() return theme.get("text_size") - 2 end,
                color = function() return theme.get("text") end,
                text = t.text,
            },
        },
    })
    return node
end
