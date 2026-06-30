//! Panels — ★ the only place (besides `workspace.rs`) allowed to import
//! `gpui_component`. Each `*Panel` is a thin adapter that embeds an inner
//! `terminal/` entity, delegates focus to it, and exposes a title. No business
//! logic lives here (CLAUDE.md §1).

pub mod session_list;
pub mod terminal;
