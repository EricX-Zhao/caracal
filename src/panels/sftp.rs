//! `SftpPanel`: a minimal remote file browser over the SFTP subsystem of an
//! existing [`SshSession`] (the *same* connection as the terminal — CLAUDE.md §2:
//! one Session, one connection). Lists directories, navigates, downloads and
//! uploads via the platform file picker.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::ActiveTheme;
use gpui_component::dock::{Panel, PanelEvent};

use crate::terminal::ssh::{SftpEntry, SshSession};

pub struct SftpPanel {
    session: Arc<SshSession>,
    label: SharedString,
    focus_handle: FocusHandle,
    /// Current remote directory (starts at the login dir, ".").
    path: String,
    entries: Vec<SftpEntry>,
    status: String,
}

impl SftpPanel {
    pub fn new(
        session: Arc<SshSession>,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            session,
            label: label.into(),
            focus_handle: cx.focus_handle(),
            path: ".".to_string(),
            entries: Vec::new(),
            status: "Loading…".to_string(),
        };
        this.refresh(cx);
        this
    }

    /// Re-list the current directory.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let rx = self.session.sftp_read_dir(self.path.clone());
        cx.spawn(async move |this, cx| {
            let result = rx.recv_async().await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(entries)) => {
                        this.status = format!("{} item(s)", entries.len());
                        this.entries = entries;
                    }
                    Ok(Err(e)) => this.status = format!("read_dir failed: {e}"),
                    Err(_) => this.status = "session closed".to_string(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn enter_dir(&mut self, name: &str, cx: &mut Context<Self>) {
        self.path = remote_join(&self.path, name);
        self.status = "Loading…".to_string();
        self.refresh(cx);
        cx.notify();
    }

    fn go_up(&mut self, cx: &mut Context<Self>) {
        self.path = remote_parent(&self.path);
        self.status = "Loading…".to_string();
        self.refresh(cx);
        cx.notify();
    }

    fn download(&mut self, name: &str, cx: &mut Context<Self>) {
        let remote = remote_join(&self.path, name);
        let dir = home_dir();
        let rx = cx.prompt_for_new_path(&dir, Some(name));
        let session = self.session.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(local))) = rx.await else {
                return;
            };
            let drx = session.sftp_download(remote, local.clone());
            let result = drx.recv_async().await;
            this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(Ok(n)) => format!("downloaded {n} bytes → {}", local.display()),
                    Ok(Err(e)) => format!("download failed: {e}"),
                    Err(_) => "session closed".to_string(),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn upload(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let session = self.session.clone();
        let path = self.path.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(local) = paths.into_iter().next() else {
                return;
            };
            let name = local
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "upload.bin".to_string());
            let remote = remote_join(&path, &name);
            let urx = session.sftp_upload(local, remote);
            let result = urx.recv_async().await;
            this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(Ok(n)) => format!("uploaded {n} bytes → {name}"),
                    Ok(Err(e)) => format!("upload failed: {e}"),
                    Err(_) => "session closed".to_string(),
                };
                cx.notify();
            })
            .ok();
            // Reflect the new file in the listing.
            this.update(cx, |this, cx| this.refresh(cx)).ok();
        })
        .detach();
    }

    fn row(
        &self,
        id: usize,
        text: String,
        cx: &Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        div()
            .id(("sftp-row", id))
            .w_full()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .text_color(cx.theme().foreground)
            .hover(|s| s.bg(cx.theme().accent))
            .child(text)
            .on_click(cx.listener(move |this, _ev, window, cx| on_click(this, window, cx)))
    }
}

impl Focusable for SftpPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<PanelEvent> for SftpPanel {}

impl Panel for SftpPanel {
    fn panel_name(&self) -> &'static str {
        "SftpPanel"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(format!("SFTP: {}", self.label))
    }
}

/// Shown in the left "SFTP" dock when the focused terminal has no SFTP
/// browser to show — no terminal focused yet, or the focused one isn't SSH
/// (local shell / serial). `Workspace` swaps this in and out as focus moves
/// between terminal tabs, so the dock always holds exactly one tab.
pub struct SftpPlaceholder {
    focus_handle: FocusHandle,
}

impl SftpPlaceholder {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
        }
    }
}

impl Focusable for SftpPlaceholder {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::EventEmitter<PanelEvent> for SftpPlaceholder {}

impl Panel for SftpPlaceholder {
    fn panel_name(&self) -> &'static str {
        "SftpPlaceholder"
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("SFTP")
    }
}

impl Render for SftpPlaceholder {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("当前终端不是 SSH 会话 — 切换到一个 SSH 终端标签,或在「已保存的连接」中点击文件夹图标")
    }
}

impl Render for SftpPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Header: path + upload.
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .child(SharedString::from(self.path.clone()))
            .child(
                div()
                    .id("sftp-upload")
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(cx.theme().secondary)
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().accent))
                    .child("Upload")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.upload(cx))),
            );

        let up_row = self.row(usize::MAX, "..".to_string(), cx, |this, _w, cx| this.go_up(cx));

        let rows = self.entries.iter().enumerate().map(|(ix, entry)| {
            let name = entry.name.clone();
            let text = if entry.is_dir {
                format!("{}/", entry.name)
            } else {
                format!("{}    {}", entry.name, human_size(entry.size))
            };
            let is_dir = entry.is_dir;
            self.row(ix, text, cx, move |this, _w, cx| {
                if is_dir {
                    this.enter_dir(&name, cx);
                } else {
                    this.download(&name, cx);
                }
            })
        });

        div()
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .size_full()
            .text_sm()
            .child(header)
            .child(up_row)
            .child(div().flex().flex_col().flex_1().overflow_hidden().children(rows))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(self.status.clone())),
            )
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Join a remote directory and a child name (POSIX paths).
fn remote_join(base: &str, name: &str) -> String {
    if base == "." || base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Parent of a remote path (stays within where browsing started).
fn remote_parent(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => path[..i].to_string(),
        None => ".".to_string(),
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[0])
    } else {
        format!("{size:.1}{}", UNITS[unit])
    }
}
