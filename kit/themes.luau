--!strict
-- A shell's own theme: its colours, sizes, type and times as tokens every
-- node binds to and none copies, so one change reaches the whole desktop.
--
--   local theme = require("@genoma/theme")
--   local t = theme.new {
--       roles = { background = "#1c1f26", text = "#e3e7ef", primary = "#7aa2f7" },
--       derive = {
--           surface = function(c) return g.colour.lift(c.background, 0.045, c.dark) end,
--       },
--       sizes = { radius = 12, row = 30 },
--       fonts = { body = "Inter" },
--       type = { label = { font = "body", size = 15, weight = 500 } },
--       timing = { open = { duration = 0.25, curve = { 0.2, 0.9, 0.3, 1.15 } } },
--   }
--   g.capsule { fill = function() return t.colour("surface") end, radius = function() return t.size("radius") end }
--   t.set { primary = "#e0a35f" }
--
-- Roles are the colours a palette gives; everything else is derived from
-- them, once per change rather than once per binding that asks. A change of
-- roles is a crossing (g.crossing): every binding runs once, and every
-- colour it sets moves from the old to the new over the `crossing` timing,
-- so the desktop crosses together rather than in patches.
--
-- Nothing here is Bioma's: Bioma's own theme is one of these, with its
-- values.
local g = require("@genoma")

-- A timing entry, and whether the theme's speed and instant leave it alone:
-- a wait that is not an animation (a debounce, a dwell) is `fixed`.
export type Entry = {
    duration: number?,
    curve: { number }?,
    delay: number?,
    spring: { stiffness: number?, damping: number?, mass: number? }?,
    fixed: boolean?,
}

-- One voice of the type scale: a font named in `fonts`, a size the density
-- step scales (to whole units), weight, tracking in em, upper case.
export type Style = {
    font: string?,
    size: number,
    weight: number?,
    tracking: number?,
    upper: boolean?,
}

-- What derive's functions read: c.primary is that colour now, c.dark
-- whether the theme is dark.
export type Colours = { [string]: any }

export type Spec = {
    -- The palette's colours, "#rrggbb[aa]": what t.set and t.source change.
    roles: { [string]: string },
    -- Colours no palette changes: a string, or { dark = …, light = … }
    -- following the theme's side (state colours, say).
    fixed: { [string]: string | { dark: string, light: string } }?,
    -- Colours (or numbers) worked out from the others.
    derive: { [string]: (c: Colours) -> any }?,
    -- Whether the theme is dark. Without it: the `background` role's luma
    -- below 0.5, or dark when there is no background role.
    dark: ((c: Colours) -> boolean)?,
    -- Logical units. A function is read where it is bound (a setting).
    sizes: { [string]: number | () -> number }?,
    -- Sizes the density step leaves alone: hairlines, a rim.
    unscaled: { string }?,
    -- Density steps by name, each a factor on the sizes and the type.
    steps: { [string]: number }?,
    step: string?,
    -- Families by the name the type scale uses.
    fonts: { [string]: string | () -> string }?,
    type: { [string]: Style }?,
    -- Timing entries, given to g.timing after speed and instant.
    timing: { [string]: Entry }?,
    speed: number?,
    -- The timing a change of roles crosses over. Defined, if neither the
    -- theme's timing nor the shell's has it, as 0.25 s ease in and out.
    crossing: string?,
}

export type Theme = {
    -- t.colour("surface"): a role, a fixed or a derived colour.
    colour: (name: string) -> any,
    -- t.target("primary"): where a role is going, before the crossing has
    -- begun (a palette's chip arrives at once).
    target: (role: string) -> string,
    -- t.has("surface"): whether the theme names it, as a colour or a size.
    has: (name: string) -> boolean,
    -- t.roles(): the roles' names, in order.
    roles: () -> { string },
    -- t.set { primary = "#…" }: those roles change, crossing.
    set: (changes: { [string]: string }) -> (),
    -- While true, t.set lands at once: a palette being edited follows the
    -- hand rather than a crossing behind it.
    immediate: { value: boolean },
    -- t.source { path, watch, map }: the roles from a JSON file, followed.
    -- `map(data)` turns another shape into { role = "#…" }; without it the
    -- file's keys that name roles are taken. `path` may be a function;
    -- t.source(nil) stops following.
    source: (spec: { path: string | () -> string, watch: boolean?, map: ((data: any) -> { [string]: string })? }?) -> (),
    is_dark: () -> boolean,
    -- t.size("radius"): logical units at the current step.
    size: (name: string) -> number,
    -- t.step(): the step's name; t.step("compact"): to that step.
    step: (name: string?) -> string,
    factor: () -> number,
    -- t.font("label"): { font, size, weight, tracking, upper } at the step.
    font: (style: string) -> { [string]: any },
    -- t.text("label", { text = …, color = … }): a g.text's table with the
    -- style's font, size, weight and tracking, the text upper-cased when
    -- the style says so.
    text: (style: string, props: { [string]: any }) -> { [string]: any },
    -- t.speed(): the factor durations are divided by; t.speed(2): twice as
    -- fast.
    speed: (value: number?) -> number,
    -- t.instant(true): everything not `fixed` is over in a millisecond.
    instant: (on: boolean?) -> boolean,
    -- t.seconds("open"): an entry's duration after speed and instant, for
    -- a timer.
    seconds: (name: string) -> number,
    -- The timing a change of roles crosses over.
    crossing: string,
}

local M = {}

local IN_OUT_QUAD = { 0.455, 0.03, 0.515, 0.955 }

local function read(v: any): any
    if type(v) == "function" then return (v :: any)() end
    return v
end

function M.new(spec: Spec): Theme
    assert(type(spec) == "table" and type(spec.roles) == "table", "theme.new: { roles = { … } }")
    return g.lasting(function()
        local t = {} :: any

        -- ---- Colours -------------------------------------------------------

        local role_names: { string } = {}
        local roles: { [string]: any } = {}
        for name, value in spec.roles do
            assert(g.colour.parse(value), `theme.new: role {name} is not a colour`)
            table.insert(role_names, name)
            roles[name] = g.signal(g.colour.format(g.colour.parse(value)))
        end
        table.sort(role_names)
        local targets = g.signal((function()
            local out = {}
            for name, s in roles do out[name] = s.value end
            return out
        end)())
        local fixed = spec.fixed or {}
        local derived: { [string]: any } = {}

        local colours: Colours = setmetatable({}, {
            __index = function(_, name: string): any
                if name == "dark" then return t.is_dark() end
                return t.colour(name)
            end,
        }) :: any

        local dark = g.computed(function(): boolean
            if spec.dark then return spec.dark(colours) end
            local bg = roles.background
            return if bg then g.colour.luma(bg.value) < 0.5 else true
        end)
        function t.is_dark(): boolean return dark.value end

        for name, f in spec.derive or {} do
            assert(roles[name] == nil and fixed[name] == nil, `theme.new: {name} is both given and derived`)
            derived[name] = g.computed(function() return f(colours) end)
        end

        function t.colour(name: string): any
            local r = roles[name]
            if r then return r.value end
            local d = derived[name]
            if d then return d.value end
            local f = fixed[name]
            if f ~= nil then
                if type(f) == "table" then return if dark.value then f.dark else f.light end
                return f
            end
            error(`theme: no colour {name}`, 2)
        end

        function t.target(role: string): string
            local v = targets.value[role]
            if v == nil then error(`theme: no role {role}`, 2) end
            return v
        end

        function t.roles(): { string } return table.clone(role_names) end

        local sizes = spec.sizes or {}
        function t.has(name: string): boolean
            return roles[name] ~= nil or derived[name] ~= nil or fixed[name] ~= nil or sizes[name] ~= nil
        end

        t.immediate = g.signal(false)

        -- Every role at once, then the colours cross in the scene: a turn
        -- later, so a set made inside an effect is not inside its wave.
        local function apply(changes: { [string]: string })
            local clean: { [string]: string } = {}
            for name, value in changes do
                if roles[name] and type(value) == "string" and g.colour.parse(value) then
                    clean[name] = g.colour.format(g.colour.parse(value))
                end
            end
            if next(clean) == nil then return end
            g.untracked(function()
                local next_targets = table.clone(targets.value)
                for name, value in clean do next_targets[name] = value end
                targets.value = next_targets
            end)
            local function put()
                for name, value in clean do roles[name].value = value end
            end
            if g.untracked(function() return t.immediate.value end) then
                g.batch(put)
            else
                g.after(0, function() g.crossing(t.crossing, put) end)
            end
        end

        function t.set(changes: { [string]: string })
            for name in changes do
                if roles[name] == nil then error(`theme.set: no role {name}`, 2) end
            end
            apply(changes)
        end

        local followed = g.signal(nil :: any)
        function t.source(s: any)
            followed.value = s
        end
        -- The file the source names, watched; a new source replaces the
        -- watch, as what an effect makes its next run disposes.
        g.effect(function()
            local s = followed.value
            if s == nil then return end
            local file = g.file { path = read(s.path), watch = s.watch ~= false }
            g.effect(function()
                local text = file.text
                if not text then return end
                local ok, data = pcall(g.parse_json, text)
                if not ok or type(data) ~= "table" then return end
                if s.map then data = s.map(data) end
                g.untracked(function() apply(data) end)
            end)
        end)

        -- ---- Sizes and type -------------------------------------------------

        local steps = spec.steps or { normal = 1 }
        local step = g.signal(spec.step or (if steps.normal then "normal" else next(steps)) or "normal")
        local unscaled: { [string]: boolean } = {}
        for _, name in spec.unscaled or {} do unscaled[name] = true end

        function t.step(name: string?): string
            if name ~= nil then
                if steps[name] == nil then error(`theme.step: no step {name}`, 2) end
                step.value = name
                return name
            end
            return step.value
        end
        function t.factor(): number return steps[step.value] or 1 end

        function t.size(name: string): number
            local v = sizes[name]
            if v == nil then error(`theme: no size {name}`, 2) end
            local n = read(v) :: number
            return if unscaled[name] then n else n * t.factor()
        end

        local fonts = spec.fonts or {}
        local styles = spec.type or {}
        function t.font(name: string): { [string]: any }
            local s = styles[name]
            if s == nil then error(`theme: no type style {name}`, 2) end
            local family = if s.font then read(fonts[s.font]) or s.font else nil
            return {
                font = family,
                size = math.floor(s.size * t.factor() + 0.5),
                weight = s.weight or 400,
                tracking = s.tracking or 0,
                upper = s.upper == true,
            }
        end

        function t.text(name: string, props: { [string]: any }): { [string]: any }
            local s = styles[name]
            if s == nil then error(`theme: no type style {name}`, 2) end
            local out = table.clone(props)
            if s.font then out.font = function() return t.font(name).font end end
            out.size = function() return t.font(name).size end
            out.weight = s.weight or 400
            if s.tracking then out.tracking = s.tracking end
            if s.upper and props.text ~= nil then
                local text = props.text
                out.text = function() return string.upper(tostring(read(text))) end
            end
            return out
        end

        -- ---- Times ------------------------------------------------------------

        local entries = spec.timing or {}
        local speed = g.signal(spec.speed or 1)
        local instant = g.signal(false)
        function t.speed(value: number?): number
            if value ~= nil then speed.value = math.max(0.01, value) end
            return speed.value
        end
        function t.instant(on: boolean?): boolean
            if on ~= nil then instant.value = on end
            return instant.value
        end
        function t.seconds(name: string): number
            local e = entries[name]
            if e == nil then error(`theme: no timing {name}`, 2) end
            if e.fixed then return e.duration or 0 end
            if instant.value then return 0.001 end
            return (e.duration or 0) / speed.value
        end

        g.effect(function()
            local k, quick = speed.value, instant.value
            local set = {}
            for name, e in entries do
                local entry: { [string]: any } = { curve = e.curve }
                if e.fixed then
                    entry.duration, entry.delay, entry.spring = e.duration, e.delay, e.spring
                elseif quick then
                    entry.duration, entry.delay = 0.001, 0
                    entry.curve = nil
                else
                    entry.duration = if e.duration then e.duration / k else nil
                    entry.delay = if e.delay then e.delay / k else nil
                    if e.spring then
                        -- Twice the speed is half the time: four times the
                        -- stiffness, twice the damping.
                        entry.spring = {
                            stiffness = (e.spring.stiffness or 300) * k * k,
                            damping = (e.spring.damping or 26) * k,
                            mass = e.spring.mass,
                        }
                    end
                end
                set[name] = entry
            end
            g.timing(set :: any)
        end)

        t.crossing = spec.crossing or "theme"
        g.timing_default { [t.crossing] = { duration = 0.25, curve = IN_OUT_QUAD } }

        return t :: Theme
    end)
end

return M
