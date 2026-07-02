//! 自定义 AssetSource：包装 `gpui_component_assets::Assets`（上游 lucide 图标集），
//! 叠加本项目自定义 SVG 图标（upload / download / file-plus / folder-plus /
//! refresh-cw / trash-2）。
//!
//! 上游没有的图标在这里补充，查找路径先查本地，再 fallthrough 到上游。
//! 这样 `Icon::new(IconName::Upload)` 能直接渲染自定义 SVG，无需改调用方。

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const LOCAL_ICONS: &[(&str, &[u8])] = &[
    (
        "icons/upload.svg",
        include_bytes!("../assets/icons/upload.svg"),
    ),
    (
        "icons/download.svg",
        include_bytes!("../assets/icons/download.svg"),
    ),
    (
        "icons/file-plus.svg",
        include_bytes!("../assets/icons/file-plus.svg"),
    ),
    (
        "icons/folder-plus.svg",
        include_bytes!("../assets/icons/folder-plus.svg"),
    ),
    (
        "icons/refresh-cw.svg",
        include_bytes!("../assets/icons/refresh-cw.svg"),
    ),
    (
        "icons/trash-2.svg",
        include_bytes!("../assets/icons/trash-2.svg"),
    ),
];

pub struct CaracalAssets;

impl AssetSource for CaracalAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        if let Some((_, bytes)) = LOCAL_ICONS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_component_assets::Assets::get(path)
            .map(|f| Some(f.data))
            .ok_or_else(|| anyhow::anyhow!("could not find asset at path {:?}", path))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = LOCAL_ICONS
            .iter()
            .map(|(p, _)| SharedString::from(*p))
            .filter(|p| p.as_ref().starts_with(path))
            .collect();
        out.extend(
            gpui_component_assets::Assets::iter()
                .filter(|p| p.starts_with(path))
                .map(|p| p.into()),
        );
        Ok(out)
    }
}
