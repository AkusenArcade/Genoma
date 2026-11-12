--!strict
-- The kit's look: colours, sizes and times every component reads, in one
-- table a shell changes with theme.set. Components bind to it, so a change
-- reaches what is on screen, the colours moving over `kit_move`.
--
--   local theme = require("@genoma/kit/theme")
--   theme.set { accent = "#e0a35f", radius = 10 }
--   theme.follow(t)  -- or the look of a shell's theme (@genoma/theme)
--
-- The times are timing set entries a shell may define before or after
-- requiring the kit: kit_quick (hover, press), kit_move (colours, a knob
-- travelling), kit_open (a menu, a tooltip appearing).
local g = require("@genoma")

g.timing_default {
    kit_quick = { duration = 0.1, curve = { 0.2, 0, 0, 1 } },
    kit_move = { duration = 0.2, curve = { 0.2, 0, 0, 1 } },
    kit_open = { duration = 0.16, curve = { 0.2, 0, 0, 1 } },
}

export type Tokens = {
    -- What a panel, a menu or a popup is drawn on.
    background: string,
    -- A control at rest on the background: a button, a track, a well.
    control: string,
    -- A control under the pointer, and pressed.
    control_hover: string,
    control_press: string,
    -- Edges: a control's rim, a separator.
    line: string,
    text: string,
    -- Text that says less: a hint, a count, a disabled label.
    faint: string,
    -- What is chosen, on, or the thing to do.
    accent: string,
    -- Text and marks drawn on the accent.
    on_accent: string,
    -- What cannot be undone.
    danger: string,
    -- A row chosen in a list or a menu.
    selection: string,
    -- Logical units.
    text_size: number,
    radius: number,
    control_height: number,
    -- 0 to 1: how much a disabled control is still seen.
    disabled_opacity: number,
}

local defaults: Tokens = {
    background = "#1c1f26f5",
    control = "#2a2f3a",
    control_hover = "#333948",
    control_press = "#262a33",
    line = "#3a404d",
    text = "#e3e7ef",
    faint = "#8b93a5",
    accent = "#7aa2f7",
    on_accent = "#10131a",
    danger = "#e5737f",
    selection = "#7aa2f733",
    text_size = 14,
    radius = 8,
    control_height = 32,
    disabled_opacity = 0.45,
}

local tokens = g.signal(table.clone(defaults))

-- A shell's theme the kit takes its look from (theme.follow), and which of
-- its names gives each token.
local followed = g.signal(nil :: any)

local M = {}

-- What a followed theme is asked for each token, where it does not name a
-- token itself: a colour of its own by these names first, then one worked
-- out from its background, text and accent.
local function lift(t: any, amount: number): string
    return g.colour.lift(t.colour("background"), amount, t.is_dark())
end
local derived: { [string]: (t: any, name: (string) -> any) -> any } = {
    background = function(t) return t.colour("background") end,
    text = function(t) return t.colour("text") end,
    accent = function(t) return t.colour("primary") end,
    control = function(t) return lift(t, 0.07) end,
    control_hover = function(t) return lift(t, 0.11) end,
    control_press = function(t) return lift(t, 0.04) end,
    line = function(t) return lift(t, 0.16) end,
    faint = function(_, name) return g.colour.alpha(name("text"), 0.6) end,
    selection = function(_, name) return g.colour.alpha(name("accent"), 0.2) end,
    -- Whichever of the background and the text reads better on the accent.
    on_accent = function(t, name)
        local accent, bg, text = name("accent"), t.colour("background"), t.colour("text")
        return if g.colour.contrast(bg, accent) >= g.colour.contrast(text, accent) then g.colour.alpha(bg, 1) else text
    end,
}
-- Where a theme's own name for a token is not the kit's.
local aliases: { [string]: { string } } = {
    accent = { "accent", "primary" },
    danger = { "danger", "alert", "error" },
    faint = { "faint", "text_faint", "text_muted" },
    on_accent = { "on_accent", "on_primary" },
    selection = { "selection" },
    line = { "line", "border" },
    background = { "background" },
}

local function from_theme(f: any, name: string): any
    local t = f.theme
    local given = f.map and f.map[name]
    if type(given) == "function" then return given(t) end
    if type(given) == "string" then
        return if (defaults :: any)[name] ~= nil and type((defaults :: any)[name]) == "number" then t.size(given) else t.colour(given)
    end
    local own = (tokens.value :: any)[name]
    if type((defaults :: any)[name]) == "number" then
        return if t.has(name) then t.size(name) else own
    end
    for _, candidate in aliases[name] or { name } do
        if t.has(candidate) then return t.colour(candidate) end
    end
    local d = derived[name]
    if d then return d(t, function(n) return from_theme(f, n) end) end
    return own
end

-- theme.set { … }: the tokens given replace those of the same name.
function M.set(changes: { [string]: any })
    local next: { [string]: any } = table.clone(g.untracked(function() return tokens.value end)) :: any
    for k, v in changes do
        if (defaults :: any)[k] == nil then
            error(`theme.set: no token {k}`, 2)
        end
        next[k] = v
    end
    tokens.value = next
end

-- theme.reset(): back to the kit's own.
function M.reset()
    tokens.value = table.clone(defaults)
end

-- theme.get("accent"): a token, read where it is bound.
function M.get(name: string): any
    local f = followed.value
    if f then return from_theme(f, name) end
    return (tokens.value :: any)[name]
end

-- theme.follow(t): the kit takes its look from a shell's theme
-- (require("@genoma/theme").new { … }), so the kit's controls retint with
-- the rest of the desktop. Each token is the theme's colour or size of the
-- same name, or a near one (accent ← primary, danger ← alert), or worked
-- out from its background, text and accent. `map` names them by hand:
-- { accent = "secondary", radius = "radius_well", control = function(t) … end }.
-- theme.follow(nil): back to the kit's own tokens.
function M.follow(t: any?, map: { [string]: string | (t: any) -> any }?)
    followed.value = if t then { theme = t, map = map } else nil
end

-- theme.alpha("#7aa2f7", 0.2): a colour with that opacity.
function M.alpha(colour: string, a: number): string
    local hex = string.sub(colour, 1, 7)
    return hex .. string.format("%02x", math.clamp(math.floor(a * 255 + 0.5), 0, 255))
end

-- A value given to a component, or the function that gives it.
function M.read(v: any): any
    -- `function` is not callable to the type checker: as any, it is.
    if type(v) == "function" then return (v :: any)() end
    return v
end

return M
