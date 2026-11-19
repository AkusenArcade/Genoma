--!strict
-- Genoma, shown by itself: a presentation of what the toolkit draws, moves
-- and knows, one slide at a time, each slide drawn by what it is about.
--
--   genoma examples/showcase
--
-- A window like any program's: the compositor places it; F11, or the
-- compositor's own fullscreen (niri's Mod+Shift+F), gives it the whole
-- screen, and either takes it back.
--
-- → space or a click: the next slide; ← or the right button: the one
-- before; a swipe either way. Digits go to a slide by its number. C shows
-- the Luau that draws the slide, A plays the slides by themselves, Escape
-- ends it. Drawn for 1920 × 1080; any other size gets the same picture,
-- scaled and letterboxed.
local g = require("@genoma")
local style = require("style")
local code = require("code")

local u = style.u
local t = style.t

-- ---- The slides ---------------------------------------------------------------

local SLIDES = {
    require("slides/01_title"),
    require("slides/02_reactivity"),
    require("slides/03_shapes"),
    require("slides/04_gradients"),
    require("slides/05_fusion"),
    require("slides/06_paths"),
    require("slides/07_type"),
    require("slides/08_motion"),
    require("slides/09_depth"),
    require("slides/10_glass"),
    require("slides/11_masks"),
    require("slides/12_shaders"),
    require("slides/13_particles"),
    require("slides/14_charts"),
    require("slides/15_layout"),
    require("slides/16_kit"),
    require("slides/17_services"),
    require("slides/18_closing"),
}

-- ---- Where the deck is --------------------------------------------------------

local current = g.state("showcase.slide", 1)
current.value = math.clamp(current.value, 1, #SLIDES)
local leaving = g.signal(0)
-- Whether the slide wanted is built yet: it is built only once the one
-- before has gone, so the frame that builds it (its nodes, its text, its
-- outlines) falls on an empty stage, where it cannot be seen to stall.
local ready = g.signal(true)
local mount_timer: any = nil
local direction = g.signal(1)
local code_open = g.state("showcase.code", false)
local auto = g.signal(false)

local leave_timer: any = nil
local auto_timer: any = nil

local go: (n: number) -> ()

-- A slide with steps of its own takes → and ← while it has one left (the
-- last slide's ending): what it registered, by slide number, answering
-- whether it took the key.
local steps: { [number]: { next: (() -> boolean)?, previous: (() -> boolean)?, waiting: (() -> boolean)? } } = {}
local next_slide: () -> ()

local function schedule()
    if auto_timer then auto_timer:stop() end
    auto_timer = nil
    if not g.untracked(function() return auto.value end) then return end
    local n = g.untracked(function() return current.value end)
    auto_timer = g.after(SLIDES[n].duration or 10, function()
        auto_timer = nil
        -- A step of the slide's own first (the last one's ending); then it
        -- plays on for its duration again.
        local own = steps[n] and steps[n].next
        if own and own() then
            schedule()
            return
        end
        if n >= #SLIDES then
            auto.value = false
            return
        end
        go(n + 1)
    end)
end

go = function(n: number)
    n = math.clamp(n, 1, #SLIDES)
    local now = g.untracked(function() return current.value end)
    if n == now then return end
    g.batch(function()
        direction.value = if n > now then 1 else -1
        leaving.value = now
        current.value = n
        ready.value = false
    end)
    if mount_timer then mount_timer:stop() end
    mount_timer = g.after(t.seconds("slide_out"), function()
        mount_timer = nil
        ready.value = true
    end)
    -- In the log, for reading a recording's frame times back by slide.
    print(string.format("showcase: slide %d (%s)", n, SLIDES[n].key))
    if leave_timer then leave_timer:stop() end
    leave_timer = g.after(t.seconds("slide_out") + 0.05, function()
        leave_timer = nil
        leaving.value = 0
    end)
    schedule()
end

next_slide = function()
    local n = g.untracked(function() return current.value end)
    local own = steps[n] and steps[n].next
    if own and own() then return end
    go(n + 1)
end
local function previous_slide()
    local n = g.untracked(function() return current.value end)
    local own = steps[n] and steps[n].previous
    if own and own() then return end
    go(n - 1)
end

-- Digits typed close together make one number: 1 then 2 is slide 12.
local typed = ""
local typed_timer: any = nil
local function digit(d: string)
    typed ..= d
    if typed_timer then typed_timer:stop() end
    local function done()
        typed_timer = nil
        local n = tonumber(typed)
        typed = ""
        if n and n >= 1 then go(n) end
    end
    if #typed >= 2 or tonumber(typed .. "0") > #SLIDES then
        done()
    else
        typed_timer = g.after(0.6, done)
    end
end

-- On the whole screen: asked with F11, and following the compositor when
-- its own key puts it there or takes it off.
local whole = g.signal(false)

local function key(name: string, text: string?)
    if name == "Escape" then
        g.quit()
    elseif name == "F11" then
        whole.value = not g.untracked(function() return whole.value end)
    elseif name == "Right" or name == "space" or name == "Next" or name == "Return" or name == "Down" then
        next_slide()
    elseif name == "Left" or name == "BackSpace" or name == "Prior" or name == "Up" then
        previous_slide()
    elseif name == "Home" then
        go(1)
    elseif name == "End" then
        go(#SLIDES)
    elseif text == "c" or text == "C" then
        code_open.value = not g.untracked(function() return code_open.value end)
    elseif text == "a" or text == "A" then
        auto.value = not g.untracked(function() return auto.value end)
        -- A slide waiting to be started (the cover's START) starts now.
        local n = g.untracked(function() return current.value end)
        local own = steps[n]
        if auto.value and own and own.waiting and own.waiting() and own.next then own.next() end
        schedule()
    elseif text and string.match(text, "^%d$") then
        digit(text)
    end
end

g.ipc("showcase", {
    go = function(n: string) go(tonumber(n) or 1) return tostring(g.untracked(function() return current.value end)) end,
    next = function() next_slide() return "" end,
    previous = function() previous_slide() return "" end,
    code = function() code_open.value = not g.untracked(function() return code_open.value end) return "" end,
    auto = function() auto.value = not g.untracked(function() return auto.value end) schedule() return "" end,
})

-- ---- A slide on the stage ------------------------------------------------------------
--
-- The slide leaving slides on and blurs away first; the one arriving is
-- built once it has gone, and comes in from the side it is going toward,
-- out of a blur. No depth: a rounded clip does not follow a node
-- under it, and the mark's strands would leave their capsule.

local function slide_node(index: () -> number): any
    local n = g.untracked(index)
    local arrived = g.signal(false)
    -- Built on the frame before, it comes in from the next.
    g.after(0.05, function() arrived.value = true end)
    local function here(): boolean return current.value == n end
    local function state(): string
        if not here() then return "gone" end
        return if arrived.value then "in" else "before"
    end
    local function timing(): string return if state() == "gone" then "slide_out" else "slide_in" end
    local content = SLIDES[n].build {
        number = n,
        arrived = function() return arrived.value and here() end,
        -- t.next and t.previous: functions that take the key, answering
        -- whether they did; t.waiting: whether it waits to be started.
        steps = function(t: { [string]: any })
            steps[n] = t
            g.cleanup(function() if steps[n] == t then steps[n] = nil end end)
        end,
    }
    return g.box {
        overlay = true,
        attach = "corner",
        width = function() return u(style.W) end,
        height = function() return u(style.H) end,
        -- Waiting to come in it is drawn once all but invisible, on the
        -- empty stage: its shaders' pipelines and its pictures are made
        -- then, not on the frame it shows (a node at 0 is not drawn).
        opacity = function()
            local s = state()
            return if s == "in" then 1 elseif s == "before" then 0.002 else 0
        end,
        offset_x = function()
            local s = state()
            if s == "in" then return 0 end
            local d = direction.value
            return if s == "before" then d * u(160) else -d * u(160)
        end,
        blur_radius = function() return if state() == "in" then 0 else u(10) end,
        transition = { opacity = timing, offset_x = timing, blur_radius = timing },
        content,
    }
end

local function shown(): { number }
    local out = {}
    if leaving.value ~= 0 and leaving.value ~= current.value then table.insert(out, leaving.value) end
    if ready.value then table.insert(out, current.value) end
    return out
end

-- ---- What stays on the stage -----------------------------------------------------

local CLEAR = "#00000000"

local function rgba(name: string): { number }
    local c = g.colour.parse(t.colour(name))
    return { c.r, c.g, c.b, c.a }
end

local background = g.shader {
    overlay = true,
    attach = "corner",
    width = function() return u(style.W) end,
    height = function() return u(style.H) end,
    src = g.dir .. "/background.wgsl",
    clock = true,
    fps = 30,
    values = function()
        local out = {}
        for _, name in { "background", "accent", "accent2" } do
            for _, v in rgba(name) do table.insert(out, v) end
        end
        -- The first light sits behind the mark on the title, then moves
        -- toward the middle.
        table.insert(out, if current.value == 1 then 0.3 else 0.62)
        table.insert(out, 1)
        return out
    end,
    transition = { values = "slide_in" },
}

-- Along the foot: how far through the deck, and which slide.
local function chrome_shown(): boolean return current.value ~= 1 and not style.quiet.value end
local chrome = g.box {
    overlay = true,
    attach = "corner",
    offset_x = function() return u(120) end,
    offset_y = function() return u(style.H - 70) end,
    width = function() return u(style.W - 240) end,
    height = function() return u(20) end,
    opacity = function() return if chrome_shown() then 1 else 0 end,
    transition = { opacity = "fade" },
    direction = "row",
    align = "center",
    gap = function() return u(28) end,
    style.text("label", {
        text = function() return if auto.value then "AUTO" else "" end,
        color = style.c("accent"),
    }),
    g.capsule {
        grow = 1,
        height = function() return u(2) end,
        radius = 0,
        fill = style.c("line"),
        rim_top = CLEAR,
        rim_bottom = CLEAR,
        g.capsule {
            overlay = true,
            attach = "corner",
            width = function() return u(style.W - 240 - 140) * current.value / #SLIDES end,
            height = function() return u(2) end,
            radius = 0,
            fill_stops = style.accent_stops,
            fill_across = true,
            rim_top = CLEAR,
            rim_bottom = CLEAR,
            transition = { width = "slide_in" },
        },
    },
    style.text("label", {
        text = function() return string.format("%02d / %02d", current.value, #SLIDES) end,
        color = style.c("muted"),
    }),
}

-- The Luau that draws the slide, on glass at the right.
local code_panel = g.capsule {
    overlay = true,
    attach = "corner",
    offset_x = function() return if code_open.value then u(style.W - 680) else u(style.W + 40) end,
    offset_y = function() return u(90) end,
    width = function() return u(640) end,
    height = function() return u(style.H - 200) end,
    radius = function() return u(28) end,
    fill = style.c("panel"),
    -- Only while it is out: hidden past the edge, a frosted glass still
    -- blurred what was under its strip, every frame, on every slide.
    backdrop_blur = function() return if code_open.value then u(36) else 0 end,
    rim_top = function() return g.colour.alpha(t.colour("text"), 0.22) end,
    rim_bottom = style.c("line"),
    shadow = "#00000088",
    shadow_blur = function() return u(60) end,
    transition = { offset_x = "panel" },
    direction = "column",
    align = "start",
    justify = "start",
    padding = function() return u(40) end,
    gap = function() return u(22) end,
    style.text("kicker", { text = "LUAU", color = style.c("accent") }),
    g.each {
        items = function() return { current.value } end,
        key = function(n: number) return tostring(n) end,
        row = function(n: () -> number) return code.view(SLIDES[g.untracked(n)].code or "") end,
    },
}

return g.window {
    title = "Genoma",
    app_id = "genoma.showcase",
    width = 1280,
    height = 720,
    min = { 640, 360 },
    fullscreen = function() return whole.value end,
    on_fullscreen = function(on: boolean) whole.value = on end,

    -- The whole window, whatever the compositor gives: it grows into it
    -- and measures it, and the stage inside is scaled to fit.
    g.capsule {
        grow = 1,
        radius = 0,
        fill = "#000000",
        rim_top = CLEAR,
        rim_bottom = CLEAR,
        align = "center",
        justify = "center",
        catch = true,
        inhibit_idle = true,
        on_size = function(w: number, h: number)
            style.k.value = math.min(w / style.W, h / style.H)
        end,
        on_key = key,
        on_click = function(button: number)
            if button == 273 then previous_slide() else next_slide() end
        end,
        on_swipe = function(way: string)
            if way == "left" or way == "up" then next_slide()
            elseif way == "right" or way == "down" then previous_slide() end
        end,
        drag_threshold = 12,

        -- Over the window, not in its flow: the stage never sizes it.
        g.box {
            overlay = true,
            attach = "center",
            width = function() return u(style.W) end,
            height = function() return u(style.H) end,
            clip = true,
            background,
            g.each {
                overlay = true,
                attach = "corner",
                width = function() return u(style.W) end,
                height = function() return u(style.H) end,
                items = shown,
                key = function(n: number) return tostring(n) end,
                row = slide_node,
            },
            chrome,
            code_panel,
        },
    },
}
