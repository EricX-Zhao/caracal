//! Terminal world. ★ No `gpui_component` imports allowed below this module
//! (CLAUDE.md §1). Boundary self-check: `grep -r gpui_component src/terminal/`
//! must return nothing.

pub mod backend;
pub mod bridge;
pub mod grid_snapshot;
pub mod keymap;
pub mod model;
pub mod render;
pub mod scrollback;
pub mod selection;
pub mod ssh;
pub mod view;
