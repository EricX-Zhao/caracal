//! Top-level workspace: hosts the `gpui_component` `DockArea` and seeds it with
//! one central terminal tab. Event routing (open new sessions → new panels)
//! arrives in Phase 6. The `terminal/` world stays free of `gpui_component`; the
//! shell (here + `panels/`) is where it's used.

use gpui::{AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div};
use gpui_component::dock::{DockArea, DockItem};

use crate::panels::terminal::TerminalPanel;
use crate::terminal::ssh::SshConfig;
use crate::terminal::view::TerminalView;

pub struct Workspace {
    dock_area: Entity<DockArea>,
}

impl Workspace {
    pub fn new(ssh: Option<SshConfig>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dock_area =
            cx.new(|cx| DockArea::new("caracal-main", Some(1), window, cx));
        let weak = dock_area.downgrade();

        // The central tab: a terminal (local or SSH), wrapped in its panel adapter.
        let terminal = cx.new(|cx| match ssh {
            Some(config) => TerminalView::new_ssh(window, cx, config),
            None => TerminalView::new(window, cx),
        });
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        let center = DockItem::tab(panel, &weak, window, cx);

        dock_area.update(cx, |dock_area, cx| {
            dock_area.set_center(center, window, cx);
        });

        Self { dock_area }
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.dock_area.clone())
    }
}
