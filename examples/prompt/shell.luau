--!strict
-- A prompt: one line to type into, centred on the screen, what was entered
-- listed above it. The field is Genoma's own: caret and selection by key
-- and by pointer (double-click a word, triple-click the line), undo with
-- Ctrl+Z, the clipboard with Ctrl+C/X/V, and input methods for scripts
-- typed by composition.
--
--   genoma examples/prompt
--
-- Return enters the line, Escape leaves. The eye hides what is typed, as
-- a password field does: drawn as dots, never copied out.
local g = require("@genoma")

local theme = {
    background = "#16181df2",
    line = "#2a2e37",
    text = "#d8dee9",
    faint = "#7b8394",
    accent = "#88c0d0",
}

-- What the field holds, kept here: the field tells every change, and is
-- told back when the prompt clears it.
local value = g.signal("")
local hidden = g.signal(false)
local entered = g.signal({} :: { { id: number, text: string } })

local function enter(text: string)
    if text == "" then return end
    local list = table.clone(entered.value)
    table.insert(list, { id = #list + 1, text = if hidden.value then string.rep("•", utf8.len(text) or #text) else text })
    entered.value = list
    value.value = ""
end

return g.surface {
    -- No edge: the compositor centres it. It takes the keyboard for as long
    -- as it is open.
    anchor = {},
    keyboard = "exclusive",
    namespace = "genoma-prompt",

    g.capsule {
        direction = "column",
        align = "start",
        gap = 10,
        padding = 16,
        radius = 14,
        fill = theme.background,
        rim_top = theme.line,

        g.each {
            direction = "column",
            align = "start",
            gap = 4,
            items = function() return entered.value end,
            key = function(line) return line.id end,
            row = function(line)
                return g.text { size = 14, color = theme.faint, text = function() return line().text end }
            end,
        },

        g.box {
            gap = 12,
            g.field {
                width = 360,
                size = 16,
                color = theme.text,
                caret_color = theme.accent,
                selection_color = "#88c0d055",
                placeholder = "Type, then Return",
                focused = true,
                secret = function() return hidden.value end,
                text = function() return value.value end,
                on_change = function(text: string) value.value = text end,
                on_submit = enter,
                -- Keys the field leaves: Escape closes the prompt.
                on_key = function(name: string)
                    if name == "Escape" then g.quit() end
                end,
            },
            g.capsule {
                role = "button",
                name = "Hide what is typed",
                cursor = "pointer",
                padding_x = 8,
                height = 24,
                radius = 6,
                fill = function() return if hidden.value then theme.accent else theme.line end,
                on_click = function() hidden.value = not hidden.value end,
                g.text {
                    size = 12,
                    weight = 600,
                    color = function() return if hidden.value then theme.background else theme.text end,
                    text = function() return if hidden.value then "HIDDEN" else "SHOWN" end,
                },
            },
        },
    },
}
