--!strict
-- The showcase's look: its own theme (a `@genoma/theme`), its type, and the
-- stage's scale. Drawn for a 1920 × 1080 screen; any other size scales the
-- whole stage to fit, letterboxed, so a recording is the same picture.
local g = require("@genoma")
local themes = require("@genoma/theme")

local M = {}

M.W, M.H = 1920, 1080

-- The stage's scale against the 1920 × 1080 it is drawn for.
M.k = g.signal(1)
function M.u(n: number): number return n * M.k.value end

-- The logo's colours: a pale green that ripens, on a near-black with a green
-- cast. A second accent, a cold teal, for what the first one is not.
M.t = themes.new {
    roles = {
        background = "#0B0F0A",
        text = "#F1F6EC",
        accent = "#B9DE6B",
        accent2 = "#5ED3C1",
    },
    derive = {
        muted = function(c) return g.colour.alpha(c.text, 0.66) end,
        faint = function(c) return g.colour.alpha(c.text, 0.40) end,
        line = function(c) return g.colour.alpha(c.text, 0.12) end,
        panel = function(c) return g.colour.alpha(g.colour.lift(c.background, 0.05, c.dark), 0.72) end,
        glow = function(c) return g.colour.lighten(c.accent, 0.22) end,
        deep = function(c) return g.colour.lighten(c.accent, -0.18) end,
    },
    timing = {
        -- A slide arriving: long, decelerating to rest; one leaving: shorter.
        slide_in = { duration = 0.9, curve = { 0.16, 1, 0.3, 1 } },
        slide_out = { duration = 0.45, curve = { 0.45, 0, 0.55, 1 } },
        -- The logo drawing itself, and what follows it in.
        draw = { duration = 1.6, curve = { 0.65, 0, 0.35, 1 } },
        draw_strand = { duration = 1.3, curve = { 0.65, 0, 0.35, 1 }, delay = 0.45 },
        draw_strand_late = { duration = 0.9, curve = { 0.65, 0, 0.35, 1 }, delay = 0.95 },
        rise = { duration = 0.8, curve = { 0.16, 1, 0.3, 1 } },
        fade = { duration = 0.45, curve = { 0.4, 0, 0.2, 1 } },
        panel = { spring = { stiffness = 210, damping = 24 } },
        step = { duration = 0.35, curve = { 0.2, 0.9, 0.3, 1 } },
        -- The demos: a shape changing its mind, a soft spring, a flash.
        morph = { duration = 0.9, curve = { 0.65, 0, 0.35, 1 } },
        soft = { spring = { stiffness = 120, damping = 13 } },
        firm = { spring = { stiffness = 260, damping = 22 } },
        flash = { duration = 0.12, curve = { 0.2, 0, 0.4, 1 } },
        dim = { duration = 0.9, curve = { 0.4, 0, 0.2, 1 } },
        -- Constant motion: a turn that never eases, one step a period.
        linear_2 = { duration = 2, curve = { 0, 0, 1, 1 } },
        linear_3 = { duration = 3, curve = { 0, 0, 1, 1 } },
        instant = { duration = 0 },
    },
}
local t = M.t

-- The kit's controls wear this theme too.
require("@genoma/kit").theme.follow(t)

-- A value told along a line, each that hears it a little after the one
-- before: `follow_1` … `follow_6`.
do
    local follow = {}
    for i = 1, 6 do
        follow["follow_" .. i] = { duration = 0.7, curve = { 0.16, 1, 0.3, 1 }, delay = (i - 1) * 0.09 }
        follow["spark_" .. i] = { duration = 0.12, curve = { 0.2, 0, 0.4, 1 }, delay = (i - 1) * 0.09 }
    end
    g.timing(follow)
end

-- What comes in after a slide arrives comes in a cascade: `rise_1` …
-- `rise_12`, each a little after the one before.
do
    local cascade = {}
    for i = 1, 12 do
        cascade["rise_" .. i] = { duration = 0.8, curve = { 0.16, 1, 0.3, 1 }, delay = 0.15 + (i - 1) * 0.08 }
    end
    g.timing(cascade)
end

M.CLEAR = "#00000000"

-- While true the deck's own furniture (the progress line) steps aside: a
-- slide that wants the whole screen, as the last one's end does.
M.quiet = g.signal(false)

-- A capsule placed by its corner in a demo's own units, rims off unless
-- given: the demos draw their own light.
function M.cap(props: { [any]: any }): any
    local out = table.clone(props)
    if out.overlay == nil then out.overlay = true end
    if out.attach == nil and out.overlay then out.attach = "corner" end
    for _, k in { "x", "y", "w", "h", "r" } do out[k] = nil end
    local function scaled(v: any): any
        if type(v) == "function" then return function() return M.u(v()) end end
        return function() return M.u(v) end
    end
    if props.x ~= nil then out.offset_x = scaled(props.x) end
    if props.y ~= nil then out.offset_y = scaled(props.y) end
    if props.w ~= nil then out.width = scaled(props.w) end
    if props.h ~= nil then out.height = scaled(props.h) end
    if props.r ~= nil then out.radius = scaled(props.r) end
    if out.rim_top == nil then out.rim_top = M.CLEAR end
    if out.rim_bottom == nil then out.rim_bottom = M.CLEAR end
    return g.capsule(out)
end

-- A step every `period` seconds once `arrived()`, counting from 1 round to
-- `count`: what a demo cycles through. Made in a slide, it goes with it.
function M.cycle(arrived: () -> boolean, period: number, count: number): any
    local step = g.signal(1)
    g.timer { every = period, on_tick = function()
        if g.untracked(arrived) then step.value = step.value % count + 1 end
    end }
    return step
end

function M.c(name: string): () -> string
    return function(): string return t.colour(name) end
end

-- Three voices: Orbitron for what is technical (labels, numbers, the
-- name), Spectral for what is said, a monospace for code.
M.display = "Orbitron"
M.body = "Spectral"
M.mono = "DejaVu Sans Mono"

-- A text in one of the type scale's styles, at the stage's scale.
local STYLES = {
    hero = { font = "display", size = 120, weight = 500, tracking = 0.08 },
    title = { font = "body", size = 64, weight = 500, tracking = -0.01 },
    lead = { font = "body", size = 30, weight = 400 },
    body = { font = "body", size = 24, weight = 400 },
    number = { font = "display", size = 52, weight = 500, tracking = 0.04 },
    kicker = { font = "display", size = 15, weight = 600, tracking = 0.32 },
    label = { font = "display", size = 13, weight = 500, tracking = 0.18 },
    code = { font = "mono", size = 20, weight = 400 },
}
local FONTS = { display = M.display, body = M.body, mono = M.mono }

function M.text(style: string, props: { [string]: any }): any
    local s = STYLES[style]
    local out = table.clone(props)
    out.font = FONTS[s.font]
    out.size = function() return math.floor(s.size * M.k.value + 0.5) end
    out.weight = out.weight or s.weight
    if s.tracking and out.tracking == nil then out.tracking = s.tracking end
    if out.color == nil then out.color = M.c("text") end
    return g.text(out)
end

-- The fill of everything in the accent: the logo's own gradient.
function M.accent_stops(): { any }
    return { { 0, t.colour("glow") }, { 1, t.colour("deep") } }
end

return M
