--!strict
-- Charts drawn on the GPU in one rectangle, no node per point: a line, an
-- area under it, or bars, from a list of numbers that may change as often
-- as it likes (a binding).
--
--   local chart = require("@genoma/kit/chart")
--   chart.line { width = 160, height = 32, values = function() return cpu_history.value end, max = 100 }
--   chart.area { width = 160, height = 48, values = history, color = "#7aa2f7", fade = 0.8 }
--   chart.bars { width = 120, height = 40, values = { 3, 5, 2, 8 }, gap = 0.25, radius = 2 }
--
-- values: the numbers, oldest first; the last 1008 are drawn. min, max:
-- the range shown (by default the values' own). color: the line or the
-- bars ("#rrggbb" or "#rrggbbaa", the theme's accent by default); line:
-- its width, logical units. fill (area: the colour under the line, the
-- line's colour at a quarter by default; line: nothing by default) and
-- fade (0…1, how much the fill fades toward the bottom). gap (bars, 0…1
-- of a bar's slot) and radius (bars' corners). inset: room kept above and
-- below, half the line by default. Anything a node takes (width, height,
-- grow, opacity, present, id…) is given as usual; any parameter may be a
-- function.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local LIMIT = 1008

local function read(v: any): any
    if type(v) == "function" then return (v :: any)() end
    return v
end

local function rgba(colour: string?): (number, number, number, number)
    local hex = string.gsub(colour or "#000000", "#", "")
    local function at(i: number): number
        return (tonumber(string.sub(hex, i, i + 1), 16) or 0) / 255
    end
    local a = if #hex >= 8 then at(7) else 1
    return at(1), at(3), at(5), a
end

local NODE = {
    "id", "width", "height", "grow", "overlay", "attach", "offset_x", "offset_y", "opacity", "present",
    "follow", "min_width", "max_width", "radius",
}

local function chart(t: { [any]: any }, kind: string): any
    local node: { [any]: any } = { src = "genoma:chart" }
    for _, k in NODE do
        if t[k] ~= nil and not (k == "radius" and kind == "bars") then node[k] = t[k] end
    end
    node.values = function()
        local list: { number } = read(t.values) or {}
        local n = #list
        local first = math.max(1, n - LIMIT + 1)
        local lo, hi = read(t.min), read(t.max)
        if lo == nil or hi == nil then
            local a, b = math.huge, -math.huge
            for i = first, n do
                a = math.min(a, list[i])
                b = math.max(b, list[i])
            end
            if n == 0 then a, b = 0, 1 end
            if kind == "bars" then a = math.min(a, 0) end
            if b <= a then a, b = a - 1, b + 1 end
            lo = if lo == nil then a else lo
            hi = if hi == nil then b else hi
        end
        local colour = read(t.color) or theme.get("accent")
        local r, gr, b, a = rgba(colour)
        local fr, fg, fb, fa = 0, 0, 0, 0
        local fill = read(t.fill)
        if fill then
            fr, fg, fb, fa = rgba(fill)
        elseif kind == "area" then
            fr, fg, fb, fa = r, gr, b, a * 0.25
        end
        local line = read(t.line) or 1.5
        local extra = if kind == "bars" then (read(t.radius) or 0) else (read(t.fade) or 0)
        local out = {
            r, gr, b, a, fr, fg, fb, fa,
            if kind == "bars" then 1 else 0, n - first + 1, lo, hi,
            line, read(t.gap) or 0.2, read(t.inset) or (if kind == "bars" then 0 else line * 0.5), extra,
        }
        for i = first, n do
            table.insert(out, list[i])
        end
        return out
    end
    return g.shader(node)
end

local M = {}

function M.line(t: { [any]: any }): any
    return chart(t, "line")
end

function M.area(t: { [any]: any }): any
    return chart(t, "area")
end

function M.bars(t: { [any]: any }): any
    return chart(t, "bars")
end

return M
