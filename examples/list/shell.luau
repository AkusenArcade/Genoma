--!strict
-- A run prompt over every program in /usr/bin: thousands of rows, of which
-- `g.list` mounts only the dozen in view. Type to filter, Up and Down (or
-- Page Up and Page Down) to choose, Return or a click to run, Escape to
-- leave. The wheel steps through the list, a touchpad moves and flings it.
--
--   genoma examples/list
local g = require("@genoma")

local theme = {
    background = "#16181df2",
    line = "#2a2e37",
    text = "#d8dee9",
    faint = "#7b8394",
    accent = "#88c0d0",
    chosen = "#88c0d02e",
}

local ROW = 28
local SHOWN = 12

-- Read once: /usr/bin changes when packages do.
local folder = g.folder { path = "/usr/bin", watch = false }
local programs = g.computed(function()
    local names = {}
    for _, e in folder.entries do
        if not e.dir then table.insert(names, e.name) end
    end
    return names
end)

local query = g.signal("")
local matches = g.computed(function()
    local q = string.lower(query.value)
    if q == "" then return programs.value end
    local found = {}
    for _, name in programs.value do
        if string.find(string.lower(name), q, 1, true) then table.insert(found, name) end
    end
    return found
end)

-- The chosen row's place, from 1; back to the first whenever the filter
-- changes.
local chosen = g.signal(1)
g.watch(function() return query.value end, function() chosen.value = 1 end)

local function choose(by: number)
    local count = #matches.value
    if count == 0 then return end
    chosen.value = math.clamp(chosen.value + by, 1, count)
end

local function run(name: string?)
    if not name then return end
    -- In a session of its own: it outlives the prompt.
    g.process { command = { "setsid", "-f", name } }
    g.quit()
end

return g.surface {
    anchor = {},
    keyboard = "exclusive",
    namespace = "genoma-list",

    g.capsule {
        direction = "column",
        align = "stretch",
        gap = 10,
        padding = 14,
        radius = 14,
        width = 420,
        fill = theme.background,
        rim_top = theme.line,

        g.box {
            gap = 10,
            g.field {
                grow = 1,
                size = 16,
                color = theme.text,
                caret_color = theme.accent,
                selection_color = "#88c0d055",
                placeholder = "Run…",
                focused = true,
                text = function() return query.value end,
                on_change = function(text: string) query.value = text end,
                on_submit = function() run(matches.value[chosen.value]) end,
                -- The keys the field leaves.
                on_key = function(name: string)
                    if name == "Escape" then g.quit()
                    elseif name == "Down" then choose(1)
                    elseif name == "Up" then choose(-1)
                    elseif name == "Next" then choose(SHOWN - 1)
                    elseif name == "Prior" then choose(-(SHOWN - 1))
                    end
                end,
            },
            g.text {
                size = 12,
                color = theme.faint,
                text = function() return `{#matches.value} of {#programs.value}` end,
            },
        },

        g.list {
            height = ROW * SHOWN,
            row_height = ROW,
            radius = 8,
            items = function() return matches.value end,
            key = function(name: string) return name end,
            reveal = function() return chosen.value end,
            row = function(name: () -> string, index: () -> number)
                return g.capsule {
                    padding_x = 10,
                    radius = 6,
                    cursor = "pointer",
                    fill = function() return if index() == chosen.value then theme.chosen else "#00000000" end,
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                    on_click = function() run(name()) end,
                    g.text {
                        grow = 1,
                        size = 14,
                        color = function() return if index() == chosen.value then theme.text else theme.faint end,
                        text = name,
                    },
                }
            end,
        },
    },
}
