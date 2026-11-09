--!strict
-- A lock screen: the session locked (ext-session-lock) as it loads, a
-- surface on every output with the time and a password field, unlocked
-- only when PAM says the password is right. Its PAM file is beside it
-- (pam/genoma-example-lock).
--
--   genoma examples/lock
local g = require("@genoma")
local lock = require("@genoma/lock")
local clock = require("@genoma/clock")
local kit = require("@genoma/kit")

local theme = kit.theme

local password = g.signal("")
local checking = g.signal(false)
local said = g.signal("")

-- Locked once, as soon as the compositor's lock is known (a moment after
-- the load); one that has none after a few seconds is told, and the lock
-- leaves rather than pretend.
local began = g.state("lock.began", false)
g.watch(function() return lock.available end, function(available: boolean)
    if available and not g.untracked(function() return began.value end) then
        began.value = true
        lock:lock()
    end
end)
g.after(3, function()
    if not g.untracked(function() return began.value end) then
        print("examples/lock: the compositor offers no ext-session-lock")
        g.quit(1)
    end
end)
g.watch(function() return lock.finished end, function(done: boolean)
    if done then g.quit(1) end
end)

local function submit()
    if g.untracked(function() return checking.value or password.value == "" end) then return end
    checking.value = true
    said.value = "Checking…"
    local auth: any
    auth = g.pam.start {
        service = "genoma-example-lock",
        directory = g.dir .. "/pam",
        on_prompt = function() auth:respond(g.untracked(function() return password.value end)) end,
        on_done = function(result: string)
            checking.value = false
            if result == "success" then
                lock:unlock()
                g.quit()
                return
            end
            password.value = ""
            said.value = if result == "failure" then "That is not the password"
                elseif result == "max-tries" then "Too many attempts"
                else "The password could not be checked"
        end,
    }
end

local function two(n: number): string return string.format("%02d", n) end

return g.surface {
    layer = "lock",
    outputs = "all",
    full = true,
    anchor = { "top", "bottom", "left", "right" },
    namespace = "genoma-lock",
    function(_output: string, size: any)
        size = size or { width = 1920, height = 1080 }
        return g.capsule {
            width = size.width,
            height = size.height,
            radius = 0,
            direction = "column",
            justify = "center",
            align = "center",
            gap = 18,
            fill = "#10131a",
            fill_bottom = "#1d2433",
            rim_top = "#00000000",
            rim_bottom = "#00000000",
            g.text {
                size = 72,
                weight = 300,
                tabular = true,
                color = function() return theme.get("text") end,
                text = function() return two(clock.hour) .. ":" .. two(clock.minute) end,
            },
            g.capsule {
                padding_x = 14,
                height = 40,
                width = 300,
                radius = 20,
                fill = function() return theme.get("control") end,
                rim_top = function() return theme.get("line") end,
                rim_bottom = function() return theme.get("line") end,
                g.field {
                    grow = 1,
                    size = 15,
                    secret = true,
                    focused = true,
                    placeholder = "Password",
                    placeholder_color = function() return theme.get("faint") end,
                    color = function() return theme.get("text") end,
                    caret_color = function() return theme.get("accent") end,
                    text = function() return password.value end,
                    on_change = function(text: string)
                        password.value = text
                        if text ~= "" then said.value = "" end
                    end,
                    on_submit = function() submit() end,
                },
            },
            g.text {
                size = 13,
                color = function() return if checking.value then theme.get("faint") else theme.get("danger") end,
                text = function() return said.value end,
            },
        }
    end,
}
