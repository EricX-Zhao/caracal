//! Caracal — native GPUI terminal. Phase 1: a bare window hosting a single
//! `TerminalView` running the local shell. No `gpui_component` yet (that arrives
//! in Phase 5 as the dock shell).

mod terminal;

use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};

use terminal::view::TerminalView;

/// Work around blade-graphics (gpui's renderer) picking the *first* enumerated
/// Vulkan device with no present-capable fallback: on a hybrid-GPU laptop it
/// grabs the discrete NVIDIA GPU, which can't present to the iGPU-driven Wayland
/// compositor surface (panics with `PlatformNotSupported`). If the user hasn't
/// already chosen an ICD, prefer a non-NVIDIA one (the GPU actually driving the
/// compositor). No-op on single-GPU / NVIDIA-only machines.
fn prefer_compositor_gpu() {
    if std::env::var_os("VK_DRIVER_FILES").is_some()
        || std::env::var_os("VK_ICD_FILENAMES").is_some()
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir("/usr/share/vulkan/icd.d") else {
        return;
    };
    let mut pick: Option<std::path::PathBuf> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !name.ends_with(".json") {
            continue;
        }
        // Skip the discrete NVIDIA GPU and legacy/software fallback drivers
        // (`hasvk` = Haswell-era, `lvp`/`swrast` = software) that don't drive a
        // modern compositor.
        if ["nvidia", "hasvk", "lvp", "swrast"]
            .iter()
            .any(|bad| name.contains(bad))
        {
            continue;
        }
        // Prefer the primary Intel ICD; otherwise take the first viable one.
        if name.contains("intel") {
            pick = Some(entry.path());
            break;
        }
        pick.get_or_insert(entry.path());
    }
    if let Some(path) = pick {
        // SAFETY: called at the very start of main, before any threads spawn or
        // the Vulkan loader reads the environment. Set both the current
        // (`VK_DRIVER_FILES`) and legacy (`VK_ICD_FILENAMES`) variables.
        unsafe {
            std::env::set_var("VK_DRIVER_FILES", &path);
            std::env::set_var("VK_ICD_FILENAMES", &path);
        }
    }
}

fn main() {
    prefer_compositor_gpu();

    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TerminalView::new(window, cx)),
        )
        .expect("failed to open window");

        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
