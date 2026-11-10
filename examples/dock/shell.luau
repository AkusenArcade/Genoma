--!strict
-- A dock at the bottom: the applications pinned, then the others that have
-- windows open, each its icon from @genoma/apps. A dot under one that runs,
-- the focused one lit. A click focuses its window (the next one, clicked
-- again) or starts it; a tooltip names it. On any compositor.
--
--   genoma examples/dock
local g = require("@genoma")
local apps = require("@genoma/apps")
local compositor = require("@genoma/compositor")
local kit = require("@genoma/kit")

local theme = kit.theme
local SIZE = 44

-- Edit to taste: desktop file ids.
local PINNED = { "org.gnome.Nautilus", "firefox", "Alacritty" }

type Slot = { key: string, app: any?, windows: { any }, pinned: boolean }

-- Pinned first, in their order; then each application with windows, by
-- the first window's place.
local slots = g.computed(function(): { Slot }
    local out: { Slot } = {}
    local by: { [string]: Slot } = {}
    for _, id in PINNED do
        local app = apps:find(id)
        local key = if app then app.id else id
        local slot = { key = key, app = app, windows = {}, pinned = true }
        table.insert(out, slot)
        by[key] = slot
    end
    for _, w in compositor.windows do
        local app = w.app_id and apps:find(w.app_id)
        local key = if app then app.id else (w.app_id or tostring(w.id))
        local slot = by[key]
        if not slot then
            slot = { key = key, app = app, windows = {}, pinned = false }
            table.insert(out, slot)
            by[key] = slot
        end
        table.insert(slot.windows, w)
    end
    return out
end)

local function focused_in(slot: Slot): boolean
    local f = compositor.focused
    if not f then return false end
    for _, w in slot.windows do
        if w.id == f.id then return true end
    end
    return false
end

-- The slot under the pointer, by key, once it has rested there: one
-- tooltip for the dock, following it (a popup cannot live in a row).
local hovered = g.signal(nil :: string?)
local shown = g.signal(nil :: string?)
local waiting: any = nil
local function hover(key: string?)
    hovered.value = key
    if waiting then waiting:stop(); waiting = nil end
    if key == nil then
        shown.value = nil
    elseif g.untracked(function() return shown.value end) then
        shown.value = key
    else
        waiting = g.after(0.5, function() waiting = nil; shown.value = hovered.value end)
    end
end

local function name_of(key: string?): string
    for _, s in slots.value do
        if s.key == key then
            local name = if s.app then s.app.name else s.key
            return if #s.windows > 1 then `{name} · {#s.windows} windows` else name
        end
    end
    return ""
end

local function activate(slot: Slot)
    if #slot.windows == 0 then
        if slot.app then apps:launch(slot.app.id) end
        return
    end
    -- The window after the focused one, round: a second click goes on.
    local f = compositor.focused
    local at = 1
    for i, w in slot.windows do
        if f and w.id == f.id then at = i % #slot.windows + 1 end
    end
    compositor:focus_window(slot.windows[at].id)
end

local dock = g.surface {
    anchor = "bottom",
    margin = { bottom = 8 },
    namespace = "genoma-dock",

    g.capsule {
        padding = 8,
        gap = 6,
        radius = 18,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,
        g.each {
            gap = 6,
            items = function() return slots.value end,
            key = function(s: Slot) return s.key end,
            row = function(slot: () -> Slot)
                local key = g.untracked(slot).key
                local function lit(): boolean return hovered.value == key end
                return g.capsule {
                        id = "dock-" .. key,
                        role = "button",
                        name = (function() local s = g.untracked(slot); return if s.app then s.app.name else s.key end)(),
                        cursor = "pointer",
                        direction = "column",
                        gap = 3,
                        padding = 4,
                        radius = 12,
                        fill = function()
                            if focused_in(slot()) then return theme.get("selection") end
                            return if lit() then theme.alpha(theme.get("control_hover"), 0.8) else theme.alpha(theme.get("control"), 0)
                        end,
                        rim_top = "#00000000",
                        rim_bottom = "#00000000",
                        transition = { fill = "kit_quick" },
                        on_hover = function(on: boolean)
                            if on then hover(key) elseif g.untracked(function() return hovered.value end) == key then hover(nil) end
                        end,
                        on_click = function(b: number) if b == 272 then activate(g.untracked(slot)) end end,
                        g.icon {
                            width = SIZE,
                            height = SIZE,
                            icon = function()
                                local s = slot()
                                local name = if s.app then s.app.icon else "application-x-executable"
                                return g.icon_path(name) or name
                            end,
                        },
                        -- Running: a dot, wider for more than one window.
                        g.capsule {
                            height = 4,
                            width = function() return if #slot().windows > 1 then 12 else 4 end,
                            radius = 2,
                            fill = function()
                                if #slot().windows == 0 then return "#00000000" end
                                return if focused_in(slot()) then theme.get("accent") else theme.get("faint")
                            end,
                            rim_top = "#00000000",
                            rim_bottom = "#00000000",
                            transition = { width = "kit_move", fill = "kit_quick" },
                        },
                    }
            end,
        },
    },
}

-- One tooltip, above the slot it names.
local tip = g.popup {
    anchor = function() return "dock-" .. (shown.value or "") end,
    side = "top",
    align = "center",
    gap = 6,
    grab = false,
    present = function() return shown.value ~= nil end,
    g.capsule {
        padding_x = 8,
        padding_y = 5,
        radius = 6,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,
        g.text { size = 12, color = function() return theme.get("text") end, text = function() return name_of(shown.value) end },
    },
}

return { dock, tip }
