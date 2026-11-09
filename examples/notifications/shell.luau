--!strict
-- A notification centre: Genoma serves org.freedesktop.Notifications, and
-- what arrives stacks in a column at the top right, newest first, each with
-- its actions and a close; "Clear all" dismisses the lot. The column is
-- there only while something is.
--
--   genoma examples/notifications
--
-- One program owns the session's notifications: with another shell running,
-- try it in a session bus of its own,
--   dbus-run-session -- sh -c 'genoma examples/notifications & sleep 1; notify-send Hello there; wait'
local g = require("@genoma")
local notifications = require("@genoma/notifications")
local kit = require("@genoma/kit")

local theme = kit.theme

local function newest_first(): { any }
    local list = notifications.list
    local out = {}
    for i = #list, 1, -1 do table.insert(out, list[i]) end
    return out
end

local function card(n: () -> any): any
    local function actions(): { any } return n().actions or {} end
    return g.capsule {
        direction = "column",
        align = "stretch",
        gap = 6,
        padding = 12,
        radius = 12,
        width = 340,
        fill = function() return theme.get("background") end,
        rim_top = function() return if n().urgency == 2 then theme.get("danger") else theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,
        g.box {
            justify = "between",
            g.text {
                size = 11,
                weight = 600,
                tracking = 0.04,
                color = function() return theme.get("faint") end,
                text = function() return string.upper(n().app_name or "") end,
            },
            kit.button {
                text = "×",
                kind = "flat",
                height = 22,
                padding_x = 6,
                on_click = function() notifications:close(g.untracked(n).id, "dismissed") end,
            },
        },
        g.text {
            size = 14,
            weight = 600,
            wrap = true,
            max_width = 316,
            color = function() return theme.get("text") end,
            text = function() return n().summary or "" end,
        },
        g.text {
            present = function() return (n().body or "") ~= "" end,
            size = 13,
            wrap = true,
            max_width = 316,
            color = function() return theme.get("faint") end,
            text = function() return n().body or "" end,
        },
        g.each {
            present = function() return #actions() > 0 end,
            gap = 6,
            items = actions,
            key = function(a: any) return a.key end,
            row = function(a: () -> any)
                return kit.button {
                    text = function() return a().label end,
                    height = 26,
                    on_click = function() notifications:invoke(g.untracked(n).id, g.untracked(a).key) end,
                }
            end,
        },
    }
end

return g.surface {
    anchor = { "top", "right" },
    margin = { top = 12, right = 12 },
    namespace = "genoma-notifications",
    present = function() return #notifications.list > 0 end,

    g.box {
        direction = "column",
        align = "end",
        gap = 8,
        kit.button {
            text = "Clear all",
            kind = "flat",
            on_click = function()
                for _, n in g.untracked(function() return notifications.list end) do
                    notifications:close(n.id, "dismissed")
                end
            end,
        },
        g.each {
            direction = "column",
            gap = 8,
            items = newest_first,
            key = function(n: any) return n.id end,
            row = card,
        },
    },
}
