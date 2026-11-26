# alacritty_terminal, as Genoma builds it

alacritty_terminal 0.26.0 from crates.io (Apache-2.0,
https://github.com/alacritty/alacritty), unchanged but for one addition in
`src/grid/mod.rs`: `Grid::scrolled()`, the count of lines that have ever
left the top of the screen for the history.

Why: a terminal emulator that marks lines (OSC 133: where a prompt, a
command and its output start) needs to find a marked line again after the
history has grown and dropped its oldest lines. A line's place counted from
the grid's first line, `scrolled()` plus its line on the screen, does not
change; alacritty_terminal keeps no such count.

Drop this copy if upstream exposes one.
