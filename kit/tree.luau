--!strict
-- A tree: items that hold items, a branch opened by its chevron, only the
-- rows in view mounted (g.list), however many there are.
--
--   local tree = require("@genoma/kit/tree")
--   local view = {
--       height = 360,
--       roots = function() return { home } end,
--       children = function(dir) return dir.entries end,   -- nil for a leaf
--       key = function(dir) return dir.path end,
--       label = function(dir) return dir.name end,
--       icon = function(dir) return if dir.entries then "folder" else "text-x-generic" end,
--       selected = function() return chosen.value end,
--       on_select = function(key) chosen.value = key end,
--       on_activate = function(key, item) open(item) end,
--   }
--   tree(view)
--   -- in the shell's on_key: if tree.key(view, name) then return end
--
-- An item is a branch when children(item) gives a list, even an empty one;
-- children is asked only once a branch is open, inside a binding, so a
-- branch can fill as its entries arrive (give an empty list meanwhile, or
-- `branch(item)` to say it is one before). expanded: a set of open keys,
-- or a function giving it; without it the tree keeps its own, and
-- on_toggle(key, open) is told either way. selected is a key, not a place:
-- a branch opening above it does not move the choice. A click selects a
-- row, a click on the selected one activates it (a branch opens or closes
-- when nobody listens), the chevron opens and closes.
--
-- tree.key(view, name) moves the choice with the keyboard: Up and Down,
-- Left closes a branch or goes to its parent, Right opens one or goes to
-- its first child, Return activates; true when it used the key.
local g = require("@genoma")
local theme = require("@genoma/kit/theme")

local read = theme.read

export type Tree = {
    roots: () -> { any },
    children: (item: any) -> { any }?,
    branch: ((item: any) -> boolean)?,
    key: ((item: any) -> string | number)?,
    label: (item: any) -> any,
    detail: ((item: any) -> any)?,
    icon: ((item: any) -> string?)?,
    expanded: ({ [any]: boolean } | () -> { [any]: boolean })?,
    on_toggle: ((key: any, open: boolean) -> ())?,
    selected: (any | () -> any)?,
    on_select: ((key: any, item: any) -> ())?,
    on_activate: ((key: any, item: any) -> ())?,
    row_height: number?,
    indent: number?,
    [any]: any,
}

type Row = { item: any, key: any, depth: number, branch: boolean, open: boolean, parent: any }

-- Each tree's own state, by its table: the open set it keeps when the
-- shell keeps none, and its rows as last laid out.
local states: { [any]: { open: any, rows: any } } = setmetatable({}, { __mode = "k" }) :: any

local function state(t: Tree): { open: any, rows: any }
    local s = states[t]
    if s then return s end
    local own = g.lasting(function() return g.signal({} :: { [any]: boolean }) end)
    local new = {
        open = own,
        rows = g.lasting(function()
            return g.computed(function(): { Row }
                local key = t.key or t.label
                local open = if t.expanded ~= nil then read(t.expanded) else own.value
                local out: { Row } = {}
                local function walk(items: { any }, depth: number, parent: any)
                    for _, item in items do
                        local k = key(item)
                        local is_branch = if t.branch then t.branch(item) else nil
                        local is_open = open[k] == true
                        local kids = if is_branch == false then nil
                            elseif is_open or is_branch == nil then t.children(item)
                            else nil
                        local branch = if is_branch ~= nil then is_branch else kids ~= nil
                        table.insert(out, { item = item, key = k, depth = depth, branch = branch, open = branch and is_open, parent = parent })
                        if branch and is_open and kids then walk(kids, depth + 1, k) end
                    end
                end
                walk(t.roots(), 0, nil)
                return out
            end)
        end),
    }
    states[t] = new
    return new
end

local function find(rows: { Row }, key: any): number?
    for i, r in rows do
        if r.key == key then return i end
    end
    return nil
end

local function toggle(t: Tree, row: Row, open: boolean)
    local s = state(t)
    if t.expanded == nil then
        local next = table.clone(g.untracked(function() return s.open.value end))
        next[row.key] = if open then true else nil
        s.open.value = next
    end
    if t.on_toggle then t.on_toggle(row.key, open) end
end

local function choose(t: Tree, row: Row)
    if t.on_select then t.on_select(row.key, row.item) end
end

local function activate(t: Tree, row: Row)
    if t.on_activate then
        t.on_activate(row.key, row.item)
    elseif row.branch then
        toggle(t, row, not row.open)
    end
end

local M = {}

-- tree.key(view, name): the keyboard's way through it.
function M.key(t: Tree, name: string): boolean
    local rows: { Row } = g.untracked(function() return state(t).rows.value end)
    if #rows == 0 then return false end
    local at = find(rows, g.untracked(function() return read(t.selected) end))
    local row = if at then rows[at] else nil
    if name == "Down" or name == "Up" then
        local to = if at == nil then 1 elseif name == "Down" then math.min(at + 1, #rows) else math.max(at - 1, 1)
        choose(t, rows[to])
        return true
    end
    if not row then return false end
    if name == "Right" then
        if row.branch and not row.open then
            toggle(t, row, true)
        elseif row.branch and rows[at :: number + 1] and rows[at :: number + 1].depth > row.depth then
            choose(t, rows[at :: number + 1])
        end
        return true
    end
    if name == "Left" then
        if row.branch and row.open then
            toggle(t, row, false)
        elseif row.parent ~= nil then
            local up = find(rows, row.parent)
            if up then choose(t, rows[up]) end
        end
        return true
    end
    if name == "Return" or name == "KP_Enter" or name == "space" then
        activate(t, row)
        return true
    end
    return false
end

local function chevron(open: () -> boolean): any
    return g.path {
        width = 10,
        height = 10,
        -- Pointing right, or down once open.
        d = function() return if open() then "M1 3 L5 7 L9 3" else "M3 1 L7 5 L3 9" end,
        color = function() return theme.get("faint") end,
        stroke = 1.6,
        cap = "round",
        join = "round",
    }
end

setmetatable(M, {
    __call = function(_, t: Tree): any
        local s = state(t)
        local row_height = t.row_height or g.untracked(function() return theme.get("control_height") end)
        local indent = t.indent or 16
        local function selected(): any return read(t.selected) end
        local node: { [any]: any } = {
            row_height = row_height,
            items = function() return s.rows.value end,
            key = function(r: Row) return r.key end,
            reveal = function() return find(s.rows.value, selected()) end,
            row = function(r: () -> Row, _: () -> number)
                local function lit(): boolean return selected() == r().key end
                local hovered = g.signal(false)
                return g.capsule {
                    role = "button",
                    cursor = "pointer",
                    padding_left = function() return 6 + r().depth * indent end,
                    padding_right = 10,
                    gap = 6,
                    justify = "start",
                    radius = function() return theme.get("radius") - 2 end,
                    fill = function()
                        if lit() then return theme.get("selection") end
                        if hovered.value then return theme.alpha(theme.get("control_hover"), 0.6) end
                        return theme.alpha(theme.get("control_hover"), 0)
                    end,
                    transition = { fill = "kit_quick" },
                    on_hover = function(entered: boolean) hovered.value = entered end,
                    on_click = function(b: number)
                        if b ~= 272 then return end
                        local row = g.untracked(r)
                        if g.untracked(selected) == row.key then activate(t, row) else choose(t, row) end
                    end,
                    -- The chevron, or its room on a leaf, so labels line up.
                    g.box {
                        width = 14,
                        height = 14,
                        justify = "center",
                        role = "button",
                        on_click = function(b: number)
                            local row = g.untracked(r)
                            if b == 272 and row.branch then toggle(t, row, not row.open) end
                        end,
                        g.box { present = function() return r().branch end, chevron(function() return r().open end) },
                    },
                    g.icon {
                        present = function() return t.icon ~= nil and t.icon(r().item) ~= nil end,
                        width = 16,
                        height = 16,
                        icon = function() return if t.icon then t.icon(r().item) or "" else "" end,
                        color = function() return if lit() then theme.get("text") else theme.get("faint") end,
                    },
                    g.text {
                        grow = 1,
                        size = function() return theme.get("text_size") end,
                        color = function() return if lit() then theme.get("text") else theme.get("faint") end,
                        ellipsize = "end",
                        text = function() return tostring(t.label(r().item)) end,
                    },
                    g.text {
                        present = t.detail ~= nil,
                        size = function() return theme.get("text_size") - 2 end,
                        color = function() return theme.get("faint") end,
                        text = function() return if t.detail then tostring(t.detail(r().item) or "") else "" end,
                    },
                }
            end,
        }
        local own = {
            roots = true, children = true, branch = true, key = true, label = true, detail = true, icon = true,
            expanded = true, on_toggle = true, selected = true, on_select = true, on_activate = true, indent = true,
        }
        for k, v in t do
            if type(k) == "string" and node[k] == nil and not (own :: any)[k] then node[k] = v end
        end
        return g.list(node)
    end,
})

return M
