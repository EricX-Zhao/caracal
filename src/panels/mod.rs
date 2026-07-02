//! Panels — ★ the only place (besides `workspace.rs`) allowed to import
//! `gpui_component`. Each `*Panel` is a thin adapter that embeds an inner
//! `terminal/` entity, delegates focus to it, and exposes a title. No business
//! logic lives here (CLAUDE.md §1).

pub mod activity_bar;
pub mod header;
pub mod icons;
pub mod saved_connections;
pub mod sftp;
pub mod side_region;
pub mod stub;
pub mod terminal;
