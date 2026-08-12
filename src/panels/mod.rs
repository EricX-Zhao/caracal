//! Panels — ★ the only place (besides `workspace.rs`) allowed to import
//! `gpui_component`. Each `*Panel` is a thin adapter that embeds an inner
//! `terminal/` entity, delegates focus to it, and exposes a title. No business
//! logic lives here (CLAUDE.md §1).

pub mod activity_bar;
pub mod command_history_panel;
pub mod header;
pub mod icons;
pub mod keybindings;
pub mod monitor;
pub mod new_connection_window;
pub mod quick_commands_panel;
pub mod security_auth;
pub mod sessions;
pub mod settings_window;
pub mod sftp;
pub mod side_region;
pub mod stub;
pub mod terminal;
