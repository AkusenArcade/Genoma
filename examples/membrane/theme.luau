--!strict
-- Bioma's palette, derived as core/Theme.qml derives it (2026-10-28), from
-- the four roles of the live palette.json. The animated theme singleton comes
-- with Bioma's port; for the M2 comparison the palette is read once, at load.
local g = require("@genoma")

type RGB = { r: number, g: number, b: number, a: number }

local function parse(hex: string): RGB
    local function at(i: number): number
        return tonumber(string.sub(hex, i, i + 1), 16) :: number / 255
    end
    return { r = at(2), g = at(4), b = at(6), a = if #hex >= 9 then at(8) else 1 }
end

local function format(c: RGB): string
    local function byte(v: number): string
        return string.format("%02x", math.floor(math.clamp(v, 0, 1) * 255 + 0.5))
    end
    return "#" .. byte(c.r) .. byte(c.g) .. byte(c.b) .. byte(c.a)
end

-- RGB ↔ HSL, as Qt's hslHue / hslSaturation / hslLightness (hue 0…1, -1 for
-- an achromatic colour).
local function hsl(c: RGB): (number, number, number)
    local max, min = math.max(c.r, c.g, c.b), math.min(c.r, c.g, c.b)
    local l = (max + min) / 2
    if max == min then
        return -1, 0, l
    end
    local d = max - min
    local s = if l > 0.5 then d / (2 - max - min) else d / (max + min)
    local h
    if max == c.r then
        h = (c.g - c.b) / d + (if c.g < c.b then 6 else 0)
    elseif max == c.g then
        h = (c.b - c.r) / d + 2
    else
        h = (c.r - c.g) / d + 4
    end
    return h / 6, s, l
end

local function hsla(h: number, s: number, l: number, a: number): RGB
    if s == 0 then
        return { r = l, g = l, b = l, a = a }
    end
    local function channel(p: number, q: number, t: number): number
        t = t % 1
        if t < 1 / 6 then return p + (q - p) * 6 * t end
        if t < 1 / 2 then return q end
        if t < 2 / 3 then return p + (q - p) * (2 / 3 - t) * 6 end
        return p
    end
    local q = if l < 0.5 then l * (1 + s) else l + s - l * s
    local p = 2 * l - q
    return { r = channel(p, q, h + 1 / 3), g = channel(p, q, h), b = channel(p, q, h - 1 / 3), a = a }
end

local palette = g.json("~/.config/bioma/generated/palette.json")
local background = parse(palette.background)
local text = parse(palette.text)
local primary = parse(palette.primary)

local isDark = 0.2126 * background.r + 0.7152 * background.g + 0.0722 * background.b < 0.5

local function lift(c: RGB, amount: number): RGB
    local h, s, l = hsl(c)
    local step = if isDark then amount else -amount
    return hsla(math.max(0, h), s, math.clamp(l + step, 0, 1), c.a)
end

local function alpha(c: RGB, a: number): RGB
    return { r = c.r, g = c.g, b = c.b, a = a }
end

-- The structural family: line, node and rim at three lightnesses of one hue.
local bgH, bgS = hsl(background)
local prH, prS = hsl(primary)
local hue = if bgS > 0.05 and bgH >= 0 then bgH else math.max(0, prH)
local saturation = math.max(math.min(bgS + 0.04, 0.40), math.min(prS * 0.33, 0.28))
local function structural(lightness: number): RGB
    return hsla(hue, saturation, lightness, 1)
end

local function gradientTop(c: RGB): RGB
    local h, s, l = hsl(c)
    return hsla(math.max(0, h), math.min(1, s + 0.14), math.min(1, l + 0.16), c.a)
end
local function gradientBottom(c: RGB): RGB
    local h, s, l = hsl(c)
    return hsla(math.max(0, h), math.max(0, s - 0.15), math.max(0, l - 0.15), c.a)
end

local state = if isDark
    then { calm = "#6ee7a6", active = "#f2c14e", alert = "#ff6b5d" }
    else { calm = "#2e9c6a", active = "#b37414", alert = "#c43a2e" }

-- His settings (override.json): cell.opacity 0.6, tissue.opacity 0.5.
local CELL_OPACITY = 0.6
local TISSUE_OPACITY = 0.5

return {
    isDark = isDark,
    background = format(background),
    text = format(text),
    textMuted = format(alpha(text, 0.56)),
    textHalf = format(alpha(text, 0.5)),
    primary = format(primary),
    primaryTop = format(gradientTop(primary)),
    primaryBottom = format(gradientBottom(primary)),
    -- Halfway between the two, where one bar ends and the next begins.
    primaryMid = (function()
        local a, b = gradientTop(primary), gradientBottom(primary)
        return format({ r = (a.r + b.r) / 2, g = (a.g + b.g) / 2, b = (a.b + b.b) / 2, a = 1 })
    end)(),
    elevated = format(lift(background, 0.045)),
    cellFill = format(alpha(lift(background, 0.075), CELL_OPACITY)),
    tissueFill = format(alpha(lift(background, 0.045), TISSUE_OPACITY)),
    iconDisc = format(lift(background, -0.015)),
    line = format(structural(if isDark then 0.32 else 0.72)),
    node = format(structural(if isDark then 0.43 else 0.62)),
    rim = format(structural(if isDark then 0.55 else 0.50)),
    trackLine = format(alpha(structural(if isDark then 0.32 else 0.72), 0.4)),
    line70 = format(alpha(structural(if isDark then 0.32 else 0.72), 0.7)),
    primary12 = format(alpha(primary, 0.12)),
    well = format(alpha(lift(background, -0.01), 0.8)),
    text72 = format(alpha(text, 0.72)),
    text50 = format(alpha(text, 0.5)),
    text30 = format(alpha(text, 0.3)),
    text28 = format(alpha(text, 0.28)),
    calm = state.calm,
    active = state.active,
    alert = state.alert,
    -- Thresholds 0.30 and 0.80.
    -- The 165° gradient's two ends for any colour (Theme.gradientTop/Bottom).
    gradTop = function(hex: string): string
        return format(gradientTop(parse(hex)))
    end,
    gradBottom = function(hex: string): string
        return format(gradientBottom(parse(hex)))
    end,
    stateFor = function(load: number): string
        return if load >= 0.8 then state.alert elseif load >= 0.3 then state.active else state.calm
    end,
}
