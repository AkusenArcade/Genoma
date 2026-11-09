--!strict
-- How far something has got: a track and the part done.
--
--   local progress = require("@genoma/kit/progress")
--   progress { width = 240, value = function() return download.done end }
--
-- value: 0 to 1, a value or a function; the bar moves to it over kit_move.
-- There is no endless kind: a bar that moves with nothing behind it says
-- nothing, so a task without a measure shows a label instead.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local read = theme.read

export type Progress = {
    value: (number | () -> number)?,
    width: (number | () -> number)?,
    height: number?,
    color: (string | () -> string)?,
    name: string?,
    [any]: any,
}

return function(t: Progress): any
    local height = t.height or 6
    local function width(): number return read(t.width) or 200 end
    local function value(): number return math.clamp(read(t.value) or 0, 0, 1) end
    local node: { [any]: any } = {
        role = "status",
        name = t.name or "Progress",
        width = width,
        height = height,
        radius = height / 2,
        fill = function() return theme.get("control") end,
        rim_top = "#00000000",
        rim_bottom = "#00000000",
        justify = "start",
        g.capsule {
            overlay = true,
            attach = "corner",
            width = function() return width() * value() end,
            height = height,
            radius = height / 2,
            fill = function() return read(t.color) or theme.get("accent") end,
            rim_top = "#00000000",
            rim_bottom = "#00000000",
            transition = { width = "kit_move", fill = "kit_move" },
        },
    }
    for _, k in { "id", "present", "grow" } do
        if t[k] ~= nil then node[k] = t[k] end
    end
    return g.capsule(node)
end
