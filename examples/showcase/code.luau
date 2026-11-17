--!strict
-- A slide's Luau, coloured as an editor would: keywords, strings, numbers,
-- comments, the calls that make nodes, and the names before `=`. The text
-- is one `g.text`; the colours are its `runs`.
local style = require("style")

local M = {}

local KEYWORDS = {}
for _, w in { "local", "function", "return", "if", "then", "else", "elseif", "end", "for", "in", "do", "while",
    "and", "or", "not", "nil", "true", "false", "repeat", "until", "break" } do
    KEYWORDS[w] = true
end

-- { from, to, kind } for each stretch worth a colour, characters from 1.
local function tokens(src: string): { { any } }
    local out = {}
    local i, n = 1, #src
    while i <= n do
        local c = string.sub(src, i, i)
        if string.sub(src, i, i + 1) == "--" then
            local j = string.find(src, "\n", i, true) or (n + 1)
            table.insert(out, { i, j - 1, "comment" })
            i = j
        elseif c == '"' or c == "'" or c == "`" then
            local j = i + 1
            while j <= n and string.sub(src, j, j) ~= c do
                if string.sub(src, j, j) == "\\" then j += 1 end
                j += 1
            end
            table.insert(out, { i, math.min(j, n), "string" })
            i = j + 1
        elseif string.match(c, "%d") then
            local j = i
            while j + 1 <= n and string.match(string.sub(src, j + 1, j + 1), "[%w%.]") do j += 1 end
            table.insert(out, { i, j, "number" })
            i = j + 1
        elseif string.match(c, "[%a_]") then
            local j = i
            while j + 1 <= n and string.match(string.sub(src, j + 1, j + 1), "[%w_%.]") do j += 1 end
            local word = string.sub(src, i, j)
            local after = string.match(string.sub(src, j + 1), "^%s*(.)")
            if KEYWORDS[word] then
                table.insert(out, { i, j, "keyword" })
            elseif after == "{" or after == "(" then
                table.insert(out, { i, j, "call" })
            elseif after == "=" and string.sub(src, j + 1, j + 3) ~= " ==" and string.match(string.sub(src, j + 1), "^%s*==") == nil then
                table.insert(out, { i, j, "key" })
            end
            i = j + 1
        else
            i += 1
        end
    end
    return out
end

local COLOURS = {
    comment = function() return style.t.colour("faint") end,
    string = function() return "#E8C47A" end,
    number = function() return "#F08A6C" end,
    keyword = function() return style.t.colour("accent2") end,
    call = function() return style.t.colour("accent") end,
    key = function() return style.t.colour("muted") end,
}

function M.runs(src: string): { any }
    local out = {}
    for _, tk in tokens(src) do
        local run: { [string]: any } = { from = tk[1], to = tk[2], color = COLOURS[tk[3]]() }
        if tk[3] == "comment" then run.italic = true end
        table.insert(out, run)
    end
    return out
end

-- The code, as a text node: the slide's source, its colours.
function M.view(src: string): any
    return style.text("code", {
        text = src,
        runs = M.runs(src),
        wrap = true,
        max_width = function() return style.u(560) end,
        color = style.c("text"),
    })
end

return M
