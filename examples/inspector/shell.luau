--!strict
-- An inspector for another Genoma interface: its surfaces, its tree of
-- nodes as they are now, and the chosen node's properties, outlined on
-- screen in magenta where it is drawn.
--
--   GENOMA_IPC=inspector GENOMA_INSPECT=genoma genoma examples/inspector
--
-- GENOMA_INSPECT names the interface to inspect: its GENOMA_IPC, "genoma"
-- unless it was started with another. The tree is asked for once a second
-- (`genoma ipc <name> genoma tree`); Pause holds it still.
local g = require("@genoma")
local kit = require("@genoma/kit")

local theme = kit.theme
local target = g.env("GENOMA_INSPECT") or "genoma"

type Node = {
    node: number,
    parent: number?,
    depth: number,
    kind: string,
    role: string,
    name: string?,
    id: string?,
    text: string?,
    bounds: { number },
    present: boolean,
    props: { { string } },
}
type Surface = {
    surface: number,
    namespace: string?,
    output: string?,
    size: { number },
    kind: string,
    asleep: boolean,
    nodes: { Node },
}

local surfaces = g.signal({} :: { Surface })
local failure = g.signal("")
local paused = g.signal(false)
local chosen_surface = g.signal(1)
local chosen = g.signal(nil :: number?)

local function ask(words: { string }, on_reply: ((string) -> ())?)
    -- The instance is the environment's; the first word the target, Genoma's own.
    local command = { "genoma", "ipc", "genoma" }
    for _, w in words do table.insert(command, w) end
    local out = {}
    local errors = {}
    g.process {
        command = command,
        env = { GENOMA_IPC = target },
        on_line = function(line: string) table.insert(out, line) end,
        on_error_line = function(line: string) table.insert(errors, line) end,
        on_exit = function(code: number)
            if code ~= 0 then
                failure.value = if #errors > 0 then errors[1] else `genoma ipc exited with {code}`
                return
            end
            failure.value = ""
            if on_reply then on_reply(table.concat(out, "\n")) end
        end,
    }
end

local function refresh()
    if g.untracked(function() return paused.value end) then return end
    ask({ "tree" }, function(reply: string)
        local ok, parsed = pcall(g.parse_json, reply)
        if ok and type(parsed) == "table" then surfaces.value = parsed :: any end
    end)
end
refresh()
g.timer { every = 1, on_tick = refresh }

local function surface(): Surface?
    return surfaces.value[chosen_surface.value]
end

local function nodes(): { Node }
    local s = surface()
    return if s then s.nodes else {}
end

-- The chosen node's place in the list, for the list to keep it in view.
local function place_of(id: number?): number?
    if not id then return nil end
    for i, n in nodes() do
        if n.node == id then return i end
    end
    return nil
end

local function chosen_node(): Node?
    local at = place_of(chosen.value)
    return if at then nodes()[at] else nil
end

-- Outlined in the inspected interface whenever the choice changes.
g.watch(function() return { chosen.value, chosen_surface.value } end, function(pair: any)
    local s = g.untracked(surface)
    if pair[1] and s then
        ask({ "highlight", tostring(s.surface), tostring(pair[1]) })
    else
        ask({ "highlight", "none" })
    end
end)

local function label(n: Node): string
    local what = n.kind
    if n.id then what ..= " #" .. n.id end
    if n.name then what ..= ` "{n.name}"` end
    if n.text and n.text ~= "" then what ..= "  " .. string.sub(n.text, 1, 40) end
    -- Absent: not drawn, its subtree with it. Said first, so a long label
    -- does not end it out of sight.
    return string.rep("   ", n.depth) .. (if n.present then "" else "(absent) ") .. what
end

local function faint(text: string | () -> string, size: number?): any
    return g.text { size = size or 12, wrap = true, max_width = 390, color = function() return theme.get("faint") end, text = text }
end

return g.surface {
    anchor = { "top", "right", "bottom" },
    margin = { top = 12, right = 12, bottom = 12 },
    layer = "overlay",
    keyboard = "on_demand",
    namespace = "genoma-inspector",

    g.capsule {
        direction = "column",
        align = "stretch",
        gap = 12,
        padding = 14,
        width = 440,
        grow = 1,
        radius = 14,
        fill = function() return theme.get("background") end,
        rim_top = function() return theme.get("line") end,
        rim_bottom = function() return theme.get("line") end,

        g.box {
            gap = 10,
            justify = "between",
            g.text { size = 15, weight = 600, color = function() return theme.get("text") end, text = `Inspecting "{target}"` },
            kit.toggle {
                name = "Pause",
                on = function() return paused.value end,
                on_toggle = function(on: boolean) paused.value = on end,
            },
        },
        faint(function()
            if failure.value ~= "" then return failure.value end
            return if paused.value then "Paused." else "Asked once a second."
        end),

        kit.dropdown {
            width = 412,
            options = function()
                local names = {}
                for i, s in surfaces.value do
                    local size = `{math.floor(s.size[1])}×{math.floor(s.size[2])}`
                    names[i] = `{s.surface} · {s.namespace or "?"} · {s.kind} {size}{if s.asleep then " · asleep" else ""}`
                end
                return names
            end,
            selected = function() return chosen_surface.value end,
            on_select = function(i: number)
                chosen_surface.value = i
                chosen.value = nil
            end,
        },

        kit.list {
            grow = 1,
            height = 360,
            row_height = 26,
            label_width = 330,
            items = nodes,
            key = function(n: Node) return n.node end,
            label = label,
            detail = function(n: Node) return `{math.floor(n.bounds[3])}×{math.floor(n.bounds[4])}` end,
            selected = function() return place_of(chosen.value) end,
            on_select = function(i: number) chosen.value = nodes()[i].node end,
            on_activate = function() chosen.value = nil end,
        },

        g.capsule {
            direction = "column",
            align = "stretch",
            gap = 4,
            padding = 10,
            radius = 8,
            fill = function() return theme.get("control") end,
            rim_top = "#00000000",
            rim_bottom = "#00000000",
            faint(function()
                local n = chosen_node()
                if not n then return "Choose a node: it is outlined where it is drawn. Choose it again to let it go." end
                local b = n.bounds
                return `{n.kind} {n.node}, {n.role} · at {math.floor(b[1])}, {math.floor(b[2])} · {math.floor(b[3])}×{math.floor(b[4])}`
            end),
            g.each {
                direction = "column",
                align = "stretch",
                gap = 2,
                items = function()
                    local n = chosen_node()
                    return if n then n.props else {}
                end,
                key = function(p: { string }) return p[1] end,
                row = function(p: () -> { string })
                    return g.box {
                        justify = "between",
                        g.text { size = 12, color = function() return theme.get("text") end, text = function() return p()[1] end },
                        g.text { size = 12, tabular = true, color = function() return theme.get("faint") end, text = function() return p()[2] end },
                    }
                end,
            },
        },
    },
}
