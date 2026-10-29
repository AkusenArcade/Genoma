# taffy, as Genoma builds it

taffy 0.15.1 from crates.io (MIT, https://github.com/DioxusLabs/taffy),
unchanged but for one fix in `src/tree/taffy_tree.rs`, `TaffyTree::remove`:
the node removed is also taken out of every node's hoisted children.

Why: an absolutely positioned node is recorded in its containing block's
hoisted list when that block is laid out. A block whose layout does not run
again — hidden (`display: none`), or a leaf by now — keeps the list of its
last run, and the rounding pass walks hoisted lists whatever the block's
display. A node removed meanwhile is then an invalid slotmap key there, and
`compute_layout` panics ("invalid SlotMap key used"). Bioma hit it with a
closed dropdown list whose rows changed (2026-10-29).

Drop this copy for an upstream release that fixes it.
