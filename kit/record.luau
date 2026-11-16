--!strict
-- Screen recording through a recorder program: wl-screenrec (hardware
-- encoding), else wf-recorder. Genoma encodes nothing itself; this module
-- starts the program, counts the seconds, ends it cleanly and says how it
-- went.
--
--   local record = require("@genoma/kit/record")
--   record.start {
--       path = "~/Videos/clip.mp4",
--       area = { x = 0, y = 0, w = 1280, h = 720 },   -- or output = "DP-1"
--       on_end = function(path, error) print(path, error) end,
--   }
--   record.recording()   -- true until it ends
--   record.elapsed()     -- whole seconds since it began
--   record.stop()
--
-- One recording at a time. Areas are logical coordinates, as
-- @genoma/capture takes them. The recorder is ended with SIGINT, which
-- lets it finish the file: killed outright, an MP4 has no index and does
-- not play.
local g = require("@genoma")

local M = {}

export type Options = {
    -- Where the video goes; "~/" is expanded, the folder made.
    path: string,
    -- One output by name, or a logical area; neither is every output the
    -- recorder chooses (wl-screenrec: the focused one).
    output: string?,
    area: { x: number, y: number, w: number, h: number }?,
    -- Frames a second at most (60).
    fps: number?,
    -- The default audio source recorded too.
    audio: boolean?,
    -- The pointer drawn in (wl-screenrec only).
    cursor: boolean?,
    -- Once it ends: the path, or nil and why.
    on_end: ((path: string?, error: string?) -> ())?,
}

local recording = g.signal(false)
local elapsed = g.signal(0)
local process: any = nil
local counting: any = nil

function M.recording(): boolean return recording.value end
function M.elapsed(): number return elapsed.value end

local function expand(path: string): string
    if string.sub(path, 1, 2) == "~/" then return (g.env("HOME") or "") .. string.sub(path, 2) end
    return path
end

local function geometry(a: { x: number, y: number, w: number, h: number }): string
    return string.format("%d,%d %dx%d", math.floor(a.x), math.floor(a.y), math.floor(a.w), math.floor(a.h))
end

-- Each recorder's arguments for the same recording.
local recorders: { { name: string, args: (Options, string) -> { string } } } = {
    {
        name = "wl-screenrec",
        args = function(o: Options, file: string): { string }
            local a = { "--filename", file, "--max-fps", tostring(o.fps or 60) }
            if not o.cursor then table.insert(a, "--no-cursor") end
            if o.audio then table.insert(a, "--audio") end
            if o.output then
                table.insert(a, "-o"); table.insert(a, o.output)
            elseif o.area then
                table.insert(a, "-g"); table.insert(a, geometry(o.area))
            end
            return a
        end,
    },
    {
        name = "wf-recorder",
        args = function(o: Options, file: string): { string }
            local a = { "-y", "-f", file, "-r", tostring(o.fps or 60) }
            if o.audio then table.insert(a, "-a") end
            if o.output then
                table.insert(a, "-o"); table.insert(a, o.output)
            elseif o.area then
                table.insert(a, "-g"); table.insert(a, geometry(o.area))
            end
            return a
        end,
    },
}

-- 127: the program is not there, the next one is tried.
local launch = 'mkdir -p "$1" || exit 1; shift; command -v "$1" >/dev/null 2>&1 || exit 127; exec "$@"'

local function finish(o: Options, path: string?, why: string?)
    if counting then counting:stop() end
    counting = nil
    process = nil
    g.batch(function() recording.value = false end)
    if o.on_end then o.on_end(path, why) end
end

local function try(o: Options, file: string, folder: string, index: number)
    local recorder = recorders[index]
    if not recorder then
        finish(o, nil, "no recorder: install wl-screenrec or wf-recorder")
        return
    end
    local command = { "sh", "-c", launch, "record", folder, recorder.name }
    for _, a in recorder.args(o, file) do table.insert(command, a) end
    local said = ""
    -- Lasting: started from an effect, the recording outlives its re-run.
    g.lasting(function()
        process = g.process {
            command = command,
            on_error_line = function(line: string)
                if line ~= "" then said = line end
            end,
            on_exit = function(code: number)
                if code == 127 then
                    try(o, file, folder, index + 1)
                elseif code == 0 or code == 130 or code == 2 then
                    finish(o, file, nil)
                else
                    finish(o, nil, recorder.name .. " ended with " .. code .. (if said ~= "" then ": " .. said else ""))
                end
            end,
        }
    end)
end

-- Begins a recording; false and why when one is already running.
function M.start(o: Options): (boolean, string?)
    if g.untracked(function() return recording.value end) then return false, "already recording" end
    if o.output and o.area then return false, "an output or an area, not both" end
    local file = expand(o.path)
    local folder = string.match(file, "^(.*)/[^/]*$") or "."
    g.batch(function()
        recording.value = true
        elapsed.value = 0
    end)
    g.lasting(function()
        counting = g.timer {
            every = 1,
            on_tick = function() elapsed.value += 1 end,
        }
    end)
    try(o, file, folder, 1)
    return true, nil
end

-- Ends it; on_end follows once the recorder has written the file.
function M.stop()
    if process then process:signal(2) end
end

return M
