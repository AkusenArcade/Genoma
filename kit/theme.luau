--!strict
-- The kit's look: colours, sizes and times every component reads, in one
-- table a shell changes with theme.set. Components bind to it, so a change
-- reaches what is on screen, the colours moving over `kit_move`.
--
--   local theme = require("@genoma/kit/theme")
--   theme.set { accent = "#e0a35f", radius = 10 }
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

local M = {}

-- theme.set { … }: the tokens given replace those of the same name.
function M.set(changes: { [string]: any })
    local next = table.clone(g.untracked(function() return tokens.value end))
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
    return (tokens.value :: any)[name]
end

-- theme.alpha("#7aa2f7", 0.2): a colour with that opacity.
function M.alpha(colour: string, a: number): string
    local hex = string.sub(colour, 1, 7)
    return hex .. string.format("%02x", math.clamp(math.floor(a * 255 + 0.5), 0, 255))
end

-- A value given to a component, or the function that gives it.
function M.read(v: any): any
    if type(v) == "function" then return v() end
    return v
end

return M
