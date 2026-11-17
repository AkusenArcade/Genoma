--!strict
-- The machine, drawn as it runs: the processor's load as an area with its
-- history, the memory as a line, the sound as bars (when something plays;
-- otherwise a stand-in so the bars have something to say), each a single
-- node however many points it holds.
local g = require("@genoma")
local style = require("style")
local frame = require("frame")
local chart = require("@genoma/kit/chart")
local vitals = require("@genoma/vitals")
local spectrum = require("@genoma/spectrum")

local u = style.u
local cap = style.cap
local t = style.t

-- A point a frame at 120 a second: the history is five seconds, and it
-- scrolls a few pixels a frame rather than jumping.
local POINTS = 600
local RATE = 120

return {
    key = "charts",
    kicker = "Live data",
    title = "Charts that breathe",
    caption = "Lines, areas and bars drawn on the GPU in one node, however many points: fed by @genoma/vitals and @genoma/spectrum, the machine's own readings, as they change.",
    duration = 11,
    code = [[
local vitals = require("@genoma/vitals")
local chart = require("@genoma/kit/chart")

local history = g.signal({})
g.timer { every = 0.25, on_tick = function()
    local h = table.clone(history.value)
    table.insert(h, vitals.cpu * 100)
    history.value = h
end }

chart.area {
    width = 640, height = 260,
    values = function() return history.value end,
    min = 0, max = 100,
    color = "#b9de6b", fade = 0.85,
}]],

    build = function(ctx: { [string]: any }): any
        -- An idle machine draws a flat line, so for the film the load is
        -- acted: a processor that wanders between quiet and busy with now
        -- and then a burst, and a memory that drifts round what the
        -- machine really holds. A small generator of its own, so the same
        -- seed gives the same film.
        local state = 12345
        local function random(): number
            state = (state * 1103515245 + 12345) % 2147483648
            return state / 2147483648
        end
        -- Per point: a target held for a second or two, approached on a
        -- critically damped spring, a little tremor on top.
        local load, speed, aim, held, tremor = 18, 0, 30, 0, 0
        local function next_cpu(): number
            held -= 1
            if held <= 0 then
                aim = if random() < 0.18 then 70 + random() * 25 else 8 + random() * 45
                held = 100 + math.floor(random() * 180)
            end
            speed += ((aim - load) * 0.001 - speed * 0.06)
            load += speed
            tremor += ((random() - 0.5) * 1.6 - tremor) * 0.075
            return math.clamp(load + tremor, 1, 99)
        end
        local base = vitals.ram * 100
        local drift, tick = 0, 0
        local function next_ram(): number
            tick += 1
            drift = math.clamp(drift + (random() - 0.5) * 0.056, -3, 3)
            return math.clamp(base + drift + math.sin(tick / 320) * 1.2, 1, 99)
        end
        local function history(next: () -> number): { number }
            local out = {}
            for _ = 1, POINTS do table.insert(out, next()) end
            return out
        end
        local cpu = g.signal(history(next_cpu))
        local ram = g.signal(history(next_ram))
        local function last(list: { number }): number return list[#list] or 0 end
        -- The figures change four times a second, to stay readable.
        local shown = g.signal({ last(cpu.value), last(ram.value) })
        g.timer { every = 0.25, on_tick = function()
            shown.value = { last(g.untracked(function() return cpu.value end)), last(g.untracked(function() return ram.value end)) }
        end }
        local phase = g.signal(0)
        g.timer { every = 1 / RATE, on_tick = function()
            local c = table.clone(cpu.value)
            table.insert(c, next_cpu())
            if #c > POINTS then table.remove(c, 1) end
            cpu.value = c
            local r = table.clone(ram.value)
            table.insert(r, next_ram())
            if #r > POINTS then table.remove(r, 1) end
            ram.value = r
        end }
        g.timer { every = 1 / 30, on_tick = function() phase.value += 1 / 30 end }

        local hold = spectrum:hold()
        g.cleanup(function() hold:release() end)
        local function bands(): { number }
            local left = spectrum.left
            if #left > 0 and spectrum.level > 0.02 then return left end
            -- Nothing playing: a stand-in that moves like music.
            local out = {}
            local p = phase.value
            for i = 1, 64 do
                local x = i / 64
                local v = 0.55 * math.exp(-x * 2.2) + 0.25 * (0.5 + 0.5 * math.sin(p * 3.1 + i * 0.45)) * (1 - x * 0.6)
                    + 0.12 * (0.5 + 0.5 * math.sin(p * 7.3 - i * 1.3))
                table.insert(out, math.clamp(v, 0, 1))
            end
            return out
        end

        local function big(label: string, value: () -> string, colour: () -> string): any
            return g.box {
                direction = "column",
                gap = function() return u(4) end,
                style.text("label", { text = label, color = style.c("muted") }),
                style.text("number", { text = value, color = colour }),
            }
        end

        return frame.new {
            number = ctx.number,
            kicker = "Live data",
            title = "Charts that breathe",
            caption = "Lines, areas and bars drawn on the GPU in one node, however many points: fed by @genoma/vitals and @genoma/spectrum, the machine's own readings, as they change.",
            arrived = ctx.arrived,
            demo = g.box {
                width = function() return u(1000) end,
                height = function() return u(800) end,
                g.box {
                    overlay = true,
                    attach = "corner",
                    offset_x = function() return u(10) end,
                    offset_y = function() return u(10) end,
                    direction = "row",
                    gap = function() return u(60) end,
                    big("PROCESSOR", function() return string.format("%d%%", math.floor(shown.value[1] + 0.5)) end, style.c("glow")),
                    big("MEMORY", function() return string.format("%d%%", math.floor(shown.value[2] + 0.5)) end, style.c("accent2")),
                    big("CLOCK", function() return string.format("%d%%", math.floor(vitals.cpu_clock * 100 + 0.5)) end, style.c("text")),
                },
                frame.tile(0, 120, 1000, 300, "PROCESSOR  ·  AREA", function(w, h)
                    return chart.area {
                        width = w,
                        height = h,
                        values = function() return cpu.value end,
                        min = 0,
                        max = 100,
                        color = t.colour("glow"),
                        fill = g.colour.alpha(t.colour("accent"), 0.45),
                        fade = 0.9,
                        line = 3,
                    }
                end),
                frame.tile(0, 440, 480, 330, "MEMORY  ·  LINE", function(w, h)
                    return chart.line {
                        width = w,
                        height = h,
                        values = function() return ram.value end,
                        min = base - 6,
                        max = base + 6,
                        color = t.colour("accent2"),
                        line = 3,
                    }
                end),
                frame.tile(500, 440, 500, 330, "SOUND  ·  64 BANDS", function(w, h)
                    return g.box {
                        width = w,
                        height = h,
                        padding = function() return u(20) end,
                        chart.bars {
                            width = function() return w() - u(40) end,
                            height = function() return h() - u(70) end,
                            values = bands,
                            min = 0,
                            max = 1,
                            color = "#E05E7A",
                            gap = 0.3,
                            radius = 3,
                        },
                    }
                end),
            },
        }
    end,
}
