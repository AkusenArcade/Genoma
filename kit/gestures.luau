--!strict
-- Gestures ready to use: a node dragged and thrown, a node zoomed with two
-- fingers, a node swiped away. Each gives the values to bind (where the
-- node is, how large) and `attach`, which adds the handlers to a node's
-- table:
--
--   local gestures = require("@genoma/kit/gestures")
--   local card = gestures.draggable { bounds = { 0, 400, 0, 0 }, snap = { { 0, 0 }, { 400, 0 } } }
--   g.capsule(card.attach { offset_x = card.x, offset_y = card.y, width = 120, height = 80 })
--
-- Their motion is the timing set's: `gesture_glide` (thrown, slowing),
-- `gesture_snap` (settling on a point, a spring) and `gesture_out` (swiped
-- away), defined here unless the shell has them.
local g = require("@genoma")

g.timing_default {
    gesture_glide = { duration = 0.5, curve = { 0.25, 0.8, 0.3, 1 } },
    gesture_snap = { spring = { stiffness = 380, damping = 30 } },
    gesture_out = { duration = 0.22, curve = { 0.4, 0, 1, 1 } },
}

local M = {}

-- How far a throw carries: about its speed times this, as a scroller's
-- fling does.
local GLIDE = 0.325

local function read(v: any): any
    if type(v) == "function" then return (v :: any)() end
    return v
end

-- The handlers of `extra` added to `props`; where both have one, both run,
-- the node's own first.
local function merged(props: { [string]: any }, extra: { [string]: any }): { [string]: any }
    local out = table.clone(props)
    for k, f in extra do
        local theirs = out[k]
        if type(theirs) == "function" and type(f) == "function" and string.sub(k, 1, 3) == "on_" then
            out[k] = function(...)
                theirs(...)
                return f(...)
            end
        else
            out[k] = f
        end
    end
    return out
end

export type Draggable = {
    x: () -> number,
    y: () -> number,
    dragging: () -> boolean,
    -- d.move_to(x, y, timing?): there, over a timing entry or at once.
    move_to: (x: number, y: number, timing: string?) -> (),
    attach: (props: { [string]: any }) -> { [string]: any },
}

-- gestures.draggable { x, y, axis, bounds, inertia, snap, threshold,
-- on_move, on_release }: a position the pointer or a finger moves.
-- `axis` "x", "y" or "both"; `bounds` { min_x, max_x, min_y, max_y } (or a
-- function giving it) holds it in; `inertia` (true) carries it on after a
-- throw, slowing; `snap` — a list of { x, y } points, or a function
-- (x, y) -> x, y — is where it settles, the nearest to where a throw would
-- have stopped. `on_release(x, y)` hears where it is going.
function M.draggable(t: { [string]: any }?): Draggable
    local o = t or {}
    local axis = o.axis or "both"
    local px = g.animated(o.x or 0)
    local py = g.animated(o.y or 0)
    local dragging = g.signal(false)
    local from = { 0, 0, 0, 0 }

    local function clamp(x: number, y: number): (number, number)
        local b = read(o.bounds)
        if b then
            x = math.clamp(x, b[1], b[2])
            y = math.clamp(y, b[3], b[4])
        end
        if axis == "x" then y = from[4] elseif axis == "y" then x = from[3] end
        return x, y
    end

    local function settle_at(x: number, y: number): (number, number)
        local snap = o.snap
        if type(snap) == "function" then return snap(x, y) end
        if type(snap) ~= "table" or #snap == 0 then return x, y end
        local best, distance = snap[1], math.huge
        for _, p in snap do
            local d = (p[1] - x) ^ 2 + (p[2] - y) ^ 2
            if d < distance then best, distance = p, d end
        end
        return best[1], best[2]
    end

    local d = {} :: any
    function d.x(): number return px.value :: number end
    function d.y(): number return py.value :: number end
    function d.dragging(): boolean return dragging.value end
    function d.move_to(x: number, y: number, timing: string?)
        px:set(x, timing)
        py:set(y, timing)
    end

    function d.attach(props: { [string]: any }): { [string]: any }
        return merged(props, {
            drag_threshold = props.drag_threshold or o.threshold or 4,
            on_drag_start = function(x: number, y: number)
                -- Caught mid-flight: from where it is now.
                from = { x, y, px.value :: number, py.value :: number }
                px:set(from[3])
                py:set(from[4])
                dragging.value = true
            end,
            on_drag = function(x: number, y: number)
                local nx, ny = clamp(from[3] + x - from[1], from[4] + y - from[2])
                px:set(nx)
                py:set(ny)
                if o.on_move then o.on_move(nx, ny) end
            end,
            on_drag_end = function(vx: number, vy: number)
                dragging.value = false
                local x, y = px.value :: number, py.value :: number
                local carried = o.inertia ~= false
                local tx, ty = clamp(
                    x + (if carried then vx * GLIDE else 0),
                    y + (if carried then vy * GLIDE else 0)
                )
                local sx, sy = settle_at(tx, ty)
                local snapping = sx ~= tx or sy ~= ty
                local timing = if snapping then "gesture_snap" else "gesture_glide"
                px:set(sx, timing)
                py:set(sy, timing)
                if o.on_release then o.on_release(sx, sy) end
            end,
        })
    end
    return d :: Draggable
end

export type Zoomable = {
    scale: () -> number,
    rotation: () -> number,
    pinching: () -> boolean,
    reset: () -> (),
    attach: (props: { [string]: any }) -> { [string]: any },
}

-- gestures.zoomable { scale, rotation, min, max, wheel, on_change }: a
-- scale and a turn two fingers set, on a touchpad or a screen. Past `min`
-- or `max` it follows reluctantly and springs back when let go. `wheel`
-- (false) lets the wheel zoom too, a step at a time.
function M.zoomable(t: { [string]: any }?): Zoomable
    local o = t or {}
    local lo, hi = o.min or 0.5, o.max or 4
    local scale = g.animated(o.scale or 1)
    local turn = g.animated(o.rotation or 0)
    local pinching = g.signal(false)
    local base_scale, base_turn = 1, 0

    -- Past an end the fingers pull it a third as far.
    local function reluctant(s: number): number
        if s > hi then return hi + (s - hi) / 3 end
        if s < lo then return lo - (lo - s) / 3 end
        return s
    end
    local function tell()
        if o.on_change then o.on_change(scale.target :: number, turn.target :: number) end
    end

    local z = {} :: any
    function z.scale(): number return scale.value :: number end
    function z.rotation(): number return turn.value :: number end
    function z.pinching(): boolean return pinching.value end
    function z.reset()
        scale:set(o.scale or 1, "gesture_snap")
        turn:set(o.rotation or 0, "gesture_snap")
        tell()
    end

    function z.attach(props: { [string]: any }): { [string]: any }
        local extra: { [string]: any } = {
            on_pinch = function(s: number, r: number, phase: string)
                if phase == "begin" then
                    base_scale, base_turn = scale.target :: number, turn.target :: number
                    pinching.value = true
                    return
                end
                if phase == "update" then
                    scale:set(reluctant(base_scale * s))
                    turn:set(base_turn + r)
                else
                    pinching.value = false
                    scale:set(math.clamp(scale.target :: number, lo, hi), "gesture_snap")
                end
                tell()
            end,
        }
        if o.wheel then
            extra.on_wheel = function(_: number, dy: number, steps: boolean)
                local by = if steps then 1.15 ^ -dy else math.exp(-dy / 200)
                scale:set(math.clamp((scale.target :: number) * by, lo, hi), if steps then "gesture_snap" else nil)
                tell()
            end
        end
        return merged(props, extra)
    end
    return z :: Zoomable
end

export type Dismissable = {
    offset: () -> number,
    opacity: () -> number,
    gone: () -> boolean,
    -- d.restore(): back, after it went.
    restore: () -> (),
    attach: (props: { [string]: any }) -> { [string]: any },
}

-- gestures.dismissable { axis, size, distance, on_dismiss }: a node swiped
-- or dragged away along `axis` ("x", the default, or "y"). Let go past
-- `distance` (a fraction of `size`, the node's length along the axis, 0.4
-- by default) or thrown that way, it leaves and `on_dismiss(way)` hears
-- "left", "right", "up" or "down"; otherwise it springs back. It fades
-- as it goes.
function M.dismissable(t: { [string]: any }): Dismissable
    local axis = t.axis or "x"
    local offset = g.animated(0)
    local gone = g.signal(false)
    local start = 0

    local function size(): number return read(t.size) or 300 end

    local d = {} :: any
    function d.offset(): number return offset.value :: number end
    function d.opacity(): number
        return math.clamp(1 - math.abs(offset.value :: number) / size(), 0, 1)
    end
    function d.gone(): boolean return gone.value end
    function d.restore()
        gone.value = false
        offset:set(0, "gesture_snap")
    end

    local function leave(sign: number)
        gone.value = true
        offset:set(sign * size() * 1.2, "gesture_out")
        local way = if axis == "x" then (if sign > 0 then "right" else "left") else (if sign > 0 then "down" else "up")
        if t.on_dismiss then
            g.after(0.22, function() t.on_dismiss(way) end)
        end
    end

    function d.attach(props: { [string]: any }): { [string]: any }
        return merged(props, {
            drag_threshold = props.drag_threshold or 6,
            on_drag_start = function(x: number, y: number)
                start = if axis == "x" then x else y
            end,
            on_drag = function(x: number, y: number)
                if gone.value then return end
                offset:set((if axis == "x" then x else y) - start)
            end,
            on_drag_end = function(vx: number, vy: number)
                if gone.value then return end
                local at = offset.value :: number
                local v = if axis == "x" then vx else vy
                -- Thrown: let go at a swipe's speed, the way it was going.
                local fast = math.abs(v) >= 500 and (v > 0) == (at > 0)
                if math.abs(at) >= size() * (t.distance or 0.4) or (fast and math.abs(at) > 24) then
                    leave(if at > 0 then 1 else -1)
                else
                    offset:set(0, "gesture_snap")
                end
            end,
        })
    end
    return d :: Dismissable
end

return M
