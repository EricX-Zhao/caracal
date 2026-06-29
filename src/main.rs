//! Caracal — native GPUI terminal. Phase 1: a bare window hosting a single
//! `TerminalView` running the local shell. No `gpui_component` yet (that arrives
//! in Phase 5 as the dock shell).

mod terminal;

use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};

use terminal::view::TerminalView;

fn main() {
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
