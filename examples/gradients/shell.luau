--!strict
-- Fill gradients: turned linear, radial and conic, with two colours or with
-- stops. The conic swatch turns with the seconds, a value driving the angle.
local g = require("@genoma")
local clock = require("@genoma/clock")

g.timing {
    reflow = { duration = 0.2, curve = { 0.2, 0.0, 0.0, 1.0 } },
}

local STOPS = { { 0, "#5e81ac" }, { 0.5, "#b48ead" }, { 1, "#ebcb8b" } }

local function swatch(t: { [any]: any })
    t.width = 120
    t.height = 80
    t.radius = 14
    t.rim_top = "#ffffff30"
    t.rim_bottom = "#00000040"
    return g.capsule(t)
end

return g.surface {
    anchor = "top",
    margin = { top = 120 },

    g.capsule {
        padding = 16,
        gap = 16,
        fill = "#16181df0",
        radius = 22,
        -- Today's vertical gradient, unchanged.
        swatch { fill = "#88c0d0", fill_bottom = "#2e3440" },
        swatch { fill = "#88c0d0", fill_bottom = "#2e3440", fill_angle = 90 },
        swatch { fill = "#88c0d0", fill_bottom = "#2e3440", fill_angle = 135 },
        swatch { fill_stops = STOPS, fill_angle = 45 },
        swatch { fill = "#ebcb8b", fill_bottom = "#bf616a", fill_shape = "radial" },
        swatch {
            fill_stops = { { 0, "#bf616a" }, { 0.33, "#ebcb8b" }, { 0.66, "#a3be8c" }, { 1, "#bf616a" } },
            fill_shape = "conic",
            fill_angle = function() return clock.second * 6 end,
        },
    },
}
