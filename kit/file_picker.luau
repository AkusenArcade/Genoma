--!strict
-- Files to choose: a folder's entries, places to jump to, the path as
-- steps back, a filter typed over the list, and a look at a picture.
--
--   local file_picker = require("@genoma/kit/file_picker")
--   file_picker { folder = "~/Pictures", filter = { "*.png", "*.jpg" }, preview = true,
--                 on_choose = function(paths) set_wallpaper(paths[1]) end,
--                 on_cancel = function() picking.value = false end }
--
-- folder: where it opens. on_choose(paths) is told the chosen paths, one
-- unless `multiple`. folders = true chooses folders instead of files (the
-- folder shown, or the one selected). filter: patterns ("*.png") or a
-- function (entry) -> boolean; folders always show. hidden shows
-- dot-files. preview shows the selected picture beside the list. places:
-- more { name, path } beside the user's own folders. A click selects, a
-- click on the selected one opens a folder or chooses a file; with
-- `multiple` a click marks. Typing filters (the field has the keys);
-- Up, Down and Return work from the field, BackSpace in an empty field
-- goes up. file_picker.key(view, name) does the same for a shell's keys.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")
local button = require("@genoma/kit/button")
local field = require("@genoma/kit/field")

local read = theme.read

export type Picker = {
    folder: string?,
    on_choose: ((paths: { string }) -> ())?,
    on_cancel: (() -> ())?,
    multiple: boolean?,
    folders: boolean?,
    filter: ({ string } | (entry: any) -> boolean)?,
    hidden: (boolean | () -> boolean)?,
    preview: boolean?,
    places: { { name: string, path: string } }?,
    width: number?,
    height: number?,
    [any]: any,
}

local HOME = g.env("HOME") or "/"

local function expand(path: string): string
    if path == "~" then return HOME end
    if string.sub(path, 1, 2) == "~/" then return HOME .. string.sub(path, 2) end
    return path
end

local function parent_of(path: string): string
    local up = string.match(path, "^(.*)/[^/]+/?$")
    return if up == nil or up == "" then "/" else up
end

local function extension(name: string): string
    return string.lower(string.match(name, "%.([^%.]+)$") or "")
end

local KINDS: { [string]: string } = {
    png = "image", jpg = "image", jpeg = "image", webp = "image", gif = "image", svg = "image", bmp = "image", avif = "image",
    mp3 = "audio", flac = "audio", ogg = "audio", opus = "audio", wav = "audio", m4a = "audio",
    mp4 = "video", mkv = "video", webm = "video", mov = "video", avi = "video",
    pdf = "pdf", zip = "archive", tar = "archive", gz = "archive", xz = "archive", zst = "archive", ["7z"] = "archive",
}
local ICONS = {
    folder = { "folder" }, image = { "image-x-generic" }, audio = { "audio-x-generic" }, video = { "video-x-generic" },
    pdf = { "application-pdf", "x-office-document" }, archive = { "package-x-generic", "application-x-archive" },
    file = { "text-x-generic", "unknown" },
}
local icon_cache: { [string]: string } = {}
local function icon_for(kind: string): string?
    local hit = icon_cache[kind]
    if hit ~= nil then return if hit == "" then nil else hit end
    for _, name in ICONS[kind] or ICONS.file do
        local path = g.icon_path(name)
        if path then
            icon_cache[kind] = path
            return path
        end
    end
    icon_cache[kind] = ""
    return nil
end

local function kind_of(e: any): string
    if e.dir then return "folder" end
    return KINDS[extension(e.name)] or "file"
end

local function size_text(bytes: number?): string
    if bytes == nil then return "" end
    if bytes < 1000 then return `{bytes} B` end
    local units = { "kB", "MB", "GB", "TB" }
    local v, u = bytes / 1000, 1
    while v >= 1000 and u < #units do v, u = v / 1000, u + 1 end
    return string.format(if v < 10 then "%.1f %s" else "%.0f %s", v, units[u])
end

-- The user's own folders, as user-dirs.dirs names them, those that exist.
local function user_places(): { { name: string, path: string } }
    local out = { { name = "Home", path = HOME } }
    local config = g.env("XDG_CONFIG_HOME") or (HOME .. "/.config")
    local file = g.file { path = config .. "/user-dirs.dirs", watch = false }
    local wanted = { DESKTOP = "Desktop", DOCUMENTS = "Documents", DOWNLOAD = "Downloads", MUSIC = "Music", PICTURES = "Pictures", VIDEOS = "Videos" }
    local found: { [string]: string } = {}
    for key, value in string.gmatch(file.text or "", "XDG_(%u+)_DIR=\"([^\"]*)\"") do
        found[key] = string.gsub(value, "%$HOME", HOME)
    end
    for _, key in { "DESKTOP", "DOCUMENTS", "DOWNLOAD", "MUSIC", "PICTURES", "VIDEOS" } do
        local path = found[key]
        if path and path ~= HOME then table.insert(out, { name = wanted[key], path = path }) end
    end
    return out
end

local function matches(t: Picker, e: any): boolean
    if e.dir then return true end
    if t.folders then return false end
    local f = t.filter
    if f == nil then return true end
    if type(f) == "function" then return (f :: any)(e) end
    local name = string.lower(e.name)
    for _, pattern in f :: { string } do
        local ext = string.match(pattern, "^%*%.(.+)$")
        if pattern == "*" or (ext and string.sub(name, -(#ext + 1)) == "." .. string.lower(ext)) or name == string.lower(pattern) then
            return true
        end
    end
    return false
end

local states: { [any]: any } = setmetatable({}, { __mode = "k" }) :: any

local function state(t: Picker): any
    local s = states[t]
    if s then return s end
    s = g.lasting(function()
        local here = g.signal(expand(t.folder or "~"))
        local entries = g.signal({} :: { any })
        local query = g.signal("")
        local cursor = g.signal(nil :: string?)
        local marked = g.signal({} :: { [string]: boolean })
        g.effect(function()
            local f = g.folder { path = here.value, hidden = read(t.hidden) == true, details = true }
            g.effect(function()
                local list = f.entries
                g.untracked(function() entries.value = list end)
            end)
        end)
        local shown = g.computed(function(): { any }
            local q = string.lower(query.value)
            local out = {}
            for _, e in entries.value do
                if matches(t, e) and (q == "" or string.find(string.lower(e.name), q, 1, true)) then
                    table.insert(out, e)
                end
            end
            table.sort(out, function(a, b)
                if a.dir ~= b.dir then return a.dir end
                return string.lower(a.name) < string.lower(b.name)
            end)
            return out
        end)
        return { here = here, entries = entries, query = query, cursor = cursor, marked = marked, shown = shown }
    end)
    states[t] = s
    return s
end

local function go(t: Picker, path: string)
    local s = state(t)
    s.here.value = path
    s.query.value = ""
    s.cursor.value = nil
    s.marked.value = {}
end

local function chosen(t: Picker): { string }
    local s = state(t)
    local out = {}
    if t.multiple then
        for path, on in g.untracked(function() return s.marked.value end) do
            if on then table.insert(out, path) end
        end
        table.sort(out)
    end
    if #out == 0 then
        local at = g.untracked(function() return s.cursor.value end)
        local entry = nil
        for _, e in g.untracked(function() return s.shown.value end) do
            if e.path == at then entry = e end
        end
        if entry and (entry.dir == (t.folders == true)) then
            table.insert(out, entry.path)
        elseif t.folders then
            table.insert(out, g.untracked(function() return s.here.value end))
        end
    end
    return out
end

local function choose(t: Picker)
    local paths = chosen(t)
    if #paths > 0 and t.on_choose then t.on_choose(paths) end
end

local function open_entry(t: Picker, e: any)
    if e.dir then
        go(t, e.path)
    elseif not t.folders then
        local s = state(t)
        s.cursor.value = e.path
        if t.multiple then
            local m = table.clone(g.untracked(function() return s.marked.value end))
            m[e.path] = true
            s.marked.value = m
        end
        choose(t)
    end
end

local M = {}

-- file_picker.key(view, name): Up, Down, Return, BackSpace (up a folder).
function M.key(t: Picker, name: string): boolean
    local s = state(t)
    local list = g.untracked(function() return s.shown.value end)
    local at: number? = nil
    local cur = g.untracked(function() return s.cursor.value end)
    for i, e in list do
        if e.path == cur then at = i end
    end
    if name == "Down" or name == "Up" then
        if #list == 0 then return true end
        local to = if at == nil then 1 elseif name == "Down" then math.min(at + 1, #list) else math.max(at - 1, 1)
        s.cursor.value = list[to].path
        return true
    end
    if name == "Return" or name == "KP_Enter" then
        if at then open_entry(t, list[at]) elseif t.folders then choose(t) end
        return true
    end
    if name == "BackSpace" and g.untracked(function() return s.query.value end) == "" then
        go(t, parent_of(g.untracked(function() return s.here.value end)))
        return true
    end
    if name == "Escape" and t.on_cancel then
        t.on_cancel()
        return true
    end
    return false
end

local function picker(t: Picker): any
    local s = state(t)
    local width = t.width or 640
    local height = t.height or 380
    local side = 150
    local look = if t.preview then 200 else 0

    -- The path as steps: each a button back to there.
    local steps = g.computed(function(): { { name: string, path: string } }
        local path = s.here.value
        local out = {}
        local from = 1
        if string.sub(path, 1, #HOME) == HOME then
            table.insert(out, { name = "Home", path = HOME })
            from = #HOME + 1
        else
            table.insert(out, { name = "/", path = "/" })
        end
        local walked = string.sub(path, 1, from - 1)
        for part in string.gmatch(string.sub(path, from), "[^/]+") do
            walked = (if walked == "/" then "" else walked) .. "/" .. part
            table.insert(out, { name = part, path = walked })
        end
        -- A deep path keeps its start and its last three steps; the steps
        -- between are one "…" back to the first of them left out.
        if #out > 5 then
            local short = { out[1], { name = "…", path = out[#out - 3].path } }
            for i = #out - 2, #out do table.insert(short, out[i]) end
            return short
        end
        return out
    end)
    local crumbs = g.each {
        gap = 2,
        max_width = width - 240,
        clip = true,
        items = function() return steps.value end,
        key = function(step) return step.path end,
        row = function(step, place)
            return g.box {
                gap = 2,
                g.text {
                    present = function() return place() > 1 end,
                    size = function() return theme.get("text_size") end,
                    color = function() return theme.get("faint") end,
                    text = "/",
                },
                button {
                    kind = "flat",
                    text = function() return step().name end,
                    on_click = function() go(t, g.untracked(step).path) end,
                },
            }
        end,
    }

    local filter = field {
        width = 180,
        placeholder = "Filter…",
        clearable = true,
        text = function() return s.query.value end,
        on_change = function(text: string)
            s.query.value = text
            local first = g.untracked(function() return s.shown.value end)[1]
            s.cursor.value = if first then first.path else nil
        end,
        on_submit = function() M.key(t, "Return") end,
        on_key = function(name: string) M.key(t, name) end,
    }

    local places = {}
    for _, p in user_places() do table.insert(places, p) end
    for _, p in t.places or {} do table.insert(places, { name = p.name, path = expand(p.path) }) end
    local place_rows = {}
    for _, p in places do
        table.insert(place_rows, g.capsule {
            role = "button",
            cursor = "pointer",
            height = function() return theme.get("control_height") - 4 end,
            padding_x = 10,
            justify = "start",
            radius = function() return theme.get("radius") - 2 end,
            fill = function()
                return if s.here.value == p.path then theme.get("selection") else theme.alpha(theme.get("control_hover"), 0)
            end,
            on_click = function() go(t, p.path) end,
            g.text {
                size = function() return theme.get("text_size") end,
                color = function() return if s.here.value == p.path then theme.get("text") else theme.get("faint") end,
                ellipsize = "end",
                max_width = side - 20,
                text = p.name,
            },
        })
    end

    local list = g.list {
        width = width - side - look - 24,
        height = height,
        row_height = g.untracked(function() return theme.get("control_height") end),
        items = function() return s.shown.value end,
        key = function(e) return e.path end,
        reveal = function()
            for i, e in s.shown.value do
                if e.path == s.cursor.value then return i end
            end
            return nil
        end,
        row = function(e, _)
            local function lit(): boolean return s.cursor.value == e().path end
            local function marked(): boolean return s.marked.value[e().path] == true end
            local hovered = g.signal(false)
            return g.capsule {
                role = "button",
                cursor = "pointer",
                padding_x = 10,
                gap = 10,
                justify = "start",
                radius = function() return theme.get("radius") - 2 end,
                fill = function()
                    if lit() or marked() then return theme.get("selection") end
                    if hovered.value then return theme.alpha(theme.get("control_hover"), 0.6) end
                    return theme.alpha(theme.get("control_hover"), 0)
                end,
                transition = { fill = "kit_quick" },
                on_hover = function(on: boolean) hovered.value = on end,
                on_click = function(b: number)
                    if b ~= 272 then return end
                    local entry = g.untracked(e)
                    if t.multiple and not entry.dir then
                        local m = table.clone(g.untracked(function() return s.marked.value end))
                        m[entry.path] = if m[entry.path] then nil else true
                        s.marked.value = m
                        s.cursor.value = entry.path
                    elseif g.untracked(lit) then
                        open_entry(t, entry)
                    else
                        s.cursor.value = entry.path
                    end
                end,
                g.icon {
                    width = 18,
                    height = 18,
                    present = function() return icon_for(kind_of(e())) ~= nil end,
                    icon = function() return icon_for(kind_of(e())) or "" end,
                },
                g.text {
                    grow = 1,
                    size = function() return theme.get("text_size") end,
                    color = function() return if lit() or marked() then theme.get("text") else theme.get("faint") end,
                    ellipsize = "middle",
                    max_width = width - side - look - 140,
                    text = function() return e().name end,
                },
                g.text {
                    size = function() return theme.get("text_size") - 2 end,
                    tabular = true,
                    color = function() return theme.get("faint") end,
                    text = function() return if e().dir then "" else size_text(e().size) end,
                },
            }
        end,
    }

    local function selected_picture(): string?
        local at = s.cursor.value
        if at == nil then return nil end
        return if KINDS[extension(at)] == "image" then at else nil
    end

    local node: { [any]: any } = {
        direction = "column",
        align = "stretch",
        gap = 10,
        width = width,
        g.box {
            gap = 6,
            justify = "between",
            g.box {
                gap = 4,
                button {
                    kind = "flat",
                    text = "↑",
                    name = "Up a folder",
                    on_click = function() go(t, parent_of(g.untracked(function() return s.here.value end))) end,
                },
                crumbs,
            },
            filter,
        },
        g.box {
            gap = 12,
            align = "start",
            g.box { direction = "column", align = "stretch", gap = 2, width = side, table.unpack(place_rows) },
            g.box {
                align = "center",
                justify = "center",
                width = width - side - look - 24,
                height = height,
                present = function() return #s.shown.value == 0 end,
                g.text {
                    size = function() return theme.get("text_size") end,
                    color = function() return theme.get("faint") end,
                    text = function() return if s.query.value ~= "" then "Nothing matches" else "Empty folder" end,
                },
            },
            g.box { present = function() return #s.shown.value > 0 end, list },
            g.box {
                present = t.preview == true,
                width = look,
                height = height,
                justify = "center",
                g.image {
                    present = function() return selected_picture() ~= nil end,
                    width = look,
                    height = look,
                    fit = "contain",
                    src = function() return selected_picture() or "" end,
                },
            },
        },
        g.box {
            justify = "between",
            gap = 10,
            g.text {
                size = function() return theme.get("text_size") - 1 end,
                color = function() return theme.get("faint") end,
                ellipsize = "middle",
                max_width = width - 220,
                text = function()
                    local _ = s.marked.value, s.cursor.value, s.here.value
                    local paths = chosen(t)
                    if #paths == 0 then return "" end
                    if #paths == 1 then return paths[1] end
                    return `{#paths} chosen`
                end,
            },
            g.box {
                gap = 8,
                button { text = "Cancel", on_click = function() if t.on_cancel then t.on_cancel() end end },
                button {
                    text = if t.folders then "Choose folder" else "Choose",
                    kind = "accent",
                    disabled = function()
                        local _ = s.marked.value, s.cursor.value, s.here.value
                        return #chosen(t) == 0
                    end,
                    on_click = function() choose(t) end,
                },
            },
        },
    }
    local own = {
        folder = true, on_choose = true, on_cancel = true, multiple = true, folders = true, filter = true,
        hidden = true, preview = true, places = true, width = true, height = true,
    }
    for k, v in t do
        if type(k) == "string" and node[k] == nil and not (own :: any)[k] then node[k] = v end
    end
    return g.box(node)
end

setmetatable(M, { __call = function(_, t: Picker): any return picker(t) end })

return M
