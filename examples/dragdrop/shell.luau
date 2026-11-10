--!strict
-- Drag and drop with other programs, and within the shell. The upper card
-- gives this very file when dragged (into a file manager, an editor, the
-- lower card); the lower one takes what is dropped on it, files or text,
-- and lights up while something is over it.
--
--   genoma examples/dragdrop
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme
local got = g.signal("Nothing dropped yet.")
local over = g.signal(false)

-- A card of that height and fill, with what it holds and what else it is
-- given (a drag's files, a drop's handlers).
local function card(height: number, fill: any, child: any, more: { [string]: any }): any
    local node: { [any]: any } = { height = height, radius = 10, justify = "center", fill = fill, child }
    for k, v in more do node[k] = v end
    return g.capsule(node)
end

local function say(text: string): any
    return g.text { text = text, size = function() return theme.get("text_size") end, color = function() return theme.get("text") end }
end

return {
    g.window {
        title = "Drag and drop",
        app_id = "genoma.example.dragdrop",
        width = 380,
        height = 360,
        g.capsule {
            grow = 1,
            direction = "column",
            align = "stretch",
            gap = 16,
            padding = 20,
            radius = 0,
            fill = function() return theme.get("background") end,
            card(80, function() return theme.get("control") end, say("Drag this file somewhere"), {
                drag_files = g.dir .. "/shell.luau",
                cursor = "grab",
            }),
            card(120, function() return if over.value then theme.alpha(theme.get("accent"), 0.5) else theme.get("control") end, say("Drop here"), {
                on_drop_hover = function(on: boolean) over.value = on end,
                on_drop = function(d: any)
                    if #d.files > 0 then
                        got.value = `{#d.files} file(s): {table.concat(d.files, ", ")}`
                    else
                        got.value = `Text: {d.text or ""}`
                    end
                end,
            }),
            g.text {
                text = function() return got.value end,
                size = 12,
                color = function() return theme.get("faint") end,
                max_width = 340,
            },
        },
    },
}
