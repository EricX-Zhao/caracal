//! `NewConnectionWindow`: a standalone window (File-menu-independent — opened
//! from the saved-connections panel's toolbar/context-menu/hover-edit entry
//! points, see `SessionsPanel::open_new_connection_window`) for
//! creating or editing a saved connection. Shares one form for both: `existing`
//! is `Some((ix, conn))` when editing in place, `None` when creating new.
//! Ported from the panel's former inline `ConnForm` (see git history / the
//! design spec for what changed: standalone window instead of inline,
//! added icon picker, added SSH private-key auth).

use gpui::{
    App, AppContext, ClickEvent, Context, Div, Entity, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled, WeakEntity,
    Window, div,
};
use gpui_component::button::{Button, DropdownButton};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{ActiveTheme, Sizable};

use crate::config::{ConnectionType, SavedConnection};
use crate::panels::icons::{AppIcon, icon};
use crate::panels::sessions::SessionsPanel;

/// Icon-picker options, matching `SavedConnection::resolve_icon`'s existing
/// string-key matching in `config.rs` (`"terminal"`, `"laptop"`, `"server"`,
/// `"network"`, `"telnet"`, `"serial"`). `None` means "auto" (icon inferred
/// from `conn_type`, today's behavior). The second element is a locale key
/// (`t!(...)` supports a runtime/non-literal key, just skipping the
/// compile-time key-minify optimization — confirmed against
/// `rust-i18n-macro`'s `tr.rs`), not the display text itself, since this is
/// a `const` array and `t!()` isn't const-evaluable.
const ICON_OPTIONS: &[(Option<&str>, &str, AppIcon)] = &[
    (None, "NewConnectionWindow.icon_auto", AppIcon::Sessions),
    (Some("terminal"), "NewConnectionWindow.icon_terminal", AppIcon::Terminal),
    (Some("laptop"), "NewConnectionWindow.icon_laptop", AppIcon::LocalTerminal),
    (Some("server"), "NewConnectionWindow.icon_server", AppIcon::Sessions),
    (Some("network"), "NewConnectionWindow.icon_network", AppIcon::Network),
    (Some("telnet"), "Telnet", AppIcon::Telnet),
    (Some("serial"), "NewConnectionWindow.icon_serial", AppIcon::SerialPort),
];

pub struct NewConnectionWindow {
    panel: WeakEntity<SessionsPanel>,
    /// `Some(ix)` when editing an existing connection in place; `None` when
    /// creating a new one.
    edit_ix: Option<usize>,
    group_id: Option<String>,
    icon_key: Option<String>,
    conn_type: ConnectionType,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    auth_method: String,
    private_key_path: Entity<InputState>,
    private_key_passphrase: Entity<InputState>,
    shell_path: Entity<InputState>,
    working_dir: Entity<InputState>,
    serial_port: Entity<InputState>,
    baud_rate: Entity<InputState>,
    data_bits: u8,
    parity: String,
    stop_bits: u8,
    flow_control: String,
    /// The `sort_order` this connection will be saved with — for edits,
    /// the existing connection's value (position doesn't change on edit);
    /// for new connections, `new_sort_order` as computed by the caller
    /// (`SessionsPanel::open_new_connection_window`, which has
    /// access to the full connection list this window doesn't).
    sort_order: i32,
}

impl NewConnectionWindow {
    pub fn new(
        panel: WeakEntity<SessionsPanel>,
        existing: Option<(usize, SavedConnection)>,
        group_id: Option<String>,
        new_sort_order: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let conn = existing.as_ref().map(|(_, c)| c.clone());
        let edit_ix = existing.map(|(ix, _)| ix);
        let group_id = conn.as_ref().and_then(|c| c.group_id.clone()).or(group_id);

        let text = |field: fn(&SavedConnection) -> &str| -> String {
            conn.as_ref().map(field).unwrap_or_default().to_string()
        };

        Self {
            panel,
            edit_ix,
            group_id,
            sort_order: conn.as_ref().map(|c| c.sort_order).unwrap_or(new_sort_order),
            icon_key: conn.as_ref().and_then(|c| c.icon.clone()),
            conn_type: conn.as_ref().map(|c| c.conn_type.clone()).unwrap_or(ConnectionType::Ssh),
            name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.name_placeholder"))
                    .default_value(text(|c| &c.name))
            }),
            host: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.host_placeholder"))
                    .default_value(text(|c| &c.host))
            }),
            port: cx.new(|cx| {
                // No fixed "(默认 NN)" here — SSH defaults to 22, Telnet to
                // 23, and the field's *label* (see `Render`'s per-type
                // fields list) already states the right one for whichever
                // type is currently selected.
                InputState::new(window, cx)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.port_placeholder"))
                    .default_value(conn.as_ref().map(|c| c.port.to_string()).unwrap_or_default())
            }),
            user: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.user_placeholder"))
                    .default_value(text(|c| &c.user))
            }),
            password: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.password_placeholder"))
                    .default_value(text(|c| &c.password))
            }),
            auth_method: conn
                .as_ref()
                .map(|c| c.auth_method.clone())
                .unwrap_or_else(|| "password".to_string()),
            private_key_path: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.private_key_path_placeholder"))
                    .default_value(
                        conn.as_ref()
                            .and_then(|c| c.private_key_path.clone())
                            .unwrap_or_default(),
                    )
            }),
            private_key_passphrase: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.private_key_passphrase_placeholder"))
                    .default_value(
                        conn.as_ref()
                            .and_then(|c| c.private_key_passphrase.clone())
                            .unwrap_or_default(),
                    )
            }),
            shell_path: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.shell_path_placeholder"))
                    .default_value(
                        conn.as_ref().and_then(|c| c.shell_path.clone()).unwrap_or_default(),
                    )
            }),
            working_dir: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(rust_i18n::t!("NewConnectionWindow.working_dir_placeholder"))
                    .default_value(
                        conn.as_ref().and_then(|c| c.working_dir.clone()).unwrap_or_default(),
                    )
            }),
            serial_port: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("/dev/ttyUSB0")
                    .default_value(
                        conn.as_ref().and_then(|c| c.serial_port.clone()).unwrap_or_default(),
                    )
            }),
            baud_rate: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("115200")
                    .default_value(
                        conn.as_ref()
                            .and_then(|c| c.baud_rate)
                            .unwrap_or(115_200)
                            .to_string(),
                    )
            }),
            data_bits: conn.as_ref().and_then(|c| c.data_bits).unwrap_or(8),
            parity: conn
                .as_ref()
                .and_then(|c| c.parity.clone())
                .unwrap_or_else(|| "none".to_string()),
            stop_bits: conn.as_ref().and_then(|c| c.stop_bits).unwrap_or(1),
            flow_control: conn
                .as_ref()
                .and_then(|c| c.flow_control.clone())
                .unwrap_or_else(|| "none".to_string()),
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let group_id = self.group_id.clone();
        let edit_ix = self.edit_ix;
        let icon = self.icon_key.clone();

        let conn = match self.conn_type {
            ConnectionType::Ssh => {
                let host = self.host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host,
                    port: self.port.read(cx).value().trim().parse().unwrap_or(22),
                    user: self.user.read(cx).value().trim().to_string(),
                    // Only persist whichever credential the selected auth
                    // method actually uses — otherwise a password typed
                    // before switching to key auth would linger in
                    // plaintext on disk even though it's never used to
                    // connect (mirrors why `duplicate()` clears
                    // `private_key_passphrase` when copying a connection).
                    password: if self.auth_method == "key" {
                        String::new()
                    } else {
                        self.password.read(cx).value().to_string()
                    },
                    group_id,
                    conn_type: self.conn_type.clone(),
                    icon,
                    sort_order: self.sort_order,
                    shell_path: None,
                    working_dir: None,
                    serial_port: None,
                    baud_rate: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    flow_control: None,
                    description: None,
                    auth_method: self.auth_method.clone(),
                    private_key_path: if self.auth_method == "key" {
                        Some(self.private_key_path.read(cx).value().trim().to_string())
                    } else {
                        None
                    },
                    private_key_passphrase: if self.auth_method == "key" {
                        let p = self.private_key_passphrase.read(cx).value().to_string();
                        if p.is_empty() { None } else { Some(p) }
                    } else {
                        None
                    },
                    // Encryption isn't wired in yet (see the
                    // encrypted-credential-storage plan's Task 4) — these
                    // stay empty/absent until then, matching today's
                    // pre-migration behavior.
                    encrypted_password: String::new(),
                    encrypted_key_passphrase: None,
                    private_key_id: None,
                }
            }
            ConnectionType::Local => {
                let shell_path = self.shell_path.read(cx).value().trim().to_string();
                let working_dir = self.working_dir.read(cx).value().trim().to_string();
                SavedConnection {
                    name,
                    host: String::new(),
                    port: 0,
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type: self.conn_type.clone(),
                    icon,
                    sort_order: self.sort_order,
                    shell_path: if shell_path.is_empty() { None } else { Some(shell_path) },
                    working_dir: if working_dir.is_empty() { None } else { Some(working_dir) },
                    serial_port: None,
                    baud_rate: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    flow_control: None,
                    description: None,
                    auth_method: "password".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
                    encrypted_password: String::new(),
                    encrypted_key_passphrase: None,
                    private_key_id: None,
                }
            }
            ConnectionType::Telnet => {
                let host = self.host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host,
                    port: self.port.read(cx).value().trim().parse().unwrap_or(23),
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type: self.conn_type.clone(),
                    icon,
                    sort_order: self.sort_order,
                    shell_path: None,
                    working_dir: None,
                    serial_port: None,
                    baud_rate: None,
                    data_bits: None,
                    parity: None,
                    stop_bits: None,
                    flow_control: None,
                    description: None,
                    auth_method: "password".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
                    encrypted_password: String::new(),
                    encrypted_key_passphrase: None,
                    private_key_id: None,
                }
            }
            ConnectionType::Serial => {
                let serial_port = self.serial_port.read(cx).value().trim().to_string();
                if serial_port.is_empty() {
                    return;
                }
                SavedConnection {
                    name,
                    host: String::new(),
                    port: 0,
                    user: String::new(),
                    password: String::new(),
                    group_id,
                    conn_type: self.conn_type.clone(),
                    icon,
                    sort_order: self.sort_order,
                    shell_path: None,
                    working_dir: None,
                    serial_port: Some(serial_port),
                    baud_rate: Some(
                        self.baud_rate.read(cx).value().trim().parse().unwrap_or(115_200),
                    ),
                    data_bits: Some(self.data_bits),
                    parity: Some(self.parity.clone()),
                    stop_bits: Some(self.stop_bits),
                    flow_control: Some(self.flow_control.clone()),
                    description: None,
                    auth_method: "password".to_string(),
                    private_key_path: None,
                    private_key_passphrase: None,
                    encrypted_password: String::new(),
                    encrypted_key_passphrase: None,
                    private_key_id: None,
                }
            }
        };

        let _ = self.panel.update(cx, |panel, cx| {
            panel.upsert_connection(conn, edit_ix, cx);
        });
        window.remove_window();
    }

    fn field(
        &self,
        label: impl Into<SharedString>,
        state: &Entity<InputState>,
        cx: &App,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label(label, cx))
            .child(Input::new(state))
    }

    fn field_label(&self, label: impl Into<SharedString>, cx: &App) -> impl IntoElement {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label.into())
    }

    fn pill(id: &'static str, label: impl Into<SharedString>, active: bool, cx: &App) -> Stateful<Div> {
        div()
            .id(id)
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(if active { cx.theme().primary } else { cx.theme().accent })
            .text_color(if active {
                cx.theme().primary_foreground
            } else {
                cx.theme().foreground
            })
            .child(label.into())
    }

    fn render_icon_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = ICON_OPTIONS
            .iter()
            .find(|(key, _, _)| key.map(|k| k.to_string()) == self.icon_key)
            .unwrap_or(&ICON_OPTIONS[0]);
        // `PopupMenuItem::on_click` closures are plain closures, not
        // `cx.listener(...)` — they have no direct access to `self`. Capture
        // a `WeakEntity<Self>` and update through it instead, the same way
        // `sessions.rs`'s `confirm_delete_connection` mutates panel
        // state from inside its (also plain, non-listener) `on_ok` closure:
        // `weak_panel.update(cx, |this, cx| this.delete(ix, cx))`.
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label(rust_i18n::t!("NewConnectionWindow.icon_label"), cx))
            .child(
                DropdownButton::new("icon-picker")
                    .small()
                    .button(
                        Button::new("icon-picker-btn")
                            .icon(icon(current.2))
                            .label(rust_i18n::t!(current.1)),
                    )
                    .dropdown_menu(move |menu, _window, _cx| {
                        let mut menu = menu;
                        for (key, label, app_icon) in ICON_OPTIONS {
                            let key = key.map(|k| k.to_string());
                            let weak = weak.clone();
                            menu = menu.item(
                                PopupMenuItem::new(rust_i18n::t!(*label)).icon(icon(*app_icon)).on_click(
                                    move |_ev, _window, cx| {
                                        let _ = weak.update(cx, |this, cx| {
                                            this.icon_key = key.clone();
                                            cx.notify();
                                        });
                                    },
                                ),
                            );
                        }
                        menu
                    }),
            )
    }

    fn data_bits_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label(rust_i18n::t!("NewConnectionWindow.data_bits_label"), cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill("data-bits-5", "5", self.data_bits == 5, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.data_bits = 5;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("data-bits-6", "6", self.data_bits == 6, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.data_bits = 6;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("data-bits-7", "7", self.data_bits == 7, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.data_bits = 7;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("data-bits-8", "8", self.data_bits == 8, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.data_bits = 8;
                                cx.notify();
                            }),
                        ),
                    ),
            )
    }

    fn parity_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label(rust_i18n::t!("NewConnectionWindow.parity_label"), cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill(
                            "parity-none",
                            rust_i18n::t!("NewConnectionWindow.parity_none"),
                            self.parity == "none",
                            cx,
                        )
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.parity = "none".to_string();
                            cx.notify();
                        })),
                    )
                    .child(
                        Self::pill(
                            "parity-odd",
                            rust_i18n::t!("NewConnectionWindow.parity_odd"),
                            self.parity == "odd",
                            cx,
                        )
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.parity = "odd".to_string();
                            cx.notify();
                        })),
                    )
                    .child(
                        Self::pill("parity-even", rust_i18n::t!("NewConnectionWindow.parity_even"), self.parity == "even", cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.parity = "even".to_string();
                                cx.notify();
                            }),
                        ),
                    ),
            )
    }

    fn stop_bits_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label(rust_i18n::t!("NewConnectionWindow.stop_bits_label"), cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill("stop-bits-1", "1", self.stop_bits == 1, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.stop_bits = 1;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        Self::pill("stop-bits-2", "2", self.stop_bits == 2, cx).on_click(
                            cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                this.stop_bits = 2;
                                cx.notify();
                            }),
                        ),
                    ),
            )
    }

    fn flow_control_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label(rust_i18n::t!("NewConnectionWindow.flow_control_label"), cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        Self::pill(
                            "flow-none",
                            rust_i18n::t!("NewConnectionWindow.flow_none"),
                            self.flow_control == "none",
                            cx,
                        )
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.flow_control = "none".to_string();
                            cx.notify();
                        })),
                    )
                    .child(
                        Self::pill(
                            "flow-software",
                            rust_i18n::t!("NewConnectionWindow.flow_software"),
                            self.flow_control == "software",
                            cx,
                        )
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.flow_control = "software".to_string();
                            cx.notify();
                        })),
                    )
                    .child(
                        Self::pill(
                            "flow-hardware",
                            rust_i18n::t!("NewConnectionWindow.flow_hardware"),
                            self.flow_control == "hardware",
                            cx,
                        )
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.flow_control = "hardware".to_string();
                            cx.notify();
                        })),
                    ),
            )
    }

    fn serial_port_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.serial_port.clone();
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.field_label(rust_i18n::t!("NewConnectionWindow.serial_device_label"), cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().child(Input::new(&self.serial_port)))
                    .child(
                        DropdownButton::new("serial-port-picker")
                            .small()
                            .button(
                                Button::new("serial-port-picker-btn")
                                    .label(rust_i18n::t!("NewConnectionWindow.select_button")),
                            )
                            .dropdown_menu(move |menu, _window, _cx| {
                                let ports = crate::terminal::serial::list_ports();
                                if ports.is_empty() {
                                    return menu.label(rust_i18n::t!(
                                        "NewConnectionWindow.no_serial_ports_detected"
                                    ));
                                }
                                let mut menu = menu;
                                for path in ports {
                                    let target = target.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(path.clone()).on_click(
                                            move |_ev, window, cx| {
                                                let path = path.clone();
                                                target.update(cx, |s, cx| {
                                                    s.set_value(path, window, cx);
                                                });
                                            },
                                        ),
                                    );
                                }
                                menu
                            }),
                    ),
            )
    }

    fn render_ssh_auth_fields(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_key = self.auth_method == "key";
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(self.field_label(rust_i18n::t!("NewConnectionWindow.auth_method_label"), cx))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                Self::pill(
                                    "auth-password",
                                    rust_i18n::t!("NewConnectionWindow.auth_password"),
                                    !is_key,
                                    cx,
                                )
                                .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                    this.auth_method = "password".to_string();
                                    cx.notify();
                                })),
                            )
                            .child(
                                Self::pill(
                                    "auth-key",
                                    rust_i18n::t!("NewConnectionWindow.auth_key"),
                                    is_key,
                                    cx,
                                )
                                .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                                    this.auth_method = "key".to_string();
                                    cx.notify();
                                })),
                            ),
                    ),
            )
            .child(if is_key {
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(self.field_label(rust_i18n::t!("NewConnectionWindow.private_key_file_label"), cx))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .items_center()
                                    .child(div().flex_1().child(Input::new(&self.private_key_path)))
                                    .child(
                                        div()
                                            .id("browse-private-key")
                                            .px_2()
                                            .py_0p5()
                                            .rounded_sm()
                                            .bg(cx.theme().accent)
                                            .child(rust_i18n::t!("NewConnectionWindow.browse_button"))
                                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                                let path_input = this.private_key_path.clone();
                                                let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
                                                    files: true,
                                                    directories: false,
                                                    multiple: false,
                                                    prompt: None,
                                                });
                                                // `set_value` needs a live `&mut Window`, which a
                                                // plain `cx.spawn` async closure doesn't have
                                                // (only `AsyncApp`) — `cx.spawn_in(window, ...)`
                                                // gives an `AsyncWindowContext` instead, whose
                                                // `.update(|window, cx| ...)` hands back a real
                                                // `&mut Window` (confirmed against
                                                // `~/.cargo/git/checkouts/zed-a70e2ad075855582/1d217ee/crates/gpui/src/app/context.rs:676`
                                                // and `.../app/async_context.rs:299`).
                                                cx.spawn_in(window, async move |_this, cx| {
                                                    let Ok(Ok(Some(paths))) = rx.await else {
                                                        return;
                                                    };
                                                    let Some(path) = paths.into_iter().next() else {
                                                        return;
                                                    };
                                                    let _ = cx.update(|window, cx| {
                                                        path_input.update(cx, |s, cx| {
                                                            s.set_value(
                                                                path.to_string_lossy().to_string(),
                                                                window,
                                                                cx,
                                                            );
                                                        });
                                                    });
                                                })
                                                .detach();
                                            })),
                                    ),
                            ),
                    )
                    .child(self.field(
                        rust_i18n::t!("NewConnectionWindow.private_key_passphrase_placeholder"),
                        &self.private_key_passphrase.clone(),
                        cx,
                    ))
                    .into_any_element()
            } else {
                self.field(
                    rust_i18n::t!("NewConnectionWindow.password_placeholder"),
                    &self.password.clone(),
                    cx,
                )
                .into_any_element()
            })
    }
}

impl Render for NewConnectionWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let conn_type = self.conn_type.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .p_2()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .child(Self::pill("type-ssh", "SSH", conn_type == ConnectionType::Ssh, cx)
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.conn_type = ConnectionType::Ssh;
                            cx.notify();
                        })))
                    .child(
                        Self::pill(
                            "type-local",
                            rust_i18n::t!("NewConnectionWindow.type_local"),
                            conn_type == ConnectionType::Local,
                            cx,
                        )
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.conn_type = ConnectionType::Local;
                            cx.notify();
                        })),
                    )
                    .child(Self::pill("type-telnet", "Telnet", conn_type == ConnectionType::Telnet, cx)
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.conn_type = ConnectionType::Telnet;
                            cx.notify();
                        })))
                    .child(
                        Self::pill(
                            "type-serial",
                            rust_i18n::t!("NewConnectionWindow.type_serial"),
                            conn_type == ConnectionType::Serial,
                            cx,
                        )
                        .on_click(cx.listener(|this, _ev: &ClickEvent, _w, cx| {
                            this.conn_type = ConnectionType::Serial;
                            cx.notify();
                        })),
                    ),
            )
            .child(self.render_icon_picker(cx))
            .child(self.field(rust_i18n::t!("NewConnectionWindow.name_label"), &self.name.clone(), cx))
            .children(match conn_type {
                ConnectionType::Ssh => vec![
                    self.field(rust_i18n::t!("NewConnectionWindow.host_label"), &self.host.clone(), cx)
                        .into_any_element(),
                    self.field(
                        rust_i18n::t!("NewConnectionWindow.port_label_ssh"),
                        &self.port.clone(),
                        cx,
                    )
                    .into_any_element(),
                    self.field(rust_i18n::t!("NewConnectionWindow.user_label"), &self.user.clone(), cx)
                        .into_any_element(),
                    self.render_ssh_auth_fields(cx).into_any_element(),
                ],
                ConnectionType::Local => vec![
                    self.field(
                        rust_i18n::t!("NewConnectionWindow.shell_path_label"),
                        &self.shell_path.clone(),
                        cx,
                    )
                    .into_any_element(),
                    self.field(
                        rust_i18n::t!("NewConnectionWindow.working_dir_label"),
                        &self.working_dir.clone(),
                        cx,
                    )
                    .into_any_element(),
                ],
                ConnectionType::Telnet => vec![
                    self.field(rust_i18n::t!("NewConnectionWindow.host_label"), &self.host.clone(), cx)
                        .into_any_element(),
                    self.field(
                        rust_i18n::t!("NewConnectionWindow.port_label_telnet"),
                        &self.port.clone(),
                        cx,
                    )
                    .into_any_element(),
                ],
                ConnectionType::Serial => vec![
                    self.serial_port_field(cx).into_any_element(),
                    self.field(
                        rust_i18n::t!("NewConnectionWindow.baud_rate_label"),
                        &self.baud_rate.clone(),
                        cx,
                    )
                    .into_any_element(),
                    self.data_bits_field(cx).into_any_element(),
                    self.parity_field(cx).into_any_element(),
                    self.stop_bits_field(cx).into_any_element(),
                    self.flow_control_field(cx).into_any_element(),
                ],
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .justify_end()
                    .child(
                        div()
                            .id("newconn-cancel")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child(rust_i18n::t!("NewConnectionWindow.cancel"))
                            .on_click(cx.listener(|_this, _ev: &ClickEvent, window, _cx| {
                                window.remove_window();
                            })),
                    )
                    .child(
                        div()
                            .id("newconn-save")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .child(rust_i18n::t!("NewConnectionWindow.save"))
                            .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                                this.save(window, cx);
                            })),
                    ),
            )
    }
}
