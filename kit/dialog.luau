--!strict
-- A dialog: a question over everything, the rest dimmed until it is
-- answered.
--
--   local confirm = g.signal(false)
--   local ask = dialog {
--       present = function() return confirm.value end,
--       on_close = function() confirm.value = false end,
--       title = "Delete three files?",
--       text = "They cannot be recovered.",
--       actions = {
--           { text = "Cancel" },
--           { text = "Delete", kind = "danger", on_click = delete },
--       },
--   }
--   return { panel, ask }
--
-- It is a surface of its own, returned beside the others. Each action is
-- a kit button; choosing one calls its on_click, then on_close. Escape and
-- a press on the dimmed rest close it; Return chooses the last action. A
-- node given as its child is shown between the text and the actions.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")
local button = require("@genoma/kit/button")

local read = theme.read

return function(t: { [string]: any }): any
    local function close()
        if t.on_close then t.on_close() end
    end
    local function act(action: any)
        if read(action.disabled) == true then return end
        if action.on_click then action.on_click() end
        close()
    end
    local actions = t.actions or { { text = "OK", kind = "accent" } }

    local row = { gap = 8, justify = "end" }
    for _, action in actions do
        table.insert(row, button {
            text = action.text,
            kind = action.kind,
            disabled = action.disabled,
            on_click = function() act(action) end,
        })
    end

    return g.surface {
        layer = "overlay",
        anchor = { "top", "bottom", "left", "right" },
        full = true,
        above_zones = true,
        namespace = t.namespace or "genoma-kit-dialog",
        present = t.present,
        function(_output: string?, size: any)
            size = size or { width = 1920, height = 1080 }
            -- Asleep, a full surface still draws: the dialog fades itself,
            -- so it also leaves the way it came.
            local function shown(): boolean return read(t.present) ~= false end
            return g.box {
                width = size.width,
                height = size.height,
                justify = "center",
                align = "center",
                -- The rest of the screen, dimmed: a press there closes.
                g.capsule {
                    opacity = function() return if shown() then 1 else 0 end,
                    transition = { opacity = "kit_open" },
                    overlay = true,
                    attach = "corner",
                    width = size.width,
                    height = size.height,
                    radius = 0,
                    fill = t.scrim or "#00000080",
                    rim_top = "#00000000",
                    rim_bottom = "#00000000",
                    on_click = close,
                },
                g.capsule {
                    catch = true,
                    opacity = function() return if shown() then 1 else 0 end,
                    transition = { opacity = "kit_open" },
                    role = "group",
                    name = if type(t.title) == "string" then t.title else nil,
                    on_key = function(name: string)
                        if name == "Escape" then close()
                        elseif name == "Return" or name == "KP_Enter" then act(actions[#actions])
                        end
                    end,
                    -- A press on the card is the card's, not the dimmed rest's.
                    on_click = function() end,
                    direction = "column",
                    align = "stretch",
                    gap = 12,
                    padding = 20,
                    width = t.width or 380,
                    radius = function() return theme.get("radius") + 6 end,
                    fill = function() return theme.get("background") end,
                    rim_top = function() return theme.get("line") end,
                    rim_bottom = function() return theme.get("line") end,
                    shadow = "#00000080",
                    shadow_blur = 32,
                    shadow_y = 10,
                    g.text {
                        present = t.title ~= nil,
                        size = function() return theme.get("text_size") + 3 end,
                        weight = 600,
                        color = function() return theme.get("text") end,
                        text = t.title or "",
                    },
                    g.text {
                        present = t.text ~= nil,
                        wrap = true,
                        max_width = (t.width or 380) - 40,
                        size = function() return theme.get("text_size") end,
                        color = function() return theme.get("faint") end,
                        text = t.text or "",
                    },
                    t[1] or g.box { present = false },
                    g.box(row),
                },
            }
        end,
    }
end
