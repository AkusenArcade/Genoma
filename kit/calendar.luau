--!strict
-- A month to choose a day from: its weeks in a grid, today ringed, the
-- chosen day lit, days with something on them marked.
--
--   local calendar = require("@genoma/kit/calendar")
--   local view = {
--       date = function() return day.value end,            -- { year, month, day } or nil
--       on_select = function(d) day.value = d end,
--       marks = function(d) return if busy[calendar.stamp(d)] then "#e0af68" else nil end,
--   }
--   calendar(view)
--   -- in the shell's on_key: if calendar.key(view, name) then return end
--
-- range = true chooses two days: date gives { from, to } and on_select is
-- told { from, to }, `to` nil after the first press. first_weekday is 1
-- (Monday) to 7 (Sunday). min and max bound what can be chosen. names =
-- { months = { … 12 }, days = { … 7, from Monday } } says them in another
-- language. The month shown follows the choice; the wheel, a swipe and
-- the arrows in the header turn it, and month/on_month take it over.
-- calendar.key(view, name, mods): the arrows move the day (a week up and
-- down), Page_Up and Page_Down the month, Home and End the month's ends;
-- with range, shift and an arrow stretch the range.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")
local button = require("@genoma/kit/button")
-- The clock service, started by the first calendar that asks for today,
-- not by requiring the kit.
local clock: any = nil

local read = theme.read

export type Date = { year: number, month: number, day: number }

export type Calendar = {
    date: (Date? | { from: Date?, to: Date? } | () -> any)?,
    on_select: ((d: any) -> ())?,
    range: boolean?,
    first_weekday: number?,
    min: Date?,
    max: Date?,
    marks: ((d: Date) -> string?)?,
    names: { months: { string }, days: { string } }?,
    month: (() -> { year: number, month: number })?,
    on_month: ((year: number, month: number) -> ())?,
    cell: number?,
    [any]: any,
}

local MONTHS = { "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December" }
local DAYS = { "Mo", "Tu", "We", "Th", "Fr", "Sa", "Su" }

local M = {}

-- A date as one number, for comparing: 20261112.
function M.stamp(d: Date): number
    return d.year * 10000 + d.month * 100 + d.day
end

-- n days on (or back) from d, across months and years.
function M.add(d: Date, n: number): Date
    local t = os.date("*t", os.time { year = d.year, month = d.month, day = d.day + n, hour = 12 }) :: any
    return { year = t.year, month = t.month, day = t.day }
end

function M.days_in(year: number, month: number): number
    return (os.date("*t", os.time { year = year, month = month + 1, day = 0, hour = 12 }) :: any).day
end

-- 1 Monday … 7 Sunday.
function M.weekday(d: Date): number
    local w = (os.date("*t", os.time { year = d.year, month = d.month, day = d.day, hour = 12 }) :: any).wday
    return (w + 5) % 7 + 1
end

-- Today, again each hour so it turns at midnight.
function M.today(): Date
    clock = clock or require("@genoma/clock")
    local _ = clock.hour
    local t = os.date("*t") :: any
    return { year = t.year, month = t.month, day = t.day }
end

local function month_add(y: number, m: number, n: number): (number, number)
    local i = y * 12 + (m - 1) + n
    return i // 12, i % 12 + 1
end

-- Each view's own month, when the shell keeps none.
local shown_of: { [any]: any } = setmetatable({}, { __mode = "k" }) :: any

local function chosen(t: Calendar): (Date?, Date?)
    local v = read(t.date)
    if v == nil then return nil, nil end
    if t.range then return v.from, v.to end
    return v, nil
end

local function shown(t: Calendar): any
    local s = shown_of[t]
    if s then return s end
    local first = g.untracked(function() return chosen(t) end) or g.untracked(M.today)
    s = g.lasting(function() return g.signal({ year = first.year, month = first.month }) end)
    shown_of[t] = s
    return s
end

local function month_of(t: Calendar): { year: number, month: number }
    if t.month then return t.month() end
    return shown(t).value
end

local function turn(t: Calendar, n: number)
    local now = g.untracked(function() return month_of(t) end)
    local y, m = month_add(now.year, now.month, n)
    if not t.month then shown(t).value = { year = y, month = m } end
    if t.on_month then t.on_month(y, m) end
end

local function allowed(t: Calendar, d: Date): boolean
    if t.min and M.stamp(d) < M.stamp(t.min) then return false end
    if t.max and M.stamp(d) > M.stamp(t.max) then return false end
    return true
end

local function pick(t: Calendar, d: Date)
    if not allowed(t, d) then return end
    -- The month shown follows a day chosen outside it.
    local now = g.untracked(function() return month_of(t) end)
    if d.year ~= now.year or d.month ~= now.month then
        turn(t, (d.year - now.year) * 12 + d.month - now.month)
    end
    if not t.on_select then return end
    if not t.range then
        t.on_select(d)
        return
    end
    local from, to = g.untracked(function() return chosen(t) end)
    if from == nil or to ~= nil then
        t.on_select({ from = d, to = nil })
    elseif M.stamp(d) < M.stamp(from) then
        t.on_select({ from = d, to = from })
    else
        t.on_select({ from = from, to = d })
    end
end

-- calendar.key(view, name, mods).
function M.key(t: Calendar, name: string, mods: { shift: boolean? }?): boolean
    local from, to = g.untracked(function() return chosen(t) end)
    local at = to or from
    local m = g.untracked(function() return month_of(t) end)
    if at == nil then
        at = g.untracked(M.today)
        if at.year ~= m.year or at.month ~= m.month then at = { year = m.year, month = m.month, day = 1 } end
    end
    local d: Date? = nil
    if name == "Left" then d = M.add(at :: Date, -1)
    elseif name == "Right" then d = M.add(at :: Date, 1)
    elseif name == "Up" then d = M.add(at :: Date, -7)
    elseif name == "Down" then d = M.add(at :: Date, 7)
    elseif name == "Page_Up" or name == "Prior" or name == "Page_Down" or name == "Next" then
        local y, mo = month_add((at :: Date).year, (at :: Date).month, if name == "Page_Up" or name == "Prior" then -1 else 1)
        d = { year = y, month = mo, day = math.min((at :: Date).day, M.days_in(y, mo)) }
    elseif name == "Home" then d = { year = (at :: Date).year, month = (at :: Date).month, day = 1 }
    elseif name == "End" then d = { year = (at :: Date).year, month = (at :: Date).month, day = M.days_in((at :: Date).year, (at :: Date).month) }
    end
    if d == nil then return false end
    if not t.range then
        pick(t, d)
        return true
    end
    if not allowed(t, d) or not t.on_select then return true end
    if mods and mods.shift and from ~= nil then
        -- Shift and an arrow move the far end of the range.
        if M.stamp(d) < M.stamp(from) then t.on_select({ from = d, to = from }) else t.on_select({ from = from, to = d }) end
    else
        t.on_select({ from = d, to = nil })
    end
    local now = g.untracked(function() return month_of(t) end)
    if d.year ~= now.year or d.month ~= now.month then turn(t, (d.year - now.year) * 12 + d.month - now.month) end
    return true
end

setmetatable(M, {
    __call = function(_, t: Calendar): any
        local names = t.names or { months = MONTHS, days = DAYS }
        local first_weekday = t.first_weekday or 1
        local size = t.cell or 36

        -- The 42 days the grid shows: the month and the weeks around it.
        local grid = g.computed(function(): { Date }
            local m = month_of(t)
            local lead = (M.weekday({ year = m.year, month = m.month, day = 1 }) - first_weekday) % 7
            local start = M.add({ year = m.year, month = m.month, day = 1 }, -lead)
            local out = {}
            for i = 0, 41 do out[i + 1] = M.add(start, i) end
            return out
        end)

        local function cell(i: number): any
            local function d(): Date return grid.value[i] end
            local hovered = g.signal(false)
            local function inside(): boolean return d().month == month_of(t).month end
            local function is_today(): boolean return M.stamp(d()) == M.stamp(M.today()) end
            local function lit(): boolean
                local from, to = chosen(t)
                local s = M.stamp(d())
                return (from ~= nil and s == M.stamp(from)) or (to ~= nil and s == M.stamp(to))
            end
            local function between(): boolean
                local from, to = chosen(t)
                if from == nil or to == nil then return false end
                local s = M.stamp(d())
                return s > M.stamp(from) and s < M.stamp(to)
            end
            local function can(): boolean return allowed(t, d()) end
            return g.capsule {
                role = "button",
                cursor = function() return if can() then "pointer" else "not-allowed" end,
                width = size,
                height = size - 4,
                radius = function() return theme.get("radius") end,
                direction = "column",
                gap = 1,
                fill = function()
                    if lit() then return theme.get("accent") end
                    if between() then return theme.get("selection") end
                    if hovered.value and can() then return theme.get("control_hover") end
                    return theme.alpha(theme.get("control_hover"), 0)
                end,
                outline = function() return if is_today() and not lit() then theme.get("accent") else "#00000000" end,
                outline_width = 1.5,
                transition = { fill = "kit_quick" },
                opacity = function() return if can() then 1 else theme.get("disabled_opacity") end,
                on_hover = function(on: boolean) hovered.value = on end,
                on_click = function(b: number)
                    if b == 272 then pick(t, g.untracked(d)) end
                end,
                g.text {
                    size = function() return theme.get("text_size") end,
                    tabular = true,
                    weight = function() return if lit() or is_today() then 600 else 400 end,
                    color = function()
                        if lit() then return theme.get("on_accent") end
                        if inside() then return theme.get("text") end
                        return theme.get("faint")
                    end,
                    text = function() return tostring(d().day) end,
                },
                -- Something on that day.
                g.capsule {
                    width = 4,
                    height = 4,
                    opacity = function() return if t.marks and t.marks(d()) then 1 else 0 end,
                    fill = function()
                        local c = if t.marks then t.marks(d()) else nil
                        return if lit() then theme.get("on_accent") else c or "#00000000"
                    end,
                },
            }
        end

        local weeks = {}
        for w = 0, 5 do
            local days = {}
            for k = 1, 7 do table.insert(days, cell(w * 7 + k)) end
            table.insert(weeks, g.box { gap = 2, table.unpack(days) })
        end
        local heads = {}
        for k = 0, 6 do
            local name = names.days[(first_weekday - 1 + k) % 7 + 1]
            table.insert(heads, g.box {
                width = size,
                justify = "center",
                g.text { size = function() return theme.get("text_size") - 2 end, color = function() return theme.get("faint") end, text = name },
            })
        end

        -- A touchpad's distance turns a month at a time.
        local travel = 0
        local node: { [any]: any } = {
            direction = "column",
            align = "start",
            gap = 6,
            on_wheel = function(_: number, dy: number, steps: boolean)
                if steps then
                    turn(t, if dy > 0 then 1 else -1)
                    return
                end
                travel += dy
                if math.abs(travel) >= 60 then
                    turn(t, if travel > 0 then 1 else -1)
                    travel = 0
                end
            end,
            on_swipe = function(way: string)
                if way == "left" then turn(t, 1) elseif way == "right" then turn(t, -1) end
            end,
            g.box {
                width = size * 7 + 12,
                justify = "between",
                g.text {
                    size = function() return theme.get("text_size") + 1 end,
                    weight = 600,
                    color = function() return theme.get("text") end,
                    text = function()
                        local m = month_of(t)
                        return `{names.months[m.month]} {m.year}`
                    end,
                },
                g.box {
                    gap = 4,
                    button { text = "‹", kind = "flat", name = "Previous month", on_click = function() turn(t, -1) end },
                    button { text = "›", kind = "flat", name = "Next month", on_click = function() turn(t, 1) end },
                },
            },
            g.box { gap = 2, table.unpack(heads) },
            g.box { direction = "column", gap = 2, table.unpack(weeks) },
        }
        local own = {
            date = true, on_select = true, range = true, first_weekday = true, min = true, max = true,
            marks = true, names = true, month = true, on_month = true, cell = true,
        }
        for k, v in t do
            if type(k) == "string" and node[k] == nil and not (own :: any)[k] then node[k] = v end
        end
        return g.box(node)
    end,
})

return M
