--!strict
-- Waiting written straight down instead of nested in callbacks: inside a
-- task (async.run), a process, a D-Bus call, a delay or an activation
-- token is waited for as if it answered at once, and nothing else stops
-- meanwhile.
--
--   local async = require("@genoma/async")
--   async.run(function()
--       local code, lines = async.process { "git", "-C", dir, "status", "--short" }
--       if code ~= 0 then return end
--       async.sleep(0.5)
--       local ok, reply = async.call { bus = "session", destination = "org.freedesktop.Notifications",
--           path = "/org/freedesktop/Notifications", interface = "org.freedesktop.Notifications",
--           method = "GetServerInformation" }
--   end)
--
-- A task is a coroutine: what waits yields it, and the answer resumes it.
-- A mistake in a task goes to the log with its line. The waiting functions
-- are for inside a task only.
local g = require("@genoma")

local M = {}

local function resume(task: thread, ...: any)
    local ok, why = coroutine.resume(task, ...)
    if not ok then print("async task: " .. tostring(why)) end
end

-- Runs `f` as a task now, with these arguments, until it first waits.
function M.run(f: (...any) -> (), ...: any)
    resume(coroutine.create(f), ...)
end

local function this_task(what: string): thread
    -- Told at the caller's line: this, then wait, then the function it called.
    if not coroutine.isyieldable() then error(what .. " waits only inside async.run", 4) end
    return coroutine.running()
end

-- Starts something with a function to call when it is done, and waits for
-- that call: what it is given is what the wait returns. One that answers
-- at once returns at once.
local function wait(what: string, start: (done: (...any) -> ()) -> ()): ...any
    local task = this_task(what)
    local answered: { n: number, [number]: any }? = nil
    local waiting = false
    start(function(...)
        if waiting then
            waiting = false
            resume(task, ...)
        elseif not answered then
            answered = table.pack(...)
        end
    end)
    if answered then return table.unpack(answered, 1, answered.n) end
    waiting = true
    return coroutine.yield()
end

-- Waits this many seconds.
function M.sleep(seconds: number)
    wait("async.sleep", function(done) g.after(seconds, done) end)
end

-- Runs a command to its end: its exit status, its output and its errors as
-- lines.
function M.process(command: { string }, options: { env: { [string]: string }?, cwd: string? }?): (number, { string }, { string })
    local out, errors = {}, {}
    return wait("async.process", function(done)
        g.process {
            command = command,
            env = if options then options.env else nil,
            cwd = if options then options.cwd else nil,
            on_line = function(line: string) table.insert(out, line) end,
            on_error_line = function(line: string) table.insert(errors, line) end,
            on_exit = function(code: number) done(code, out, errors) end,
        }
    end)
end

-- A D-Bus method call, as g.dbus.call takes it without `reply`: true and
-- the results, or false and the error.
function M.call(t: { [string]: any }): (boolean, ...any)
    local call = table.clone(t)
    return wait("async.call", function(done)
        call.reply = done
        g.dbus.call(call :: any)
    end)
end

-- A D-Bus property, as g.dbus.get takes it without `reply`: true and the
-- value, or false and the error.
function M.get(t: { [string]: any }): (boolean, any)
    local get = table.clone(t)
    return wait("async.get", function(done)
        get.reply = done
        g.dbus.get(get :: any)
    end)
end

-- An activation token for a program about to start (nil where the
-- compositor gives none): asked inside the handler of the press, as
-- g.activation_token is.
function M.activation_token(app_id: string?): string?
    return wait("async.activation_token", function(done)
        g.activation_token { app_id = app_id, on_token = done }
    end)
end

-- Runs these functions as tasks side by side and waits for them all: what
-- each returned, in their order.
function M.all(tasks: { () -> any }): { any }
    local task = this_task("async.all")
    local results = {}
    local left = #tasks
    if left == 0 then return results end
    local waiting = false
    for i, f in tasks do
        M.run(function()
            results[i] = f()
            left -= 1
            if left == 0 and waiting then
                waiting = false
                resume(task)
            end
        end)
    end
    if left > 0 then
        waiting = true
        coroutine.yield()
    end
    return results
end

return M
