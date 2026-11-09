--!strict
-- Genoma's component kit, every piece in one table:
--
--   local kit = require("@genoma/kit")
--   kit.button { text = "OK", kind = "accent", on_click = done }
--
-- or each on its own, `require("@genoma/kit/button")`. The look is the
-- kit's theme (kit.theme.set { … }); nothing here is Bioma's.
return {
    theme = require("@genoma/kit/theme"),
    button = require("@genoma/kit/button"),
    toggle = require("@genoma/kit/toggle"),
    slider = require("@genoma/kit/slider"),
    progress = require("@genoma/kit/progress"),
    menu = require("@genoma/kit/menu"),
    dropdown = require("@genoma/kit/dropdown"),
    tooltip = require("@genoma/kit/tooltip"),
    list = require("@genoma/kit/list"),
    dialog = require("@genoma/kit/dialog"),
    tabs = require("@genoma/kit/tabs"),
    effects = require("@genoma/kit/effects"),
    field = require("@genoma/kit/field"),
}
