--!strict
-- Every service Genoma has, each a module of reactive values, here with
-- what it says right now. The ones that own a name on the bus (the
-- notification server, the tray's watcher, the polkit agent, the session
-- lock) are not started by the showcase: the shell running beside it
-- holds those names, so they say what they do instead. Nothing that shows
-- what is on the clipboard or in a window's title is shown.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")

local clock = require("@genoma/clock")
local niri = require("@genoma/niri")
local compositor = require("@genoma/compositor")
local vitals = require("@genoma/vitals")
local power = require("@genoma/power")
local network = require("@genoma/network")
local media = require("@genoma/media")
local audio = require("@genoma/audio")
local bluetooth = require("@genoma/bluetooth")
local toplevels = require("@genoma/toplevels")
local displays = require("@genoma/displays")
local apps = require("@genoma/apps")
local drives = require("@genoma/drives")
local session = require("@genoma/session")
local portal = require("@genoma/portal")
local idle = require("@genoma/idle")

local u = style.u
local cap = style.cap
local t = style.t

local function count(list: any, one: string, many: string): string
    local n = if type(list) == "table" then #list else 0
    return n .. " " .. (if n == 1 then one else many)
end

-- name, what it reads now (or nil: what it does instead, in `does`).
local SERVICES: { { any } } = {
    { "clock", function() return string.format("%02d:%02d:%02d", clock.hour, clock.minute, clock.second) end },
    { "vitals", function() return string.format("CPU %d%%", math.floor(vitals.cpu * 100 + 0.5)) end },
    { "niri", function()
        local out = niri.focused_output
        return if out then count(niri:workspaces(out), "workspace", "workspaces") else "—"
    end },
    { "compositor", function() return compositor.name or "—" end },
    { "toplevels", function() return count(toplevels.list, "window", "windows") end },
    { "displays", function() return count(displays.heads, "screen", "screens") end },
    { "audio", function()
        for _, n in audio.nodes do
            if n.name == audio.default_sink then
                return if n.muted then "muted" else string.format("volume %d%%", math.floor(n.volume * 100 + 0.5))
            end
        end
        return "—"
    end },
    { "media", function()
        for _, p in media.players do
            if p.status == "Playing" and p.title and p.title ~= "" then return p.title end
        end
        return "silence"
    end },
    { "network", function() return tostring(network.connectivity or "—") end },
    { "bluetooth", function()
        local a = bluetooth.adapter
        if not a then return "no adapter" end
        if not a.powered then return "off" end
        local n = 0
        for _, d in bluetooth.devices do if d.connected then n += 1 end end
        return n .. " connected"
    end },
    { "power", function()
        local b = power.battery
        if b and b.present then return string.format("battery %d%%", math.floor(b.level * 100 + 0.5)) end
        return if power.profile ~= "" then power.profile else "on mains"
    end },
    { "session", function() return count(session.inhibitors, "inhibitor", "inhibitors") end },
    { "idle", function() return if idle.idle then "idle" else "in use" end },
    { "apps", function() return count(apps.list, "app", "apps") end },
    { "drives", function() return count(drives.list, "drive", "drives") end },
    { "portal", function() return tostring(portal.color_scheme or "—") .. " scheme" end },
    { "notifications", nil, "a server of its own" },
    { "tray", nil, "status items" },
    { "polkit", nil, "an auth agent" },
    { "lock", nil, "the session lock" },
    { "clipboard", nil, "history, images" },
    { "capture", nil, "shots, recordings" },
    { "gamma", nil, "night light" },
    { "brightness", nil, "backlight, DDC" },
    { "spectrum", nil, "64 bands of sound" },
    { "accounts", nil, "users, pictures" },
}

do
    local tiles = {}
    for i = 1, #SERVICES do
        tiles["tile_" .. i] = { duration = 0.7, curve = { 0.16, 1, 0.3, 1 }, delay = 0.25 + (i - 1) * 0.035 }
    end
    g.timing(tiles)
end

local COLUMNS = 6
local TILE_W, TILE_H, GAP = 158, 142, 10

return {
    key = "services",
    kicker = "Services",
    title = "The system, as signals",
    caption = "Twenty-six services in Rust, each a module of values that change when the system does: no process to start, no poll, no parsing. Read one in a binding and it is live.",
    duration = 12,
    code = [[
local audio = require("@genoma/audio")
local media = require("@genoma/media")

g.text { text = function()
    for _, p in media.players do
        if p.status == "Playing" then
            return p.title
        end
    end
    return "silence"
end }]],

    build = function(ctx: { [string]: any }): any
        local grid: { any } = {
            width = function() return u(1000) end,
            height = function() return u(800) end,
        }
        for i, s in SERVICES do
            local col, row = (i - 1) % COLUMNS, (i - 1) // COLUMNS
            local live = s[2] ~= nil
            local function shown(): boolean return ctx.arrived() end
            table.insert(grid, cap {
                x = col * (TILE_W + GAP),
                w = TILE_W, h = TILE_H, r = 22,
                fill = function() return g.colour.alpha(t.colour("text"), if live then 0.06 else 0.03) end,
                rim_top = function() return g.colour.alpha(if live then t.colour("accent") else t.colour("text"), if live then 0.45 else 0.14) end,
                rim_bottom = function() return g.colour.alpha(t.colour("text"), 0.05) end,
                opacity = function() return if shown() then 1 else 0 end,
                offset_y = function() return u(row * (TILE_H + GAP + 6) + (if shown() then 0 else 24)) end,
                transition = { opacity = "tile_" .. i, offset_y = "tile_" .. i },
                direction = "column",
                align = "start",
                justify = "between",
                padding = function() return u(16) end,
                g.box {
                    direction = "column",
                    align = "start",
                    gap = function() return u(6) end,
                    cap {
                        overlay = false, w = 10, h = 10,
                        fill = function() return if live then t.colour("accent") else g.colour.alpha(t.colour("text"), 0.25) end,
                        shadow = function() return if live then g.colour.alpha(t.colour("accent"), 0.8) else "#00000000" end,
                        shadow_blur = function() return u(10) end,
                    },
                    style.text("label", { text = string.upper(s[1]), tracking = 0.08, color = function() return if live then t.colour("text") else t.colour("muted") end }),
                },
                g.text {
                    text = if live then s[2] else s[3],
                    font = style.body,
                    italic = not live,
                    size = function() return math.floor(u(if live then 20 else 17)) end,
                    wrap = true,
                    max_width = function() return u(TILE_W - 32) end,
                    color = function() return if live then t.colour("glow") else t.colour("faint") end,
                },
            })
        end
        return frame.new {
            number = ctx.number,
            kicker = "Services",
            title = "The system, as signals",
            caption = "Twenty-six services in Rust, each a module of values that change when the system does: no process to start, no poll, no parsing. Read one in a binding and it is live.",
            arrived = ctx.arrived,
            demo = g.box(grid),
        }
    end,
}
