//! Caracal-specific asset source. Wraps `gpui_component_assets::Assets`
//! (which provides the standard lucide icons for `IconName`) and adds
//! two project-local SVGs that the standard bundle doesn't ship:
//!
//! - `icons/sftp-upload.svg`   — lucide `arrow-up-from-line` (used for
//!   the SFTP upload toolbar button; has the "level-up" baseline that
//!   the basic `IconName::ArrowUp` is missing).
//! - `icons/sftp-download.svg` — lucide `arrow-down-to-line` (the
//!   matching "level-down" baseline for the download button).
//!
//! Both SVGs are bundled via `include_bytes!` so the binary stays
//! self-contained — no runtime file lookups, no `rust-embed` dep.
//!
//! Lookups for any other path fall through to the upstream
//! `gpui_component_assets::Assets` so the rest of the icon set
//! (`IconName::Folder`, `IconName::ChevronUp`, etc.) still resolves.

use std::borrow::Cow;

use anyhow::anyhow;
use gpui::{AssetSource, Result, SharedString};

/// The two project-local SVGs, keyed by the asset path `Icon::path("…")`
/// resolves to.
const LOCAL_ICONS: &[(&str, &[u8])] = &[
    (
        "icons/sftp-upload.svg",
        include_bytes!("../assets/icons/sftp-upload.svg"),
    ),
    (
        "icons/sftp-download.svg",
        include_bytes!("../assets/icons/sftp-download.svg"),
    ),
];

/// Bundle of upstream lucide icons (gpui-component-assets) + our two
/// project extras. Register via `application().with_assets(CaracalAssets)`.
pub struct CaracalAssets;

impl AssetSource for CaracalAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        // Project-local icons first (they win over any upstream icon with
        // the same name — none today, but cheap to be explicit).
        if let Some((_, bytes)) = LOCAL_ICONS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        // Fall through to the upstream lucide bundle. `RustEmbed`'s
        // generated `get` / `iter` are associated functions (no `self`),
        // not methods — hence the `::` not `.`.
        gpui_component_assets::Assets::get(path)
            .map(|f| Some(f.data))
            .ok_or_else(|| anyhow!("could not find asset at path {:?}", path))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = LOCAL_ICONS
            .iter()
            .map(|(p, _)| SharedString::from(*p))
            .filter(|p| p.as_ref().starts_with(path))
            .collect();
        // And upstream — `Assets::iter()` already returns paths relative
        // to the bundle root.
        out.extend(
            gpui_component_assets::Assets::iter()
                .filter(|p| p.starts_with(path))
                .map(|p| p.into()),
        );
        Ok(out)
    }
}