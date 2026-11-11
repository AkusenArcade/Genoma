--!strict
-- The shader effects Genoma ships, as nodes with their parameters by name:
--
--   local effects = require("@genoma/kit/effects")
--   effects.noise { width = 300, height = 200, from = "#1b2a49", to = "#7aa2f7", drift = 0.15 }
--
-- Each is a g.shader on a built-in source (`src = "genoma:noise"`), so
-- what any node takes (width, height, grow, overlay, attach, offset_x,
-- offset_y, opacity, present, id, radius…) is given as usual. Any
-- parameter may be a function: a binding. Colours are "#rrggbb" or
-- "#rrggbbaa". Something that moves (drift, speed, rate above 0) runs the
-- node's clock.
--
--   noise     from, to, size (of a feature, logical units), drift (features
--             a second), octaves, contrast
--   ripple    color, centre ({ x, y }, 0…1), spacing (between rings), speed
--             (rings a second), reach (where they fade out), ring (a ring's
--             width, 0…1 of the spacing)
--   dissolve  color, progress (0 whole … 1 gone), edge (0…1), edge_color,
--             grain (logical units)
--   vignette  color, strength (0…1), start (0 the centre … 1 a corner),
--             softness (0…1)
--   grain     amount (0…1), size (of a speck), rate (changes a second)
--   glass     radius (corners), bend (how far the rim pulls what is under),
--             aberration (the colours parting at the rim), frost, tint,
--             rim (its top edge's light, 0…1); reads what is under it
---- Passes: post-processing for the node they are put in (`pass = true`,
-- set for you). The node and its other children are drawn into a picture,
-- and each pass in turn draws it again over the node's box:
--
--   pixelate  size (a cell's side)
--   chromatic offset (how far red and blue part at a corner)
--   wave      amplitude, wavelength, speed (waves a second)
--   glow      radius (how far light spreads), strength, threshold (0…1)
--

-- Each source is also there for a g.shader of one's own to start from; and
-- every g.shader has the g_ library (noise, distance fields, colour) the
-- README lists.
local g = require("@genoma")

local function read(v: any): any
    -- `function` is not callable to the type checker: as any, it is.
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

-- What a node takes, apart from an effect's own parameters.
local NODE = {
    "id", "width", "height", "grow", "overlay", "attach", "offset_x", "offset_y", "opacity", "present",
    "follow", "pass", "min_width", "max_width", "rotate_x", "rotate_y", "depth", "perspective", "pivot_x", "pivot_y",
}

local function effect(name: string, t: { [any]: any }, moving: { string }, values: () -> { number }): any
    local node: { [any]: any } = { src = "genoma:" .. name, values = values }
    for _, k in NODE do
        if t[k] ~= nil then node[k] = t[k] end
    end
    -- The clock runs when something can move.
    local clock = false
    for _, k in moving do
        local v = t[k]
        if type(v) == "function" or (type(v) == "number" and v > 0) then clock = true end
    end
    node.clock = clock
    return g.shader(node)
end

local M = {}

function M.noise(t: { [any]: any }): any
    return effect("noise", t, { "drift" }, function()
        local r1, g1, b1, a1 = rgba(read(t.from) or "#000000")
        local r2, g2, b2, a2 = rgba(read(t.to) or "#ffffff")
        return { r1, g1, b1, a1, r2, g2, b2, a2, read(t.size) or 48, read(t.drift) or 0, read(t.octaves) or 4, read(t.contrast) or 1 }
    end)
end

function M.ripple(t: { [any]: any }): any
    return effect("ripple", t, { "speed" }, function()
        local r, gr, b, a = rgba(read(t.color) or "#ffffff")
        local centre = read(t.centre) or { 0.5, 0.5 }
        return { r, gr, b, a, centre[1], centre[2], read(t.spacing) or 24, read(t.speed) or 0.5, read(t.reach) or 200, read(t.ring) or 0.3 }
    end)
end

function M.dissolve(t: { [any]: any }): any
    return effect("dissolve", t, {}, function()
        local r, gr, b, a = rgba(read(t.color) or "#ffffff")
        local er, eg, eb, ea = rgba(read(t.edge_color) or "#ffb35a")
        return { r, gr, b, a, read(t.progress) or 0, read(t.edge) or 0.08, er, eg, eb, ea, read(t.grain) or 24 }
    end)
end

function M.vignette(t: { [any]: any }): any
    return effect("vignette", t, {}, function()
        local r, gr, b, a = rgba(read(t.color) or "#000000")
        return { r, gr, b, a, read(t.strength) or 0.5, read(t.start) or 0.4, read(t.softness) or 0.6 }
    end)
end

-- What is under the pane, bent at its rim, the colours parting there.
function M.glass(t: { [any]: any }): any
    local node = effect("glass", t, {}, function()
        local r, gr, b, a = rgba(read(t.tint) or "#ffffff10")
        return {
            read(t.radius) or 16, read(t.bend) or 10, read(t.aberration) or 1.5, read(t.frost) or 1.5,
            r, gr, b, a, read(t.rim) or 0.25,
        }
    end)
    node.backdrop = true
    return node
end

function M.grain(t: { [any]: any }): any
    return effect("grain", t, { "rate" }, function()
        return { read(t.amount) or 0.06, read(t.size) or 1, read(t.rate) or 0 }
    end)
end

-- A pass: the effect put on the node it is in rather than drawn over it.
local function pass(name: string, t: { [any]: any }, moving: { string }, values: () -> { number }): any
    local copy = table.clone(t)
    copy.pass = true
    return effect(name, copy, moving, values)
end

function M.pixelate(t: { [any]: any }): any
    return pass("pixelate", t, {}, function()
        return { read(t.size) or 6 }
    end)
end

function M.chromatic(t: { [any]: any }): any
    return pass("chromatic", t, {}, function()
        return { read(t.offset) or 3 }
    end)
end

function M.wave(t: { [any]: any }): any
    return pass("wave", t, { "speed" }, function()
        return { read(t.amplitude) or 4, read(t.wavelength) or 60, read(t.speed) or 0 }
    end)
end

function M.glow(t: { [any]: any }): any
    return pass("glow", t, {}, function()
        return { read(t.radius) or 12, read(t.strength) or 1, read(t.threshold) or 0.5 }
    end)
end

return M
