//! Where the app stores its persisted state (`config.rs`'s saved
//! connections, `settings.rs`'s app settings, `quick_commands.rs`'s saved
//! commands). Plain Rust — no `gpui_component` here (CLAUDE.md §1 boundary),
//! same rule those three callers already follow.
//!
//! One folder, same name, on every platform: `~/.caracal`. Deliberately not
//! following the XDG Base Directory split (`~/.config/caracal` on Linux,
//! `%APPDATA%\caracal` on Windows) — a single well-known folder name is
//! easier to find/back up/delete than remembering a different convention
//! per platform.

use std::path::PathBuf;

/// The app's config directory: `~/.caracal`, resolved via `dirs::home_dir()`
/// (correctly finds `$HOME` on Linux/macOS and `%USERPROFILE%` on Windows —
/// unlike a raw `$HOME` env var read, which has no equivalent set on
/// Windows and would otherwise need its own fallback).
pub fn app_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".caracal")
}
