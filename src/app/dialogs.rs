use gpui::{
    Anchor, AppContext as _, Bounds, Context, DragMoveEvent, ElementId, Empty, Entity,
    Focusable as _, FontWeight, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Point, Render, SharedString, Size, StatefulInteractiveElement as _,
    Styled as _, Window, div, point, prelude::FluentBuilder as _, px, size,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::ButtonVariants as _,
    dialog::Dialog,
    h_flex,
    input::Input,
    menu::{DropdownMenu as _, PopupMenuItem},
    progress::Progress,
    scroll::{Scrollbar, ScrollbarShow},
    v_flex,
};
use rust_i18n::t;

use crate::{
    Ashell,
    app::controls::{pointer_button, pointer_switch, ui_rems},
    session::config::AuthMethod,
    system::{RemoteProcess, format_bytes},
    text_encoding::{FILE_ENCODINGS, TERMINAL_ENCODINGS, TextEncoding},
};

#[derive(Clone)]
enum SftpEditorDrag {
    Move,
    Resize,
}

fn session_group_dropdown(
    id: impl Into<ElementId>,
    tab_index: isize,
    selected_group: String,
    connection_groups: Vec<String>,
    view: Entity<Ashell>,
) -> impl IntoElement {
    let display_group = if selected_group.trim().is_empty() {
        t!("ungrouped").to_string()
    } else {
        selected_group.clone()
    };

    pointer_button(id)
        .w_full()
        .outline()
        .dropdown_caret(true)
        .tab_index(tab_index)
        .label(display_group)
        .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, _| {
            let ungrouped_view = view.clone();
            let menu = menu.min_w(0.).item(
                PopupMenuItem::new(t!("ungrouped").to_string())
                    .checked(selected_group.trim().is_empty())
                    .on_click(window.listener_for(&ungrouped_view, |this, _, _, cx| {
                        this.set_session_group(String::new(), cx);
                    })),
            );

            connection_groups.iter().fold(menu, |menu, group| {
                let group_value = group.clone();
                let group_label = group.clone();
                let group_view = view.clone();
                menu.item(
                    PopupMenuItem::new(group_label)
                        .checked(group.eq_ignore_ascii_case(&selected_group))
                        .on_click(window.listener_for(&group_view, move |this, _, _, cx| {
                            this.set_session_group(group_value.clone(), cx);
                        })),
                )
            })
        })
}

impl Render for SftpEditorDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

impl Ashell {
    pub(crate) fn show_ssh_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_dialog.is_some() {
            return;
        }
        self.active_dialog = Some(crate::app::DialogKind::NewSsh);

        if let Some(id) = &self.editing_session_id {
            if let Some(session) = self.config.get(id) {
                self.session_protocol = session.protocol.clone();
            }
        } else {
            self.session_protocol = "ssh".to_string();
        }

        let initial_is_serial = self.session_protocol == "serial";
        let initial_is_editing = self.editing_session_id.is_some();
        let view = cx.entity();
        let session_name_input = self.session_name_input.clone();
        let host_input = self.host_input.clone();
        let focus_host_input = host_input.clone();
        let port_input = self.port_input.clone();
        let user_input = self.user_input.clone();
        let password_input = self.password_input.clone();
        let key_path_input = self.key_path_input.clone();
        let key_inline_input = self.key_inline_input.clone();
        let passphrase_input = self.passphrase_input.clone();
        let proxy_host_input = self.proxy_host_input.clone();
        let proxy_port_input = self.proxy_port_input.clone();
        let proxy_user_input = self.proxy_user_input.clone();
        let proxy_password_input = self.proxy_password_input.clone();
        let baud_rate_input = self.baud_rate_input.clone();

        window.open_dialog(cx, move |dialog: Dialog, _window, _cx| {
            dialog
                .title(match (initial_is_serial, initial_is_editing) {
                    (true, true) => t!("edit_serial_connection"),
                    (true, false) => t!("new_serial_connection"),
                    (false, true) => t!("edit_connection"),
                    (false, false) => t!("new_connection"),
                })
                .w(px(520.))
                .overlay_closable(true)
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.active_dialog = None;
                            cx.notify();
                        });
                    }
                })
                .content({
                    let view = view.clone();
                    let session_name_input = session_name_input.clone();
                    let host_input = host_input.clone();
                    let port_input = port_input.clone();
                    let user_input = user_input.clone();
                    let password_input = password_input.clone();
                    let key_path_input = key_path_input.clone();
                    let key_inline_input = key_inline_input.clone();
                    let passphrase_input = passphrase_input.clone();
                    let proxy_host_input = proxy_host_input.clone();
                    let proxy_port_input = proxy_port_input.clone();
                    let proxy_user_input = proxy_user_input.clone();
                    let proxy_password_input = proxy_password_input.clone();
                    let baud_rate_input = baud_rate_input.clone();
                    move |content, window, cx| {
                        let auth_method = view.read(cx).ssh_auth_method;
                        let is_password = auth_method == AuthMethod::Password;
                        let is_key = auth_method == AuthMethod::Key;
                        let is_config = auth_method == AuthMethod::Config;
                        let is_editing = view.read(cx).editing_session_id.is_some();
                        let proxy_type = view.read(cx).ssh_proxy_type.clone();
                        let show_proxy_fields = proxy_type != "none";
                        let protocol = view.read(cx).session_protocol.clone();
                        let is_ssh = protocol == "ssh";
                        let is_serial = protocol == "serial";
                        let terminal_encoding = view.read(cx).ssh_terminal_encoding;
                        let (session_group, connection_groups) = {
                            let this = view.read(cx);
                            (this.session_group.clone(), this.config.connection_groups())
                        };
                        content.child(
                            v_flex()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            pointer_button("proto-ssh")
                                                .label("SSH")
                                                .when(is_ssh, |button| button.primary())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.set_session_protocol("ssh".to_string(), cx);
                                                        Self::set_input_value(&this.port_input, "22", window, cx);
                                                    },
                                                )),
                                        )
                                        .child(
                                            pointer_button("proto-serial")
                                                .label("Serial")
                                                .when(is_serial, |button| button.primary())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, _window, cx| {
                                                        this.set_session_protocol("serial".to_string(), cx);
                                                    },
                                                )),
                                        ),
                                )
                                .when(is_serial, |this| {
                                    this.child(
                                        v_flex()
                                            .gap_1()
                                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(t!("session_name").to_string()))
                                            .child(Input::new(&session_name_input).tab_index(0))
                                    )
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(t!("connection_group").to_string()))
                                            .child(session_group_dropdown(
                                                "serial-connection-group-select",
                                                1,
                                                session_group.clone(),
                                                connection_groups.clone(),
                                                view.clone(),
                                            ))
                                    )
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(t!("serial_port").to_string()))
                                            .child(Input::new(&host_input).tab_index(2))
                                    )
                                    .child(
                                        v_flex()
                                            .gap_1()
                                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(t!("baud_rate").to_string()))
                                            .child(Input::new(&baud_rate_input).tab_index(3))
                                    )
                                })
                                .when(is_ssh, |this| {
                                    this.child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                pointer_button("ssh-auth-password")
                                                    .label(t!("password").to_string())
                                                    .when(is_password, |button| button.primary())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, _, cx| {
                                                            this.set_ssh_auth_method(
                                                                AuthMethod::Password,
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            )
                                            .child(
                                                pointer_button("ssh-auth-key")
                                                    .label(t!("key").to_string())
                                                    .when(is_key, |button| button.primary())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, _, cx| {
                                                            this.set_ssh_auth_method(
                                                                AuthMethod::Key,
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            )
                                            .child(
                                                pointer_button("ssh-auth-config")
                                                    .label(t!("ssh_config").to_string())
                                                    .when(is_config, |button| button.primary())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, _, cx| {
                                                            this.set_ssh_auth_method(
                                                                AuthMethod::Config,
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            ),
                                    )
                                    .when(!is_config, |this| {
                                        this.child(Input::new(&session_name_input).tab_index(0))
                                            .child(
                                                v_flex()
                                                    .gap_1()
                                                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(t!("connection_group").to_string()))
                                                    .child(session_group_dropdown(
                                                        "ssh-connection-group-select",
                                                        1,
                                                        session_group.clone(),
                                                        connection_groups.clone(),
                                                        view.clone(),
                                                    )),
                                            )
                                            .child(
                                                h_flex()
                                                    .w_full()
                                                    .gap_2()
                                                    .child(
                                                        Input::new(&host_input)
                                                            .flex_1()
                                                            .min_w(px(0.))
                                                            .tab_index(2),
                                                    )
                                                    .child(
                                                        Input::new(&port_input)
                                                            .w(px(96.))
                                                            .tab_index(3),
                                                    ),
                                            )
                                            .when(is_password, |this| {
                                                this.child(
                                                    h_flex()
                                                        .w_full()
                                                        .gap_2()
                                                        .child(
                                                            Input::new(&user_input)
                                                                .flex_1()
                                                                .min_w(px(0.))
                                                                .tab_index(4),
                                                        )
                                                        .child(
                                                            Input::new(&password_input)
                                                                .flex_1()
                                                                .min_w(px(0.))
                                                                .mask_toggle()
                                                                .tab_index(5),
                                                        ),
                                                )
                                            })
                                            .when(is_key, |this| {
                                                this.child(
                                                    Input::new(&user_input).w_full().tab_index(4),
                                                )
                                            })
                                    })
                                    .when(is_key, |this| {
                                        this.child(
                                            h_flex()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .cursor_pointer()
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            window.listener_for(
                                                                &view,
                                                                |this, _, window, cx| {
                                                                    this.pick_ssh_key_path(window, cx);
                                                                },
                                                            ),
                                                        )
                                                        .child(
                                                            Input::new(&key_path_input).tab_index(6),
                                                        ),
                                                )
                                                .child(
                                                    pointer_button("clear-key-path")
                                                        .ghost()
                                                        .icon(IconName::Close)
                                                        .on_click(window.listener_for(
                                                            &view,
                                                            |this, _, window, cx| {
                                                                Self::set_input_value(
                                                                    &this.key_path_input,
                                                                    "",
                                                                    window,
                                                                    cx,
                                                                );
                                                            },
                                                        )),
                                                ),
                                        )
                                        .child(Input::new(&key_inline_input).h(px(128.)).tab_index(7))
                                        .child(Input::new(&passphrase_input).mask_toggle().tab_index(8))
                                    })
                                    .when(is_config, |this| {
                                        let this = this.child(
                                            v_flex()
                                                .gap_1()
                                                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(t!("connection_group").to_string()))
                                                .child(session_group_dropdown(
                                                    "ssh-config-connection-group-select",
                                                    0,
                                                    session_group.clone(),
                                                    connection_groups.clone(),
                                                    view.clone(),
                                                )),
                                        );
                                        let entries = view.read(cx).ssh_config_entries.clone();
                                        let selected = view.read(cx).ssh_config_selected;
                                        let theme = cx.theme();
                                        if entries.is_empty() {
                                            this.child(
                                                div()
                                                    .text_sm()
                                                    .text_color(theme.muted_foreground)
                                                    .child(t!("ssh_config_empty").to_string()),
                                            )
                                        } else {
                                            this.child(
                                                div()
                                                    .h(px(192.))
                                                    .id("ssh-config-list")
                                                    .track_scroll(
                                                        &view.read(cx).connection_scroll_handle,
                                                    )
                                                    .overflow_y_scroll()
                                                    .border_1()
                                                    .border_color(theme.border)
                                                    .rounded_md()
                                                    .children(entries.iter().enumerate().map(
                                                        |(i, entry)| {
                                                            let is_selected = selected == Some(i);
                                                            let label = if entry.user.is_empty() {
                                                                format!(
                                                                    "{}:{}",
                                                                    entry.hostname, entry.port
                                                                )
                                                            } else {
                                                                format!(
                                                                    "{}@{}:{}",
                                                                    entry.user,
                                                                    entry.hostname,
                                                                    entry.port
                                                                )
                                                            };
                                                            let alias_label =
                                                                if entry.host_alias == entry.hostname {
                                                                    String::new()
                                                                } else {
                                                                    format!(" ({})", entry.host_alias)
                                                                };
                                                            let view_clone = view.clone();
                                                            div()
                                                                .id(("ssh-config-entry", i))
                                                                .px_2()
                                                                .py_1()
                                                                .when(is_selected, |el| {
                                                                    el.bg(theme.selection)
                                                                })
                                                                .cursor_pointer()
                                                                .hover(|el| el.bg(theme.selection))
                                                                .text_sm()
                                                                .child(format!("{label}{alias_label}"))
                                                                .on_click(window.listener_for(
                                                                    &view_clone,
                                                                    move |this, _, window, cx| {
                                                                        this.select_ssh_config_entry(
                                                                            i, window, cx,
                                                                        );
                                                                    },
                                                                ))
                                                        },
                                                    )),
                                            )
                                        }
                                    })
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .text_sm()
                                                    .font_weight(FontWeight::BOLD)
                                                    .child(t!("terminal_encoding").to_string()),
                                            )
                                            .child(
                                                pointer_button("ssh-terminal-encoding")
                                                    .ghost()
                                                    .icon(IconName::Globe)
                                                    .label(terminal_encoding.label())
                                                    .dropdown_menu_with_anchor(
                                                        Anchor::BottomRight,
                                                        {
                                                            let view = view.clone();
                                                            move |menu, window, _| {
                                                                TERMINAL_ENCODINGS
                                                                    .iter()
                                                                    .copied()
                                                                    .fold(
                                                                        menu.min_w(0.),
                                                                        |menu, candidate| {
                                                                            menu.item(
                                                                                PopupMenuItem::new(
                                                                                    candidate
                                                                                        .label(),
                                                                                )
                                                                                .checked(
                                                                                    candidate
                                                                                        == terminal_encoding,
                                                                                )
                                                                                .on_click(
                                                                                    window.listener_for(
                                                                                        &view,
                                                                                        move |this,
                                                                                              _,
                                                                                              _,
                                                                                              cx| {
                                                                                            this.set_ssh_terminal_encoding(
                                                                                                candidate,
                                                                                                cx,
                                                                                            );
                                                                                        },
                                                                                    ),
                                                                                ),
                                                                            )
                                                                        },
                                                                    )
                                                            }
                                                        },
                                                    ),
                                            ),
                                    )
                                    .when(!is_config, |this| {
                                        this.child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::BOLD)
                                                .child(t!("proxy").to_string()),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .child(
                                                    pointer_button("proxy-none")
                                                        .label(t!("proxy_none").to_string())
                                                        .when(proxy_type == "none", |button| {
                                                            button.primary()
                                                        })
                                                        .on_click(window.listener_for(
                                                            &view,
                                                            |this, _, _, cx| {
                                                                this.set_ssh_proxy_type(
                                                                    "none".to_string(),
                                                                    cx,
                                                                )
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    pointer_button("proxy-socks5")
                                                        .label("SOCKS5")
                                                        .when(proxy_type == "socks5", |button| {
                                                            button.primary()
                                                        })
                                                        .on_click(window.listener_for(
                                                            &view,
                                                            |this, _, _, cx| {
                                                                this.set_ssh_proxy_type(
                                                                    "socks5".to_string(),
                                                                    cx,
                                                                )
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    pointer_button("proxy-http")
                                                        .label("HTTP")
                                                        .when(proxy_type == "http", |button| {
                                                            button.primary()
                                                        })
                                                        .on_click(window.listener_for(
                                                            &view,
                                                            |this, _, _, cx| {
                                                                this.set_ssh_proxy_type(
                                                                    "http".to_string(),
                                                                    cx,
                                                                )
                                                            },
                                                        )),
                                                ),
                                        )
                                        .when(
                                            show_proxy_fields,
                                            |this| {
                                                this.child(
                                                    h_flex()
                                                        .gap_2()
                                                        .child(Input::new(&proxy_host_input).flex_1())
                                                        .child(
                                                            Input::new(&proxy_port_input).w(px(96.)),
                                                        ),
                                                )
                                                .child(
                                                    h_flex()
                                                        .gap_2()
                                                        .child(Input::new(&proxy_user_input).flex_1())
                                                        .child(
                                                            Input::new(&proxy_password_input).flex_1(),
                                                        ),
                                                )
                                            },
                                        )
                                    })
                                })
                                .child(
                                    h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            pointer_button("connect-ssh-cancel")
                                                .label(t!("cancel").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.active_dialog = None;
                                                        window.close_dialog(cx);
                                                        cx.notify();
                                                    },
                                                )),
                                        )
                                        .when(!is_config, |this| {
                                            this.child(
                                                pointer_button("connect-ssh-confirm")
                                                    .primary()
                                                    .label(if is_editing {
                                                        t!("save")
                                                    } else {
                                                        t!("connect")
                                                    })
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, window, cx| {
                                                            this.connect_ssh(window, cx)
                                                        },
                                                    )),
                                            )
                                        }),
                                ),
                        )
                    }
                })
        });
        window.defer(cx, move |window, cx| {
            window.focus(&focus_host_input.read(cx).focus_handle(cx), cx);
        });
    }
    pub(crate) fn show_selector_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_dialog.is_some() {
            return;
        }
        self.active_dialog = Some(crate::app::DialogKind::SessionSelector);

        let view = cx.entity();
        let selector_focus_handle = self.selector_focus_handle.clone();
        let deferred_selector_focus_handle = selector_focus_handle.clone();
        let sessions = self.config.sessions().to_vec();
        let active_session_id = self.active_session_id().map(ToOwned::to_owned);
        self.selector_selection = self.default_selector_index();
        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("open_session").to_string())
                .w(px(520.))
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.active_dialog = None;
                            cx.notify();
                        });
                    }
                })
                .on_ok({
                    let view = view.clone();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            this.activate_selector_selection(window, cx);
                        });
                        false
                    }
                })
                .content({
                    let view = view.clone();
                    let sessions = sessions.clone();
                    let _active_session_id = active_session_id.clone();
                    let selector_focus_handle = selector_focus_handle.clone();
                    move |content, window, _cx| {
                        let selected_index = view.read(_cx).selector_selection;
                        let scroll_handle = view.read(_cx).selector_scroll_handle.clone();
                        content.child(
                            v_flex()
                                .track_focus(&selector_focus_handle)
                                .on_key_down(window.listener_for(
                                    &view,
                                    |this, event, window, cx| {
                                        this.on_selector_key_down(event, window, cx)
                                    },
                                ))
                                .gap_2()
                                .child(
                                    div()
                                        .w_full()
                                        .p_2()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(if selected_index == 0 {
                                            _cx.theme().primary
                                        } else {
                                            _cx.theme().border
                                        })
                                        .bg(if selected_index == 0 {
                                            _cx.theme().tab_active
                                        } else {
                                            _cx.theme().muted
                                        })
                                        .cursor_pointer()
                                        .hover(|this| this.bg(_cx.theme().secondary))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            window.listener_for(&view, |this, _, window, cx| {
                                                this.active_dialog = None;
                                                this.open_local(cx);
                                                window.close_dialog(cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_size(ui_rems(1.0))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(t!("local_terminal")),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(ui_rems(0.917))
                                                        .text_color(_cx.theme().muted_foreground)
                                                        .child(t!("open_local_shell_tab")),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .p_2()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(if selected_index == 1 {
                                            _cx.theme().primary
                                        } else {
                                            _cx.theme().border
                                        })
                                        .bg(if selected_index == 1 {
                                            _cx.theme().tab_active
                                        } else {
                                            _cx.theme().muted
                                        })
                                        .cursor_pointer()
                                        .hover(|this| this.bg(_cx.theme().secondary))
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            window.listener_for(&view, |this, _, window, cx| {
                                                this.active_dialog = None;
                                                window.close_dialog(cx);
                                                this.open_new_ssh_dialog(window, cx);
                                                cx.notify();
                                            }),
                                        )
                                        .child(
                                            v_flex()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_size(ui_rems(1.0))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(t!("new_connection")),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(ui_rems(0.917))
                                                        .text_color(_cx.theme().muted_foreground)
                                                        .child(t!("create_or_edit_ssh_session")),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .max_h(px(320.))
                                        .size_full()
                                        .child(
                                            v_flex()
                                                .size_full()
                                                .id("selector-scroll-view")
                                                .track_scroll(&scroll_handle)
                                                .overflow_y_scroll()
                                                .gap_2()
                                                .children(
                                                    sessions.clone().into_iter().enumerate().map(
                                                        |(ix, session)| {
                                                            let connect_id = session.id.clone();
                                                            let is_selected =
                                                                selected_index == ix + 2;
                                                            let name = session.name.clone();
                                                            let detail = if session.protocol
                                                                == "serial"
                                                            {
                                                                format!(
                                                                    "Serial: {}@{}",
                                                                    session.host, session.baud_rate
                                                                )
                                                            } else {
                                                                format!(
                                                                    "{}@{}:{}",
                                                                    session.user,
                                                                    session.host,
                                                                    session.port
                                                                )
                                                            };
                                                            div()
                                                    .id(("selector-open", ix))
                                                    .w_full()
                                                    .p_2()
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(if is_selected {
                                                        _cx.theme().primary
                                                    } else {
                                                        _cx.theme().border
                                                    })
                                                    .bg(if is_selected {
                                                        _cx.theme().tab_active
                                                    } else {
                                                        _cx.theme().muted
                                                    })
                                                    .cursor_pointer()
                                                    .hover(|this| this.bg(_cx.theme().secondary))
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        window.listener_for(
                                                            &view,
                                                            move |this, _, window, cx| {
                                                                this.active_dialog = None;
                                                                this.connect_saved_session(
                                                                    connect_id.clone(),
                                                                    window,
                                                                    cx,
                                                                );
                                                                window.close_dialog(cx);
                                                                cx.notify();
                                                            },
                                                        ),
                                                    )
                                                    .child(
                                                        v_flex()
                                                            .gap_1()
                                                            .child(
                                                                div()
                                                                    .text_size(ui_rems(1.0))
                                                                    .font_weight(
                                                                        FontWeight::SEMIBOLD,
                                                                    )
                                                                    .child(name),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(ui_rems(0.917))
                                                                    .text_color(
                                                                        _cx.theme()
                                                                            .muted_foreground,
                                                                    )
                                                                    .child(detail),
                                                            ),
                                                    )
                                                        },
                                                    ),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .bottom_0()
                                                .left_0()
                                                .right_0()
                                                .child(
                                                gpui_component::scroll::Scrollbar::new(
                                                    &scroll_handle,
                                                )
                                                .id("selector-scrollbar")
                                                .axis(
                                                    gpui_component::scroll::ScrollbarAxis::Vertical,
                                                )
                                                .scrollbar_show(
                                                    gpui_component::scroll::ScrollbarShow::Always,
                                                ),
                                            ),
                                        ),
                                ),
                        )
                    }
                })
        });
        window.defer(cx, move |window, cx| {
            window.focus(&deferred_selector_focus_handle, cx);
        });
    }
    pub(crate) fn show_transfers_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_dialog.is_some() {
            return;
        }
        self.active_dialog = Some(crate::app::DialogKind::Transfers);

        let view = cx.entity();
        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .w(px(600.))
                .close_button(false)
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.active_dialog = None;
                            cx.notify();
                        });
                    }
                })
                .content({
                    let view = view.clone();
                    move |content, window, cx| {
                        let can_clear = view.read(cx).transfers.iter().any(|t| {
                            !matches!(
                                t.state,
                                crate::terminal::TransferState::Running
                                    | crate::terminal::TransferState::Paused
                            )
                        });

                        let clear_btn = if can_clear {
                            Some(
                                pointer_button("clear_transfers_btn")
                                    .ghost()
                                    .icon(IconName::Delete)
                                    .label(t!("clear_transfers").to_string())
                                    .on_click(window.listener_for(&view, |this, _, _, cx| {
                                        this.transfers.retain(|t| {
                                            matches!(
                                                t.state,
                                                crate::terminal::TransferState::Running
                                                    | crate::terminal::TransferState::Paused
                                            )
                                        });
                                        this.config.set_transfers(this.transfers.clone());
                                        cx.notify();
                                    })),
                            )
                        } else {
                            None
                        };

                        let header = h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .child(
                                h_flex()
                                    .items_baseline()
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(t!("transfers").to_string()),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .ml_2()
                                            .child(t!("transfers_limit").to_string()),
                                    ),
                            )
                            .child(
                                h_flex().gap_2().children(clear_btn).child(
                                    pointer_button("close_dialog")
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(window.listener_for(
                                            &view,
                                            |this, _, window, cx| {
                                                this.active_dialog = None;
                                                window.close_dialog(cx);
                                                cx.notify();
                                            },
                                        )),
                                ),
                            );

                        let mut transfers = view.read(cx).transfers.clone();
                        transfers.sort_by_key(|t| match t.state {
                            crate::terminal::TransferState::Running
                            | crate::terminal::TransferState::Paused => 0,
                            _ => 1,
                        });

                        if transfers.is_empty() {
                            return content.child(
                                v_flex().gap_2().child(header).child(
                                    div()
                                        .p_4()
                                        .text_center()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("no_transfers_yet").to_string()),
                                ),
                            );
                        }
                        let list = v_flex().gap_2().children(transfers.into_iter().map(|t| {
                            let (icon, _color) = match t.info.kind {
                                crate::terminal::TransferType::Upload => {
                                    (IconName::ArrowUp, cx.theme().primary)
                                }
                                crate::terminal::TransferType::Download => {
                                    (IconName::ArrowDown, cx.theme().success)
                                }
                            };

                            let (status_text, actions) =
                                match t.state {
                                    crate::terminal::TransferState::Running => {
                                        let percent = t
                                            .total
                                            .map(|tot| {
                                                (t.transferred as f64 / tot as f64 * 100.0)
                                                    .clamp(0.0, 100.0)
                                            })
                                            .unwrap_or(0.0);
                                        let txt = if let Some(tot) = t.total {
                                            format!(
                                                "{:.1}% ({}/{})",
                                                percent,
                                                format_bytes(t.transferred),
                                                format_bytes(tot)
                                            )
                                        } else {
                                            match t.info.kind {
                                                crate::terminal::TransferType::Upload => {
                                                    format!("{}...", t!("uploading"))
                                                }
                                                crate::terminal::TransferType::Download => {
                                                    format!("{}...", t!("downloading"))
                                                }
                                            }
                                        };
                                        let btn_pause = pointer_button(SharedString::from(
                                            format!("pause-{}", t.info.id),
                                        ))
                                        .ghost()
                                        .icon(IconName::Pause)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            move |this, _, _, _| {
                                                if let Some(handle) = this.active_sftp_handle() {
                                                    handle.pause_transfer(id.clone());
                                                }
                                            }
                                        }));
                                        let btn_cancel = pointer_button(SharedString::from(
                                            format!("cancel-{}", t.info.id),
                                        ))
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            move |this, _, _, _| {
                                                if let Some(handle) = this.active_sftp_handle() {
                                                    handle.cancel_transfer(id.clone());
                                                }
                                            }
                                        }));
                                        (txt, h_flex().gap_1().child(btn_pause).child(btn_cancel))
                                    }
                                    crate::terminal::TransferState::Paused => {
                                        let txt = t!("paused").to_string();
                                        let btn_resume = pointer_button(SharedString::from(
                                            format!("resume-{}", t.info.id),
                                        ))
                                        .ghost()
                                        .icon(IconName::Play)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            move |this, _, _, _| {
                                                if let Some(handle) = this.active_sftp_handle() {
                                                    handle.resume_transfer(id.clone());
                                                }
                                            }
                                        }));
                                        let btn_cancel = pointer_button(SharedString::from(
                                            format!("cancel-{}", t.info.id),
                                        ))
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            move |this, _, _, _| {
                                                if let Some(handle) = this.active_sftp_handle() {
                                                    handle.cancel_transfer(id.clone());
                                                }
                                            }
                                        }));
                                        (txt, h_flex().gap_1().child(btn_resume).child(btn_cancel))
                                    }
                                    crate::terminal::TransferState::Interrupted(ref reason) => {
                                        let txt = format!("{}: {}", t!("interrupted"), reason);
                                        let btn_remove = pointer_button(SharedString::from(
                                            format!("remove-{}", t.info.id),
                                        ))
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            move |this, _, _, cx| {
                                                this.remove_transfer(&id, cx);
                                            }
                                        }));
                                        (txt, h_flex().gap_1().child(btn_remove))
                                    }
                                    crate::terminal::TransferState::Completed => {
                                        let txt = t!("completed").to_string();
                                        let mut actions = h_flex().gap_1();
                                        if matches!(
                                            t.info.kind,
                                            crate::terminal::TransferType::Download
                                        ) {
                                            let btn_folder = pointer_button(SharedString::from(
                                                format!("folder-{}", t.info.id),
                                            ))
                                            .ghost()
                                            .icon(IconName::Folder)
                                            .on_click({
                                                let target = t.info.target.clone();
                                                move |_, _, _| {
                                                    let _ = std::process::Command::new("open")
                                                        .arg(&target)
                                                        .spawn();
                                                }
                                            });
                                            actions = actions.child(btn_folder);
                                        }
                                        let btn_remove = pointer_button(SharedString::from(
                                            format!("remove-{}", t.info.id),
                                        ))
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            move |this, _, _, cx| {
                                                this.remove_transfer(&id, cx);
                                            }
                                        }));
                                        actions = actions.child(btn_remove);
                                        (txt, actions)
                                    }
                                    crate::terminal::TransferState::Failed(ref err) => {
                                        let txt = format!("{}: {}", t!("failed"), err);
                                        let btn_remove = pointer_button(SharedString::from(
                                            format!("remove-{}", t.info.id),
                                        ))
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            move |this, _, _, cx| {
                                                this.remove_transfer(&id, cx);
                                            }
                                        }));
                                        (txt, h_flex().gap_1().child(btn_remove))
                                    }
                                    crate::terminal::TransferState::Zombie(ref reason) => {
                                        let txt = format!("{}: {}", t!("zombie"), reason);
                                        let btn_remove = pointer_button(SharedString::from(
                                            format!("remove-{}", t.info.id),
                                        ))
                                        .ghost()
                                        .icon(IconName::Close)
                                        .on_click(window.listener_for(&view, {
                                            let id = t.info.id.clone();
                                            move |this, _, _, cx| {
                                                this.remove_transfer(&id, cx);
                                            }
                                        }));
                                        (txt, h_flex().gap_1().child(btn_remove))
                                    }
                                };

                            let percent = match t.state {
                                crate::terminal::TransferState::Completed => 100.0,
                                _ => t
                                    .total
                                    .map(|tot| t.transferred as f64 / tot as f64 * 100.0)
                                    .unwrap_or(0.0),
                            };

                            v_flex()
                                .gap_1()
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().muted)
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            pointer_button(SharedString::from(format!(
                                                "icon-{}",
                                                t.info.id
                                            )))
                                            .icon(icon)
                                            .ghost()
                                            .disabled(true),
                                        )
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .overflow_hidden()
                                                .child(
                                                    div()
                                                        .text_size(px(12.))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(cx.theme().foreground)
                                                        .overflow_hidden()
                                                        .child(t.info.name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(10.))
                                                        .text_color(cx.theme().muted_foreground)
                                                        .overflow_hidden()
                                                        .child(format!(
                                                            "{}: {}",
                                                            t!("session"),
                                                            t.tab_title
                                                        )),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(11.))
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(status_text.clone()),
                                                ),
                                        )
                                        .child(actions),
                                )
                                .when(
                                    matches!(
                                        t.state,
                                        crate::terminal::TransferState::Running
                                            | crate::terminal::TransferState::Paused
                                    ),
                                    |this| {
                                        this.child(
                                            Progress::new(format!("progress-{}", t.info.id))
                                                .with_size(px(4.))
                                                .value(percent as f32)
                                                .color(cx.theme().primary)
                                                .w_full(),
                                        )
                                    },
                                )
                        }));

                        let scroll_handle = window
                            .use_keyed_state("transfers-scroll", cx, |_, _| {
                                gpui::ScrollHandle::default()
                            })
                            .read(cx)
                            .clone();

                        content.child(
                            v_flex().gap_2().child(header).child(
                                div()
                                    .w_full()
                                    .relative()
                                    .child(
                                        div()
                                            .w_full()
                                            .max_h(px(400.))
                                            .flex_col()
                                            .id("transfers-scroll-view")
                                            .track_scroll(&scroll_handle)
                                            .overflow_y_scroll()
                                            .pr(px(14.))
                                            .child(list),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .top_0()
                                            .right_0()
                                            .bottom_0()
                                            .w(px(16.))
                                            .child(
                                                Scrollbar::vertical(&scroll_handle)
                                                    .scrollbar_show(ScrollbarShow::Always),
                                            ),
                                    ),
                            ),
                        )
                    }
                })
        });
    }
    pub(crate) fn show_delete_confirm_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity();
        let selected_entries = self
            .active_sftp()
            .map(|s| s.selected_entries.clone())
            .unwrap_or_default();
        if selected_entries.is_empty() {
            return;
        }

        let has_system_path = selected_entries.iter().any(|path| {
            let p = path.as_str();
            p.starts_with("/bin/")
                || p == "/bin"
                || p.starts_with("/etc/")
                || p == "/etc"
                || p.starts_with("/usr/")
                || p == "/usr"
                || p.starts_with("/var/")
                || p == "/var"
                || p.starts_with("/sys/")
                || p == "/sys"
                || p.starts_with("/dev/")
                || p == "/dev"
                || p.starts_with("/boot/")
                || p == "/boot"
                || p.starts_with("/lib/")
                || p == "/lib"
                || p.starts_with("/opt/")
                || p == "/opt"
                || p.starts_with("/run/")
                || p == "/run"
                || p.starts_with("/sbin/")
                || p == "/sbin"
        });

        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("confirm_delete").to_string())
                .w(px(500.))
                .keyboard(false)
                .on_ok({
                    let view = view.clone();
                    let paths_to_delete: Vec<String> =
                        selected_entries.clone().into_iter().collect();
                    move |_, window, cx| {
                        view.update(cx, |this, cx| {
                            if let Some(handle) = this.active_sftp_handle() {
                                let _ = handle.commands.send(
                                    crate::sftp::SftpCommand::DeletePaths(paths_to_delete.clone()),
                                );
                            }
                            if let Some(sftp) = this.active_sftp_mut() {
                                sftp.selected_entries.clear();
                            }
                            cx.notify();
                        });
                        window.close_dialog(cx);
                        true
                    }
                })
                .content({
                    let view = view.clone();
                    move |content, _window, cx| {
                        let scroll_handle = view.read(cx).sftp_delete_scroll_handle.clone();
                        let selected_paths: Vec<String> = view
                            .read(cx)
                            .active_sftp()
                            .map(|s| s.selected_entries.clone().into_iter().collect())
                            .unwrap_or_default();

                        let warning_block = if has_system_path {
                            Some(
                                div()
                                    .w_full()
                                    .p_3()
                                    .mb_3()
                                    .rounded_md()
                                    .bg(gpui::rgba(0xff00001a))
                                    .border_1()
                                    .border_color(gpui::rgba(0xff000080))
                                    .child(
                                        div()
                                            .text_color(gpui::rgba(0xff0000ff))
                                            .font_weight(FontWeight::BOLD)
                                            .child(t!("system_path_warning").to_string()),
                                    ),
                            )
                        } else {
                            None
                        };

                        let paths_list = div()
                            .relative()
                            .max_h(px(200.))
                            .w_full()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .rounded_md()
                            .child(
                                v_flex()
                                    .id("delete-scroll-view")
                                    .size_full()
                                    .track_scroll(&scroll_handle)
                                    .overflow_y_scroll()
                                    .p_2()
                                    .gap_1()
                                    .children(selected_paths.into_iter().map(|path| {
                                        div()
                                            .text_size(ui_rems(0.917))
                                            .text_color(cx.theme().muted_foreground)
                                            .child(path)
                                    })),
                            )
                            .child(
                                div().absolute().top_0().bottom_0().right_0().child(
                                    gpui_component::scroll::Scrollbar::vertical(&scroll_handle)
                                        .scrollbar_show(
                                            gpui_component::scroll::ScrollbarShow::Always,
                                        ),
                                ),
                            );

                        content.child(
                            v_flex()
                                .w_full()
                                .gap_2()
                                .children(warning_block)
                                .child(
                                    div().text_size(ui_rems(1.0)).mb_2().child(
                                        t!(
                                            "confirm_delete_desc",
                                            count = view
                                                .read(cx)
                                                .active_sftp()
                                                .map(|s| s.selected_entries.len())
                                                .unwrap_or(0)
                                        )
                                        .to_string(),
                                    ),
                                )
                                .child(paths_list),
                        )
                    }
                })
                .footer({
                    let view = view.clone();
                    let paths_to_delete: Vec<String> =
                        selected_entries.clone().into_iter().collect();
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            pointer_button("cancel")
                                .ghost()
                                .label(t!("cancel").to_string())
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            pointer_button("confirm")
                                .danger()
                                .label(t!("confirm").to_string())
                                .on_click({
                                    let view = view.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            if let Some(handle) = this.active_sftp_handle() {
                                                let _ = handle.commands.send(
                                                    crate::sftp::SftpCommand::DeletePaths(
                                                        paths_to_delete.clone(),
                                                    ),
                                                );
                                            }
                                            if let Some(sftp) = this.active_sftp_mut() {
                                                sftp.selected_entries.clear();
                                            }
                                            cx.notify();
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                })
        });
    }

    pub(crate) fn show_terminate_process_dialog(
        &mut self,
        tab_id: String,
        process: RemoteProcess,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if process.pid <= 1
            || self.system_tab_id.as_deref() != Some(tab_id.as_str())
            || !self.tabs.iter().any(|tab| {
                tab.id == tab_id && tab.kind == crate::terminal::TabKind::Ssh && tab.connected
            })
        {
            return;
        }

        let view = cx.entity();
        let pid = process.pid;
        let process_name = process.command;
        let process_user = process.user;

        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("confirm_terminate_process").to_string())
                .w(px(480.))
                .keyboard(false)
                .content({
                    let process_name = process_name.clone();
                    let process_user = process_user.clone();
                    move |content, _window, cx| {
                        content.child(
                            v_flex()
                                .w_full()
                                .gap_3()
                                .child(
                                    div()
                                        .w_full()
                                        .whitespace_normal()
                                        .line_clamp(3)
                                        .text_size(ui_rems(0.917))
                                        .child(
                                            t!("confirm_terminate_process_desc", pid = pid)
                                                .to_string(),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .w_full()
                                        .gap_1()
                                        .p_3()
                                        .rounded_md()
                                        .bg(cx.theme().muted)
                                        .child(
                                            div()
                                                .w_full()
                                                .truncate()
                                                .text_size(ui_rems(0.833))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(process_name.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_size(ui_rems(0.75))
                                                .text_color(cx.theme().muted_foreground)
                                                .child(
                                                    t!(
                                                        "process_summary",
                                                        name = process_user.as_str(),
                                                        pid = pid
                                                    )
                                                    .to_string(),
                                                ),
                                        ),
                                ),
                        )
                    }
                })
                .footer({
                    let view = view.clone();
                    let tab_id = tab_id.clone();
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            pointer_button("terminate-process-cancel")
                                .ghost()
                                .label(t!("cancel").to_string())
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            pointer_button("terminate-process-confirm")
                                .danger()
                                .label(t!("terminate_process").to_string())
                                .on_click(move |_, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.terminate_remote_process(tab_id.clone(), pid, cx);
                                    });
                                    window.close_dialog(cx);
                                }),
                        )
                })
        });
    }

    pub(crate) fn prompt_active_ssh_reconnect_if_needed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.active_tab.as_ref().and_then(|active_tab_id| {
            self.tabs
                .iter()
                .find(|tab| {
                    tab.id == *active_tab_id
                        && tab.kind == crate::terminal::TabKind::Ssh
                        && !tab.connected
                        && tab.disconnected_reason.is_some()
                        && tab.session.is_some()
                })
                .map(|tab| tab.id.clone())
        }) else {
            return;
        };

        self.show_ssh_reconnect_dialog(tab_id, window, cx);
    }

    pub(crate) fn show_ssh_reconnect_dialog(
        &mut self,
        tab_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_dialog.is_some()
            || self.active_tab.as_deref() != Some(tab_id.as_str())
            || !self.tabs.iter().any(|tab| {
                tab.id == tab_id
                    && tab.kind == crate::terminal::TabKind::Ssh
                    && !tab.connected
                    && tab.disconnected_reason.is_some()
                    && tab.session.is_some()
            })
        {
            return;
        }

        let session_name = self
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| tab.title.clone())
            .unwrap_or_else(|| tab_id.clone());
        let view = cx.entity();
        self.active_dialog = Some(crate::app::DialogKind::SshReconnect);

        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("reconnect_ssh_title").to_string())
                .w(px(400.))
                .keyboard(false)
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.active_dialog == Some(crate::app::DialogKind::SshReconnect) {
                                this.active_dialog = None;
                            }
                            cx.notify();
                        });
                    }
                })
                .content({
                    let session_name = session_name.clone();
                    move |content, _window, _cx| {
                        content.child(
                            div()
                                .w_full()
                                .whitespace_normal()
                                .text_size(ui_rems(0.917))
                                .child(
                                    t!("reconnect_ssh_desc", name = session_name.as_str())
                                        .to_string(),
                                ),
                        )
                    }
                })
                .footer({
                    let view = view.clone();
                    let tab_id = tab_id.clone();
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            pointer_button("ssh-reconnect-cancel")
                                .ghost()
                                .label(t!("cancel").to_string())
                                .on_click({
                                    let view = view.clone();
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            if this.active_dialog
                                                == Some(crate::app::DialogKind::SshReconnect)
                                            {
                                                this.active_dialog = None;
                                            }
                                            cx.notify();
                                        });
                                        window.close_dialog(cx);
                                    }
                                }),
                        )
                        .child(
                            pointer_button("ssh-reconnect-confirm")
                                .primary()
                                .label(t!("reconnect_ssh").to_string())
                                .on_click(move |_, window, cx| {
                                    view.update(cx, |this, cx| {
                                        if this.active_dialog
                                            == Some(crate::app::DialogKind::SshReconnect)
                                        {
                                            this.active_dialog = None;
                                        }
                                        this.retry_disconnected_tab(&tab_id, cx);
                                    });
                                    window.close_dialog(cx);
                                }),
                        )
                })
        });
    }

    pub(crate) fn show_connection_group_dialog(
        &mut self,
        group: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_dialog.is_some() {
            return;
        }
        let editing_group = group.clone();
        let is_editing = editing_group.is_some();
        let existing_groups = self.config.connection_groups();
        self.active_dialog = Some(crate::app::DialogKind::ConnectionGroup);
        self.editing_connection_group = editing_group.clone();
        Self::set_input_value(
            &self.connection_group_name_input,
            group.unwrap_or_default(),
            window,
            cx,
        );

        let view = cx.entity();
        let group_input = self.connection_group_name_input.clone();
        let focus_input = group_input.clone();
        window.open_dialog(cx, move |dialog: Dialog, _window, _cx| {
            dialog
                .title(if is_editing {
                    t!("rename_connection_group").to_string()
                } else {
                    t!("new_connection_group").to_string()
                })
                .w(px(360.))
                .close_button(false)
                .overlay_closable(false)
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.active_dialog == Some(crate::app::DialogKind::ConnectionGroup) {
                                this.active_dialog = None;
                                this.editing_connection_group = None;
                            }
                            cx.notify();
                        });
                    }
                })
                .content({
                    let view = view.clone();
                    let group_input = group_input.clone();
                    let editing_group = editing_group.clone();
                    let existing_groups = existing_groups.clone();
                    move |content, window, cx| {
                        let name = group_input.read(cx).value().trim().to_string();
                        let duplicate = existing_groups.iter().any(|group| {
                            group.eq_ignore_ascii_case(&name)
                                && editing_group
                                    .as_ref()
                                    .is_none_or(|old| !group.eq_ignore_ascii_case(old))
                        });
                        let unchanged = editing_group.as_ref().is_some_and(|old| old == &name);
                        let error = if name.is_empty() {
                            Some(t!("group_name_required").to_string())
                        } else if duplicate {
                            Some(t!("group_name_exists").to_string())
                        } else {
                            None
                        };

                        content.child(
                            v_flex()
                                .gap_3()
                                .child(Input::new(&group_input).w_full().tab_index(0))
                                .when_some(error.clone(), |this, message| {
                                    this.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().danger)
                                            .child(message),
                                    )
                                })
                                .child(
                                    h_flex()
                                        .w_full()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            pointer_button("connection-group-cancel")
                                                .ghost()
                                                .label(t!("cancel").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.active_dialog = None;
                                                        this.editing_connection_group = None;
                                                        window.close_dialog(cx);
                                                        cx.notify();
                                                    },
                                                )),
                                        )
                                        .child(
                                            pointer_button("connection-group-save")
                                                .primary()
                                                .label(t!("save").to_string())
                                                .disabled(error.is_some() || unchanged)
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.submit_connection_group_dialog(
                                                            window, cx,
                                                        );
                                                    },
                                                )),
                                        ),
                                ),
                        )
                    }
                })
        });
        window.defer(cx, move |window, cx| {
            focus_input.read(cx).focus_handle(cx).focus(window, cx);
        });
    }

    pub(crate) fn submit_connection_group_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .connection_group_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if name.is_empty() {
            return;
        }
        let editing = self.editing_connection_group.clone();
        let duplicate = self.config.connection_groups().iter().any(|group| {
            group.eq_ignore_ascii_case(&name)
                && editing
                    .as_ref()
                    .is_none_or(|old| !group.eq_ignore_ascii_case(old))
        });
        if duplicate || editing.as_ref().is_some_and(|old| old == &name) {
            return;
        }

        let previous_config = self.config.cache.clone();
        let changed = if let Some(old_name) = editing.as_deref() {
            self.config.rename_connection_group(old_name, &name)
        } else {
            self.config.add_connection_group(&name)
        };
        if !changed {
            return;
        }
        if let Err(err) = self.config.save() {
            self.config.cache = previous_config;
            tracing::warn!("failed to save connection group: {err:#}");
            self.status = format!("{}: {err:#}", t!("save")).into();
            cx.notify();
            return;
        }

        self.status = if editing.is_some() {
            t!("connection_group_renamed", name = name).into()
        } else {
            t!("connection_group_created", name = name).into()
        };
        self.active_dialog = None;
        self.editing_connection_group = None;
        window.close_dialog(cx);
        cx.notify();
    }

    fn active_sftp_dialog_target(&self) -> Option<(String, crate::sftp::SftpHandle)> {
        let group_id = self.active_group.clone()?;
        let handle = self.sftp_handles.get(&group_id)?.clone();
        Some((group_id, handle))
    }

    pub(crate) fn show_sftp_rename_dialog(
        &mut self,
        remote_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_dialog.is_some() {
            return;
        }
        let Some((group_id, _)) = self.active_sftp_dialog_target() else {
            return;
        };

        self.active_dialog = Some(crate::app::DialogKind::SftpRename);
        self.sftp_rename_state = Some(crate::app::SftpRenameState {
            group_id,
            old_path: remote_path.clone(),
            in_flight: false,
            error: None,
        });
        Self::set_input_value(
            &self.sftp_rename_input,
            crate::sftp::base_name(&remote_path),
            window,
            cx,
        );

        let view = cx.entity();
        let rename_input = self.sftp_rename_input.clone();
        let focus_input = rename_input.clone();
        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("rename").to_string())
                .w(px(440.))
                .close_button(false)
                .keyboard(false)
                .overlay_closable(false)
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.active_dialog == Some(crate::app::DialogKind::SftpRename) {
                                this.active_dialog = None;
                                this.sftp_rename_state = None;
                            }
                            cx.notify();
                        });
                    }
                })
                .content({
                    let view = view.clone();
                    let rename_input = rename_input.clone();
                    move |content, window, cx| {
                        let state = view.read(cx).sftp_rename_state.clone();
                        let new_name = rename_input.read(cx).value().trim().to_string();
                        let name_error = if new_name.is_empty() {
                            Some(t!("name_required").to_string())
                        } else if matches!(new_name.as_str(), "." | "..")
                            || new_name.contains('/')
                            || new_name.contains('\0')
                        {
                            Some(t!("invalid_remote_name").to_string())
                        } else {
                            None
                        };
                        let in_flight = state.as_ref().is_some_and(|state| state.in_flight);
                        let unchanged = state.as_ref().is_some_and(|state| {
                            crate::sftp::base_name(&state.old_path) == new_name
                        });
                        let error = state.and_then(|state| state.error);

                        content.child(
                            v_flex()
                                .gap_3()
                                .child(
                                    v_flex()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(t!("new_name").to_string()),
                                        )
                                        .child(Input::new(&rename_input).w_full().tab_index(0)),
                                )
                                .when_some(name_error.or(error), |this, message| {
                                    this.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().danger)
                                            .child(message),
                                    )
                                })
                                .child(
                                    h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            pointer_button("sftp-rename-cancel")
                                                .label(t!("cancel").to_string())
                                                .disabled(in_flight)
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        if this
                                                            .sftp_rename_state
                                                            .as_ref()
                                                            .is_some_and(|state| state.in_flight)
                                                        {
                                                            return;
                                                        }
                                                        this.active_dialog = None;
                                                        this.sftp_rename_state = None;
                                                        window.close_dialog(cx);
                                                        cx.notify();
                                                    },
                                                )),
                                        )
                                        .child(
                                            pointer_button("sftp-rename-save")
                                                .primary()
                                                .label(t!("rename").to_string())
                                                .loading(in_flight)
                                                .disabled(
                                                    in_flight
                                                        || unchanged
                                                        || new_name.is_empty()
                                                        || new_name == "."
                                                        || new_name == ".."
                                                        || new_name.contains('/')
                                                        || new_name.contains('\0'),
                                                )
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.submit_sftp_rename(window, cx);
                                                    },
                                                )),
                                        ),
                                ),
                        )
                    }
                })
        });
        window.defer(cx, move |window, cx| {
            focus_input.read(cx).focus_handle(cx).focus(window, cx);
        });
    }

    pub(crate) fn submit_sftp_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.sftp_rename_state.clone() else {
            return;
        };
        if state.in_flight {
            return;
        }

        let new_name = self.sftp_rename_input.read(cx).value().trim().to_string();
        if new_name.is_empty()
            || matches!(new_name.as_str(), "." | "..")
            || new_name.contains('/')
            || new_name.contains('\0')
        {
            return;
        }
        let parent = crate::sftp::parent_dir(&state.old_path).unwrap_or_else(|| "/".to_string());
        let new_path = crate::sftp::join_remote(&parent, &new_name);
        if new_path == state.old_path {
            return;
        }
        let Some(handle) = self.sftp_handles.get(&state.group_id).cloned() else {
            if let Some(state) = self.sftp_rename_state.as_mut() {
                state.error = Some(t!("sftp_connection_unavailable").to_string());
            }
            cx.notify();
            return;
        };

        if let Some(state) = self.sftp_rename_state.as_mut() {
            state.in_flight = true;
            state.error = None;
        }
        cx.notify();

        let group_id = state.group_id;
        let old_path = state.old_path;
        let response = handle.rename_path(old_path.clone(), new_path);
        cx.spawn_in(window, async move |this, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err(t!("sftp_connection_unavailable").to_string()));
            match result {
                Ok(()) => {
                    let _ = this.update_in(cx, |this, window, cx| {
                        let is_current = this.active_dialog
                            == Some(crate::app::DialogKind::SftpRename)
                            && this.sftp_rename_state.as_ref().is_some_and(|state| {
                                state.group_id == group_id && state.old_path == old_path
                            });
                        if is_current {
                            this.active_dialog = None;
                            this.sftp_rename_state = None;
                            this.status = t!("rename_success", name = new_name).to_string().into();
                            window.close_dialog(cx);
                            cx.notify();
                        }
                    });
                }
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(state) = this.sftp_rename_state.as_mut().filter(|state| {
                            state.group_id == group_id && state.old_path == old_path
                        }) {
                            state.in_flight = false;
                            state.error = Some(t!("rename_failed", err = error).to_string());
                            cx.notify();
                        }
                    });
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    fn initial_sftp_editor_bounds(viewport: Size<Pixels>) -> Bounds<Pixels> {
        let margin = px(16.);
        let available_width = (viewport.width - margin * 2.).max(px(1.));
        let available_height = (viewport.height - margin * 2.).max(px(1.));
        let width = px(880.).min(available_width);
        let height = px(640.).min(available_height);

        Bounds {
            origin: point(
                ((viewport.width - width) / 2.).max(margin),
                ((viewport.height - height) / 2.).max(margin),
            ),
            size: size(width, height),
        }
    }

    fn clamp_sftp_editor_bounds(
        mut bounds: Bounds<Pixels>,
        viewport: Size<Pixels>,
    ) -> Bounds<Pixels> {
        let margin = px(16.);
        let max_width = (viewport.width - margin * 2.).max(px(1.));
        let max_height = (viewport.height - margin * 2.).max(px(1.));
        let min_width = px(640.).min(max_width);
        let min_height = px(420.).min(max_height);

        bounds.size.width = bounds.size.width.clamp(min_width, max_width);
        bounds.size.height = bounds.size.height.clamp(min_height, max_height);
        let max_x = (viewport.width - margin - bounds.size.width).max(margin);
        let max_y = (viewport.height - margin - bounds.size.height).max(margin);
        bounds.origin.x = bounds.origin.x.clamp(margin, max_x);
        bounds.origin.y = bounds.origin.y.clamp(margin, max_y);
        bounds
    }

    fn start_sftp_editor_move(
        &mut self,
        pointer_origin: Point<Pixels>,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.sftp_editor_state.as_mut() else {
            return;
        };
        state.bounds = Self::clamp_sftp_editor_bounds(state.bounds, viewport);
        state.interaction = Some(crate::app::SftpEditorInteraction::Move {
            pointer_origin,
            initial_bounds: state.bounds,
        });
        cx.notify();
    }

    fn start_sftp_editor_resize(
        &mut self,
        pointer_origin: Point<Pixels>,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.sftp_editor_state.as_mut() else {
            return;
        };
        state.bounds = Self::clamp_sftp_editor_bounds(state.bounds, viewport);
        state.interaction = Some(crate::app::SftpEditorInteraction::Resize {
            pointer_origin,
            initial_bounds: state.bounds,
        });
        cx.notify();
    }

    fn update_sftp_editor_interaction(
        &mut self,
        pointer: Point<Pixels>,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.sftp_editor_state.as_mut() else {
            return;
        };
        let Some(interaction) = state.interaction.clone() else {
            return;
        };

        let mut bounds = match interaction {
            crate::app::SftpEditorInteraction::Move {
                pointer_origin,
                initial_bounds,
            } => Bounds {
                origin: point(
                    initial_bounds.origin.x + pointer.x - pointer_origin.x,
                    initial_bounds.origin.y + pointer.y - pointer_origin.y,
                ),
                size: initial_bounds.size,
            },
            crate::app::SftpEditorInteraction::Resize {
                pointer_origin,
                initial_bounds,
            } => {
                let margin = px(16.);
                let max_width = (viewport.width - margin - initial_bounds.origin.x).max(px(1.));
                let max_height = (viewport.height - margin - initial_bounds.origin.y).max(px(1.));
                let min_width = px(640.).min(max_width);
                let min_height = px(420.).min(max_height);

                Bounds {
                    origin: initial_bounds.origin,
                    size: size(
                        (initial_bounds.size.width + pointer.x - pointer_origin.x)
                            .clamp(min_width, max_width),
                        (initial_bounds.size.height + pointer.y - pointer_origin.y)
                            .clamp(min_height, max_height),
                    ),
                }
            }
        };
        bounds = Self::clamp_sftp_editor_bounds(bounds, viewport);
        if state.bounds != bounds {
            state.bounds = bounds;
            cx.notify();
        }
    }

    fn finish_sftp_editor_interaction(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.sftp_editor_state.as_mut()
            && state.interaction.take().is_some()
        {
            cx.notify();
        }
    }

    pub(crate) fn show_sftp_editor_dialog(
        &mut self,
        remote_path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_dialog.is_some() {
            return;
        }
        let Some((group_id, _)) = self.active_sftp_dialog_target() else {
            return;
        };
        let bounds = Self::initial_sftp_editor_bounds(window.viewport_size());
        let initial_encoding = self
            .active_tab
            .as_ref()
            .and_then(|tab_id| self.tabs.iter().find(|tab| tab.id == *tab_id))
            .map(|tab| tab.text_encoding())
            .unwrap_or(TextEncoding::Utf8);

        self.active_dialog = Some(crate::app::DialogKind::SftpEditor);
        self.sftp_editor_state = Some(crate::app::SftpEditorState {
            group_id,
            remote_path: remote_path.clone(),
            raw_content: Vec::new(),
            original_content: String::new(),
            encoding: initial_encoding,
            has_bom: false,
            decode_had_errors: false,
            loaded: false,
            loading: true,
            saving: false,
            message: None,
            error: None,
            bounds,
            interaction: None,
        });
        self.sftp_editor_input.update(cx, |input, cx| {
            input.set_highlighter(crate::sftp::editor_language(&remote_path), cx);
            input.set_value("", window, cx);
        });

        let view = cx.entity();
        let editor_input = self.sftp_editor_input.clone();
        let title = format!(
            "{} - {}",
            t!("edit_file"),
            crate::sftp::base_name(&remote_path)
        );
        window.open_dialog(cx, move |dialog: Dialog, window, _| {
            let viewport = window.viewport_size();

            dialog
                .w(viewport.width)
                .h(viewport.height)
                .margin_top(px(0.))
                .p_0()
                .bg(gpui::transparent_black())
                .border_0()
                .rounded_none()
                .close_button(false)
                .keyboard(true)
                .overlay_closable(false)
                .on_cancel({
                    let view = view.clone();
                    move |_, _, cx| {
                        !view
                            .read(cx)
                            .sftp_editor_state
                            .as_ref()
                            .is_some_and(|state| state.saving)
                    }
                })
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.active_dialog == Some(crate::app::DialogKind::SftpEditor) {
                                this.active_dialog = None;
                                this.sftp_editor_state = None;
                            }
                            cx.notify();
                        });
                    }
                })
                .content({
                    let view = view.clone();
                    let editor_input = editor_input.clone();
                    let title = title.clone();
                    move |content, window, cx| {
                        let viewport = window.viewport_size();
                        let state = view.read(cx).sftp_editor_state.clone();
                        let current_content = editor_input.read(cx).value().to_string();
                        let (current_bytes, encode_had_errors) = state
                            .as_ref()
                            .map(|state| {
                                let (encoded, had_errors) = state
                                    .encoding
                                    .encode_file(&current_content, state.has_bom);
                                (encoded.len(), had_errors)
                            })
                            .unwrap_or((current_content.len(), false));
                        let loaded = state.as_ref().is_some_and(|state| state.loaded);
                        let loading = state.as_ref().is_some_and(|state| state.loading);
                        let saving = state.as_ref().is_some_and(|state| state.saving);
                        let file_encoding = state
                            .as_ref()
                            .map(|state| state.encoding)
                            .unwrap_or(TextEncoding::Utf8);
                        let decode_had_errors = state
                            .as_ref()
                            .is_some_and(|state| state.decode_had_errors);
                        let dirty = state.as_ref().is_some_and(|state| {
                            state.loaded && state.original_content != current_content
                        });
                        let over_limit = current_bytes > crate::sftp::MAX_INLINE_EDIT_BYTES;
                        let error = state.as_ref().and_then(|state| state.error.clone());
                        let message = state.as_ref().and_then(|state| state.message.clone());
                        let remote_path = state
                            .as_ref()
                            .map(|state| state.remote_path.clone())
                            .unwrap_or_default();

                        let status = if loading {
                            t!("loading_file_content").to_string()
                        } else if saving {
                            t!("saving_file_content").to_string()
                        } else if over_limit {
                            t!("editor_content_too_large", max = "2 MB").to_string()
                        } else if encode_had_errors {
                            t!(
                                "encoding_encode_failed",
                                encoding = file_encoding.label()
                            )
                            .to_string()
                        } else if let Some(error) = error.clone() {
                            error
                        } else if decode_had_errors {
                            t!(
                                "encoding_decode_warning",
                                encoding = file_encoding.label()
                            )
                            .to_string()
                        } else if dirty {
                            t!("unsaved_changes").to_string()
                        } else if let Some(message) = message {
                            message
                        } else if loaded {
                            t!("file_content_loaded").to_string()
                        } else {
                            String::new()
                        };
                        let status_color = if error.is_some() || over_limit || encode_had_errors {
                            cx.theme().danger
                        } else if dirty || decode_had_errors {
                            cx.theme().warning
                        } else if loaded && !loading && !saving {
                            cx.theme().success
                        } else {
                            cx.theme().muted_foreground
                        };
                        let editor_bounds = state
                            .as_ref()
                            .map(|state| {
                                Ashell::clamp_sftp_editor_bounds(state.bounds, viewport)
                            })
                            .unwrap_or_else(|| Ashell::initial_sftp_editor_bounds(viewport));
                        let moving = state.as_ref().is_some_and(|state| {
                            matches!(
                                state.interaction.as_ref(),
                                Some(crate::app::SftpEditorInteraction::Move { .. })
                            )
                        });
                        let resizing = state.as_ref().is_some_and(|state| {
                            matches!(
                                state.interaction.as_ref(),
                                Some(crate::app::SftpEditorInteraction::Resize { .. })
                            )
                        });

                        content.p_0().child(
                            div()
                                .id("sftp-editor-stage")
                                .size_full()
                                .relative()
                                .when(moving, |this| this.cursor_grabbing())
                                .when(resizing, |this| this.cursor_nwse_resize())
                                .on_mouse_up(
                                    MouseButton::Left,
                                    window.listener_for(&view, |this, _, _, cx| {
                                        this.finish_sftp_editor_interaction(cx);
                                    }),
                                )
                                .on_mouse_up_out(
                                    MouseButton::Left,
                                    window.listener_for(&view, |this, _, _, cx| {
                                        this.finish_sftp_editor_interaction(cx);
                                    }),
                                )
                                .child(
                                    v_flex()
                                        .absolute()
                                        .left(editor_bounds.origin.x)
                                        .top(editor_bounds.origin.y)
                                        .w(editor_bounds.size.width)
                                        .h(editor_bounds.size.height)
                                        .min_h(px(0.))
                                        .overflow_hidden()
                                        .occlude()
                                        .bg(cx.theme().background)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .rounded(cx.theme().radius_lg)
                                        .shadow_xl()
                                        .on_any_mouse_down(|_, _, cx| {
                                            cx.stop_propagation();
                                        })
                                        .child(
                                            h_flex()
                                                .id("sftp-editor-title-bar")
                                                .flex_none()
                                                .h(px(40.))
                                                .px_3()
                                                .items_center()
                                                .gap_2()
                                                .border_b_1()
                                                .border_color(cx.theme().border)
                                                .bg(cx.theme().muted.opacity(0.8))
                                                .cursor_grab()
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    window.listener_for(
                                                        &view,
                                                        |this,
                                                         event: &MouseDownEvent,
                                                         window,
                                                         cx| {
                                                            this.start_sftp_editor_move(
                                                                event.position,
                                                                window.viewport_size(),
                                                                cx,
                                                            );
                                                            window.prevent_default();
                                                            cx.stop_propagation();
                                                        },
                                                    ),
                                                )
                                                .on_drag(
                                                    SftpEditorDrag::Move,
                                                    |drag, _, _, cx| {
                                                        cx.stop_propagation();
                                                        cx.new(|_| drag.clone())
                                                    },
                                                )
                                                .on_drag_move(window.listener_for(
                                                    &view,
                                                    |this,
                                                     event: &DragMoveEvent<SftpEditorDrag>,
                                                     window,
                                                     cx| {
                                                        if matches!(
                                                            event.drag(cx),
                                                            SftpEditorDrag::Move
                                                        ) {
                                                            this.update_sftp_editor_interaction(
                                                                event.event.position,
                                                                window.viewport_size(),
                                                                cx,
                                                            );
                                                        }
                                                    },
                                                ))
                                                .on_mouse_up(
                                                    MouseButton::Left,
                                                    window.listener_for(&view, |this, _, _, cx| {
                                                        this.finish_sftp_editor_interaction(cx);
                                                    }),
                                                )
                                                .on_mouse_up_out(
                                                    MouseButton::Left,
                                                    window.listener_for(&view, |this, _, _, cx| {
                                                        this.finish_sftp_editor_interaction(cx);
                                                    }),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w(px(0.))
                                                        .truncate()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(title.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            |_, window, cx| {
                                                                window.prevent_default();
                                                                cx.stop_propagation();
                                                            },
                                                        )
                                                        .child(
                                                            pointer_button("sftp-editor-close")
                                                                .small()
                                                                .ghost()
                                                                .icon(IconName::Close)
                                                                .tooltip(t!("cancel").to_string())
                                                                .disabled(saving)
                                                                .on_click(window.listener_for(
                                                                    &view,
                                                                    |this, _, window, cx| {
                                                                        if this
                                                                            .sftp_editor_state
                                                                            .as_ref()
                                                                            .is_some_and(|state| {
                                                                                state.saving
                                                                            })
                                                                        {
                                                                            return;
                                                                        }
                                                                        this.active_dialog = None;
                                                                        this.sftp_editor_state =
                                                                            None;
                                                                        window.close_dialog(cx);
                                                                        cx.notify();
                                                                    },
                                                                )),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_h(px(0.))
                                                .gap_2()
                                                .p_3()
                                                .child(
                                                    h_flex()
                                                        .w_full()
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .id("sftp-editor-remote-path")
                                                                .flex_1()
                                                                .min_w(px(0.))
                                                                .truncate()
                                                                .text_sm()
                                                                .text_color(
                                                                    cx.theme().muted_foreground,
                                                                )
                                                                .tooltip({
                                                                    let remote_path =
                                                                        remote_path.clone();
                                                                    move |window, cx| {
                                                                        gpui_component::tooltip::Tooltip::new(
                                                                            remote_path.clone(),
                                                                        )
                                                                        .build(window, cx)
                                                                    }
                                                                })
                                                                .child(remote_path),
                                                        )
                                                        .child(
                                                            pointer_button(
                                                                "sftp-editor-encoding",
                                                            )
                                                            .ghost()

                                                            .icon(IconName::Globe)
                                                            .label(file_encoding.label())
                                                            .tooltip(
                                                                t!("file_encoding").to_string(),
                                                            )
                                                            .disabled(loading || saving)
                                                            .dropdown_menu_with_anchor(
                                                                Anchor::BottomRight,
                                                                {
                                                                    let view = view.clone();
                                                                    move |menu, window, _| {
                                                                        FILE_ENCODINGS
                                                                            .iter()
                                                                            .copied()
                                                                            .fold(
                                                                                menu.min_w(0.),
                                                                                |menu, candidate| {
                                                                                    menu.item(
                                                                                        PopupMenuItem::new(
                                                                                            candidate.label(),
                                                                                        )
                                                                                        .checked(
                                                                                            candidate
                                                                                                == file_encoding,
                                                                                        )
                                                                                        .on_click(
                                                                                            window.listener_for(
                                                                                                &view,
                                                                                                move |this,
                                                                                                      _,
                                                                                                      window,
                                                                                                      cx| {
                                                                                                    this.set_sftp_editor_encoding(
                                                                                                        candidate,
                                                                                                        window,
                                                                                                        cx,
                                                                                                    );
                                                                                                },
                                                                                            ),
                                                                                        ),
                                                                                    )
                                                                                },
                                                                            )
                                                                    }
                                                                },
                                                            ),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_h(px(0.))
                                                        .when(
                                                            loading
                                                                || (!loaded && error.is_some()),
                                                            |this| {
                                                                this.flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                            },
                                                        )
                                                        .child(if loaded {
                                                            Input::new(&editor_input)
                                                                .size_full()
                                                                .tab_index(0)
                                                                .into_any_element()
                                                        } else {
                                                            v_flex()
                                                                .items_center()
                                                                .gap_3()
                                                                .child(
                                                                    div()
                                                                        .text_sm()
                                                                        .text_color(status_color)
                                                                        .child(status.clone()),
                                                                )
                                                                .when(!loading, |this| {
                                                                    this.child(
                                                                        pointer_button(
                                                                            "sftp-editor-retry",
                                                                        )
                                                                        .icon(IconName::Redo)
                                                                        .label(
                                                                            t!("retry")
                                                                                .to_string(),
                                                                        )
                                                                        .on_click(
                                                                            window.listener_for(
                                                                                &view,
                                                                                |this,
                                                                                 _,
                                                                                 window,
                                                                                 cx| {
                                                                                    this.load_sftp_editor_content(window, cx);
                                                                                },
                                                                            ),
                                                                        ),
                                                                    )
                                                                })
                                                                .into_any_element()
                                                        }),
                                                )
                                                .child(
                                                    h_flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w(px(0.))
                                                                .truncate()
                                                                .text_sm()
                                                                .text_color(status_color)
                                                                .child(status),
                                                        )
                                                        .when(loaded, |this| {
                                                            this.child(
                                                                div()
                                                                    .text_sm()
                                                                    .text_color(
                                                                        cx.theme()
                                                                            .muted_foreground,
                                                                    )
                                                                    .child(
                                                                        crate::system::format_bytes(
                                                                            current_bytes as u64,
                                                                        ),
                                                                    ),
                                                            )
                                                        })
                                                        .child(
                                                            pointer_button("sftp-editor-cancel")
                                                                .label(t!("cancel").to_string())
                                                                .disabled(saving)
                                                                .on_click(window.listener_for(
                                                                    &view,
                                                                    |this, _, window, cx| {
                                                                        if this
                                                                            .sftp_editor_state
                                                                            .as_ref()
                                                                            .is_some_and(|state| {
                                                                                state.saving
                                                                            })
                                                                        {
                                                                            return;
                                                                        }
                                                                        this.active_dialog = None;
                                                                        this.sftp_editor_state =
                                                                            None;
                                                                        window.close_dialog(cx);
                                                                        cx.notify();
                                                                    },
                                                                )),
                                                        )
                                                        .child(
                                                            pointer_button("sftp-editor-save")
                                                                .primary()
                                                                .icon(IconName::Check)
                                                                .label(t!("save").to_string())
                                                                .loading(saving)
                                                                .disabled(
                                                                    !loaded
                                                                        || !dirty
                                                                        || saving
                                                                        || over_limit
                                                                        || decode_had_errors
                                                                        || encode_had_errors,
                                                                )
                                                                .on_click(window.listener_for(
                                                                    &view,
                                                                    |this, _, window, cx| {
                                                                        this.save_sftp_editor_content(
                                                                            window, cx,
                                                                        );
                                                                    },
                                                                )),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("sftp-editor-resize-handle")
                                                .absolute()
                                                .right_0()
                                                .bottom_0()
                                                .size(px(18.))
                                                .cursor_nwse_resize()
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    window.listener_for(
                                                        &view,
                                                        |this,
                                                         event: &MouseDownEvent,
                                                         window,
                                                         cx| {
                                                            this.start_sftp_editor_resize(
                                                                event.position,
                                                                window.viewport_size(),
                                                                cx,
                                                            );
                                                            window.prevent_default();
                                                            cx.stop_propagation();
                                                        },
                                                    ),
                                                )
                                                .on_drag(
                                                    SftpEditorDrag::Resize,
                                                    |drag, _, _, cx| {
                                                        cx.stop_propagation();
                                                        cx.new(|_| drag.clone())
                                                    },
                                                )
                                                .on_drag_move(window.listener_for(
                                                    &view,
                                                    |this,
                                                     event: &DragMoveEvent<SftpEditorDrag>,
                                                     window,
                                                     cx| {
                                                        if matches!(
                                                            event.drag(cx),
                                                            SftpEditorDrag::Resize
                                                        ) {
                                                            this.update_sftp_editor_interaction(
                                                                event.event.position,
                                                                window.viewport_size(),
                                                                cx,
                                                            );
                                                        }
                                                    },
                                                ))
                                                .on_mouse_up(
                                                    MouseButton::Left,
                                                    window.listener_for(&view, |this, _, _, cx| {
                                                        this.finish_sftp_editor_interaction(cx);
                                                    }),
                                                )
                                                .on_mouse_up_out(
                                                    MouseButton::Left,
                                                    window.listener_for(&view, |this, _, _, cx| {
                                                        this.finish_sftp_editor_interaction(cx);
                                                    }),
                                                )
                                                .child(
                                                    Icon::new(IconName::ResizeCorner)
                                                        .size_3()
                                                        .absolute()
                                                        .right(px(2.))
                                                        .bottom(px(2.))
                                                        .text_color(
                                                            cx.theme()
                                                                .muted_foreground
                                                                .opacity(0.6),
                                                        ),
                                                ),
                                        ),
                                ),
                        )
                    }
                })
        });
        self.load_sftp_editor_content(window, cx);
    }

    pub(crate) fn set_sftp_editor_encoding(
        &mut self,
        encoding: TextEncoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.sftp_editor_state.clone() else {
            return;
        };
        if state.encoding == encoding {
            return;
        }

        let current_content = self.sftp_editor_input.read(cx).value().to_string();
        let dirty = state.loaded && current_content != state.original_content;
        if !state.loaded || dirty {
            if let Some(editor_state) = self.sftp_editor_state.as_mut() {
                editor_state.encoding = encoding;
                editor_state.has_bom = encoding.default_bom();
                if !state.loaded {
                    editor_state.decode_had_errors = false;
                }
                editor_state.error = None;
                editor_state.message =
                    Some(t!("file_encoding_changed", encoding = encoding.label()).to_string());
            }
            cx.notify();
            return;
        }

        let (content, had_errors, has_bom) = encoding.decode_file(&state.raw_content);
        self.sftp_editor_input.update(cx, |input, cx| {
            input.set_value(content.clone(), window, cx);
        });
        if let Some(editor_state) = self.sftp_editor_state.as_mut() {
            editor_state.encoding = encoding;
            editor_state.original_content = content;
            editor_state.has_bom = has_bom;
            editor_state.decode_had_errors = had_errors;
            editor_state.message = None;
            editor_state.error = None;
        }
        cx.notify();
    }

    pub(crate) fn load_sftp_editor_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.sftp_editor_state.clone() else {
            return;
        };
        let Some(handle) = self.sftp_handles.get(&state.group_id).cloned() else {
            if let Some(state) = self.sftp_editor_state.as_mut() {
                state.loading = false;
                state.error = Some(t!("sftp_connection_unavailable").to_string());
            }
            cx.notify();
            return;
        };

        if let Some(state) = self.sftp_editor_state.as_mut() {
            state.loaded = false;
            state.loading = true;
            state.saving = false;
            state.message = None;
            state.error = None;
        }
        cx.notify();

        let group_id = state.group_id;
        let remote_path = state.remote_path;
        let response = handle.read_text_file(remote_path.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err(t!("sftp_connection_unavailable").to_string()));
            let _ = gpui::AsyncWindowContext::update(cx, |window, cx| {
                let _ = this.update(cx, |this, cx| {
                    let is_current = this.active_dialog == Some(crate::app::DialogKind::SftpEditor)
                        && this.sftp_editor_state.as_ref().is_some_and(|state| {
                            state.group_id == group_id && state.remote_path == remote_path
                        });
                    if !is_current {
                        return;
                    }
                    match result {
                        Ok(raw_content) => {
                            let preferred_encoding = this
                                .sftp_editor_state
                                .as_ref()
                                .map(|state| state.encoding)
                                .unwrap_or(TextEncoding::Utf8);
                            let encoding = TextEncoding::detect_bom(&raw_content)
                                .or_else(|| {
                                    std::str::from_utf8(&raw_content)
                                        .is_ok()
                                        .then_some(TextEncoding::Utf8)
                                })
                                .unwrap_or(preferred_encoding);
                            let (content, had_errors, has_bom) = encoding.decode_file(&raw_content);
                            this.sftp_editor_input.update(cx, |input, cx| {
                                input.set_value(content.clone(), window, cx);
                                input.focus_handle(cx).focus(window, cx);
                            });
                            if let Some(state) = this.sftp_editor_state.as_mut() {
                                state.raw_content = raw_content;
                                state.original_content = content;
                                state.encoding = encoding;
                                state.has_bom = has_bom;
                                state.decode_had_errors = had_errors;
                                state.loaded = true;
                                state.loading = false;
                                state.message = None;
                                state.error = None;
                            }
                        }
                        Err(error) => {
                            if let Some(state) = this.sftp_editor_state.as_mut() {
                                state.loaded = false;
                                state.loading = false;
                                state.error =
                                    Some(t!("open_remote_file_failed", err = error).to_string());
                            }
                        }
                    }
                    cx.notify();
                });
            });
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn save_sftp_editor_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.sftp_editor_state.clone() else {
            return;
        };
        if !state.loaded || state.loading || state.saving {
            return;
        }
        let content = self.sftp_editor_input.read(cx).value().to_string();
        if content == state.original_content {
            return;
        }
        if state.decode_had_errors {
            if let Some(state) = self.sftp_editor_state.as_mut() {
                state.error = Some(
                    t!("encoding_decode_warning", encoding = state.encoding.label()).to_string(),
                );
            }
            cx.notify();
            return;
        }
        let (encoded_content, encode_had_errors) =
            state.encoding.encode_file(&content, state.has_bom);
        if encode_had_errors {
            if let Some(state) = self.sftp_editor_state.as_mut() {
                state.error = Some(
                    t!("encoding_encode_failed", encoding = state.encoding.label()).to_string(),
                );
            }
            cx.notify();
            return;
        }
        if encoded_content.len() > crate::sftp::MAX_INLINE_EDIT_BYTES {
            if let Some(state) = self.sftp_editor_state.as_mut() {
                state.error = Some(t!("editor_content_too_large", max = "2 MB").to_string());
            }
            cx.notify();
            return;
        }
        let Some(handle) = self.sftp_handles.get(&state.group_id).cloned() else {
            if let Some(state) = self.sftp_editor_state.as_mut() {
                state.error = Some(t!("sftp_connection_unavailable").to_string());
            }
            cx.notify();
            return;
        };

        if let Some(state) = self.sftp_editor_state.as_mut() {
            state.saving = true;
            state.message = None;
            state.error = None;
        }
        cx.notify();

        let group_id = state.group_id;
        let remote_path = state.remote_path;
        let saved_bytes = encoded_content.clone();
        let response = handle.write_text_file(remote_path.clone(), encoded_content);
        cx.spawn_in(window, async move |this, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err(t!("sftp_connection_unavailable").to_string()));
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this
                    .sftp_editor_state
                    .as_mut()
                    .filter(|state| state.group_id == group_id && state.remote_path == remote_path)
                else {
                    return;
                };
                state.saving = false;
                match result {
                    Ok(()) => {
                        state.raw_content = saved_bytes;
                        state.original_content = content;
                        state.decode_had_errors = false;
                        state.message = Some(t!("remote_file_saved").to_string());
                        state.error = None;
                    }
                    Err(error) => {
                        state.message = None;
                        state.error = Some(t!("save_remote_file_failed", err = error).to_string());
                    }
                }
                cx.notify();
            });
            Ok::<(), anyhow::Error>(())
        })
        .detach();
    }

    pub(crate) fn show_remote_processes_dialog(
        &mut self,
        view_mode: crate::app::ServerMonitorView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_dialog.is_some()
            || !self.system_tab_id.as_ref().is_some_and(|tab_id| {
                self.tabs.iter().any(|tab| {
                    tab.id == *tab_id && tab.kind == crate::terminal::TabKind::Ssh && tab.connected
                })
            })
        {
            return;
        }

        self.active_dialog = Some(crate::app::DialogKind::Processes);
        self.server_monitor_view = view_mode;
        self.remote_processes.clear();
        self.remote_process_status = None;
        self.expanded_process_pid = None;
        Self::set_input_value(&self.remote_process_filter_input, "", window, cx);
        self.sort_remote_processes();
        self.request_active_process_snapshot();

        let view = cx.entity();
        let title = t!("system_processes").to_string();

        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(title.clone())
                .w(px(720.))
                .h(px(560.))
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.active_dialog == Some(crate::app::DialogKind::Processes) {
                                this.active_dialog = None;
                            }
                            cx.notify();
                        });
                    }
                })
                .content({
                    let view = view.clone();
                    move |content, _window, cx| {
                        content.child(view.update(cx, |this, cx| {
                            this.render_remote_process_list(view_mode, cx)
                        }))
                    }
                })
        });
    }

    pub(crate) fn show_remote_ports_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_dialog.is_some()
            || !self.system_tab_id.as_ref().is_some_and(|tab_id| {
                self.tabs.iter().any(|tab| {
                    tab.id == *tab_id && tab.kind == crate::terminal::TabKind::Ssh && tab.connected
                })
            })
        {
            return;
        }

        self.active_dialog = Some(crate::app::DialogKind::Ports);
        self.remote_ports.clear();
        self.remote_ports_status = None;
        Self::set_input_value(&self.remote_port_filter_input, "", window, cx);
        self.request_active_port_snapshot();

        let view = cx.entity();
        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("network_ports").to_string())
                .w(px(760.))
                .h(px(560.))
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.active_dialog == Some(crate::app::DialogKind::Ports) {
                                this.active_dialog = None;
                            }
                            cx.notify();
                        });
                    }
                })
                .content({
                    let view = view.clone();
                    move |content, _window, cx| {
                        content.child(view.update(cx, |this, cx| this.render_remote_port_list(cx)))
                    }
                })
        });
    }

    pub(crate) fn show_about_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_dialog.is_some() {
            return;
        }

        self.active_dialog = Some(crate::app::DialogKind::About);
        let view = cx.entity();
        let version = env!("CARGO_PKG_VERSION");

        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("menu_about_ashell").to_string())
                .w(px(440.))
                .keyboard(false)
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if this.active_dialog == Some(crate::app::DialogKind::About) {
                                this.active_dialog = None;
                            }
                            cx.notify();
                        });
                    }
                })
                .content(move |content, _window, cx| {
                    content.child(
                        v_flex()
                            .w_full()
                            .items_center()
                            .gap_3()
                            .py_4()
                            .child(
                                div()
                                    .text_size(ui_rems(1.5))
                                    .font_weight(FontWeight::BOLD)
                                    .child("Ashell"),
                            )
                            .child(div().text_size(ui_rems(0.9)).child(format!(
                                "{} {}",
                                t!("version"),
                                version
                            )))
                            .child(
                                div()
                                    .text_size(ui_rems(0.9))
                                    .text_color(cx.theme().muted_foreground)
                                    .text_center()
                                    .child(t!("about_description")),
                            )
                            .child(
                                div()
                                    .text_size(ui_rems(0.9))
                                    .text_color(cx.theme().muted_foreground)
                                    .text_center()
                                    .child(t!("about_feedback_hint")),
                            )
                            .child(
                                pointer_button("about-project-link")
                                    .label("https://github.com/rust-kotlin/ashell")
                                    .ghost()
                                    .on_click(|_, _, _| {
                                        if let Err(error) =
                                            open::that("https://github.com/rust-kotlin/ashell")
                                        {
                                            tracing::warn!(
                                                "failed to open Ashell project website: {error}"
                                            );
                                        }
                                    }),
                            ),
                    )
                })
                .footer({
                    let view = view.clone();
                    h_flex().w_full().justify_end().child(
                        pointer_button("about-close")
                            .primary()
                            .label(t!("close").to_string())
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    if this.active_dialog == Some(crate::app::DialogKind::About) {
                                        this.active_dialog = None;
                                    }
                                    cx.notify();
                                });
                                window.close_dialog(cx);
                            }),
                    )
                })
        });
    }

    pub(crate) fn show_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_dialog.is_some() {
            return;
        }
        self.active_dialog = Some(crate::app::DialogKind::Settings);

        let view = cx.entity();

        // Suspend workspace commands that could interfere with recording. Quit stays active.
        crate::app::keybinding_recorder::unbind_all_workspace_keys(cx, &self.config);
        self.keybinds_suspended = true;

        window.open_dialog(cx, move |dialog: Dialog, _window, _| {
            dialog
                .title(t!("settings").to_string())
                .w(px(840.))
                .h(px(560.))
                .on_close({
                    let view = view.clone();
                    move |_, _window, cx| {
                        // Re-register all workspace keys when closing settings
                        view.update(cx, |this, cx| {
                            this.active_dialog = None;
                            this.keybinds_suspended = false;
                            this.recording_action = None;
                            this.keybind_error = None;
                            crate::app::keybinding_recorder::bind_workspace_keys_from_config(
                                cx,
                                &this.config,
                            );
                            crate::app::system_menu::set_app_menus(cx);
                            cx.notify();
                        });
                    }
                })
                .content({
                    let view = view.clone();
                    move |content, _window, cx| {
                        use gpui_component::setting::{Settings, SettingPage, SettingGroup, SettingItem, SettingField};
                        use gpui::IntoElement;
                        let version = env!("CARGO_PKG_VERSION");
                        let view_clone_for_general = view.clone();
                        let sync_endpoint_input = view.read(cx).sync_endpoint_input.clone();
                        let sync_username_input = view.read(cx).sync_username_input.clone();
                        let sync_webdav_password_input = view.read(cx).sync_webdav_password_input.clone();
                        let sync_s3_endpoint_input = view.read(cx).sync_s3_endpoint_input.clone();
                        let sync_s3_region_input = view.read(cx).sync_s3_region_input.clone();
                        let sync_s3_bucket_input = view.read(cx).sync_s3_bucket_input.clone();
                        let sync_s3_object_key_input = view.read(cx).sync_s3_object_key_input.clone();
                        let sync_s3_access_key_input = view.read(cx).sync_s3_access_key_input.clone();
                        let sync_s3_secret_key_input = view.read(cx).sync_s3_secret_key_input.clone();
                        let sync_s3_session_token_input = view.read(cx).sync_s3_session_token_input.clone();
                        let sync_encryption_password_input = view.read(cx).sync_encryption_password_input.clone();

                        let focus_handle = view.read(cx).focus_handle.clone();

                        content.child(
                            div()
                                .flex()
                                .flex_col()
                                .size_full()
                                .track_focus(&focus_handle)
                                .on_key_down({
                                    let view = view.clone();
                                    move |ev: &gpui::KeyDownEvent, window, cx| {
                                        view.update(cx, |this, cx| {
                                            let Some(action) = this.recording_action.clone() else {
                                                return;
                                            };

                                            window.prevent_default();
                                            cx.stop_propagation();

                                            if ev.keystroke.key == "escape" {
                                                this.recording_action = None;
                                                crate::app::keybinding_recorder::restore_quit_keybinding(cx, &this.config);
                                                cx.notify();
                                                return;
                                            }

                                            if ev.keystroke.key == "backspace"
                                                && !ev.keystroke.modifiers.control
                                                && !ev.keystroke.modifiers.alt
                                                && !ev.keystroke.modifiers.shift
                                                && !ev.keystroke.modifiers.platform
                                                && !ev.keystroke.modifiers.function
                                            {
                                                this.recording_action = None;
                                                this.keybind_error = None;
                                                this.config.set_key_binding(&action, "none");
                                                crate::app::keybinding_recorder::restore_quit_keybinding(cx, &this.config);
                                                this.save_preferences_background();
                                                cx.notify();
                                                return;
                                            }

                                            let Some(new_key) = crate::app::keybinding_recorder::normalize_recorded_keystroke(ev) else {
                                                return;
                                            };

                                            // Check for conflicts with other actions
                                            if let Some((_conflict_id, conflict_label)) =
                                                crate::app::keybinding_recorder::find_conflict(
                                                    &this.config,
                                                    &action,
                                                    &new_key,
                                                )
                                            {
                                                let formatted = crate::app::keybinding_recorder::format_keystroke(&new_key);
                                                this.recording_action = None;
                                                this.keybind_error = Some((
                                                    action.clone(),
                                                    t!("keybind_conflict", key = formatted, action = conflict_label).to_string(),
                                                ));
                                                crate::app::keybinding_recorder::restore_quit_keybinding(cx, &this.config);
                                                cx.notify();
                                                return;
                                            }

                                            this.recording_action = None;
                                            this.keybind_error = None;
                                            this.config.set_key_binding(&action, &new_key);
                                            crate::app::keybinding_recorder::restore_quit_keybinding(cx, &this.config);
                                            this.save_preferences_background();
                                            cx.notify();
                                        });
                                    }
                                })
                                .on_mouse_down_out({
                                    let view = view.clone();
                                    move |_, _window, cx| {
                                        view.update(cx, |this, cx| {
                                            if this.recording_action.is_some() {
                                                this.recording_action = None;
                                                crate::app::keybinding_recorder::restore_quit_keybinding(cx, &this.config);
                                                cx.notify();
                                            }
                                        });
                                    }
                                })
                                .child(
                                    Settings::new("settings")
                                        .sidebar_width(px(180.))
                                        .sidebar_style(div().bg(cx.theme().background).style())
                                .page(
                                    SettingPage::new(t!("settings_general").to_string())
                                        .icon(IconName::Settings)
                                        .default_open(true)
                                        .group(
                                            SettingGroup::new()
                                                .title(t!("settings_group_appearance").to_string())
                                                .item(
                                                    SettingItem::new(
                                                        t!("theme_mode").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                let (follow_system, is_dark_mode) = {
                                                                    let state = view.read(cx);
                                                                    (state.follow_system_theme, state.theme_mode.is_dark())
                                                                };
                                                                pointer_button("theme-mode-dropdown")

                                                                    .icon(if follow_system { IconName::Sun } else if is_dark_mode { IconName::Moon } else { IconName::Sun })
                                                                    .label(if follow_system { t!("follow_system").to_string() } else if is_dark_mode { t!("use_dark_mode").to_string() } else { t!("use_light_mode").to_string() })
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let (follow_system, is_dark_mode) = {
                                                                                let state = view.read(cx);
                                                                                (state.follow_system_theme, state.theme_mode.is_dark())
                                                                            };
                                                                            menu = menu.min_w(0.)
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("follow_system").to_string())
                                                                                        .checked(follow_system)
                                                                                        .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                                            this.set_follow_system_theme(true, window, cx)
                                                                                        }))
                                                                                )
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("use_light_mode").to_string())
                                                                                        .checked(!follow_system && !is_dark_mode)
                                                                                        .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                                            this.switch_theme_mode(crate::app::ThemeMode::Light, window, cx)
                                                                                        }))
                                                                                )
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("use_dark_mode").to_string())
                                                                                        .checked(!follow_system && is_dark_mode)
                                                                                        .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                                            this.switch_theme_mode(crate::app::ThemeMode::Dark, window, cx)
                                                                                        }))
                                                                                );
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("light_theme").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                let current_theme = view.read(cx).light_theme_name.to_string();
                                                                pointer_button("light-theme-dropdown")

                                                                    .icon(IconName::Sun)
                                                                    .label(current_theme.clone())
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current_theme = view.read(cx).light_theme_name.to_string();
                                                                            let themes = gpui_component::ThemeRegistry::global(cx).sorted_themes();
                                                                            let light_themes: Vec<_> = themes.into_iter().filter(|t| !t.mode.is_dark()).map(|t| t.name.clone()).collect();
                                                                            menu = menu.min_w(0.).max_h(px(320.)).scrollable(true);
                                                                            for theme_name in light_themes {
                                                                                let checked = theme_name == current_theme;
                                                                                menu = menu.item(
                                                                                    PopupMenuItem::new(theme_name.clone())
                                                                                        .checked(checked)
                                                                                        .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                                                                            this.apply_theme(theme_name.clone(), window, cx)
                                                                                        }))
                                                                                );
                                                                            }
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("dark_theme").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                let current_theme = view.read(cx).dark_theme_name.to_string();
                                                                pointer_button("dark-theme-dropdown")

                                                                    .icon(IconName::Moon)
                                                                    .label(current_theme.clone())
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current_theme = view.read(cx).dark_theme_name.to_string();
                                                                            let themes = gpui_component::ThemeRegistry::global(cx).sorted_themes();
                                                                            let dark_themes: Vec<_> = themes.into_iter().filter(|t| t.mode.is_dark()).map(|t| t.name.clone()).collect();
                                                                            menu = menu.min_w(0.).max_h(px(320.)).scrollable(true);
                                                                            for theme_name in dark_themes {
                                                                                let checked = theme_name == current_theme;
                                                                                menu = menu.item(
                                                                                    PopupMenuItem::new(theme_name.clone())
                                                                                        .checked(checked)
                                                                                        .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                                                                            this.apply_theme(theme_name.clone(), window, cx)
                                                                                        }))
                                                                                );
                                                                            }
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        format!("{}{}", t!("title_bar_style"), t!("restart_hint")),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                let current_style = view.read(cx).config.title_bar_style();
                                                                pointer_button("title-bar-style-dropdown")

                                                                    .label(match current_style {
                                                                        crate::session::config::TitleBarStyle::Native => t!("title_bar_native").to_string(),
                                                                        crate::session::config::TitleBarStyle::Integrated => t!("title_bar_integrated").to_string(),
                                                                    })
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current_style = view.read(cx).config.title_bar_style();
                                                                            menu = menu.min_w(0.)
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("title_bar_native").to_string())
                                                                                        .checked(current_style == crate::session::config::TitleBarStyle::Native)
                                                                                        .on_click(window.listener_for(&view, |this, _, _, cx| {
                                                                                            this.config.set_title_bar_style(crate::session::config::TitleBarStyle::Native);
                                                                                            this.save_preferences_background();
                                                                                            cx.notify();
                                                                                        }))
                                                                                )
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("title_bar_integrated").to_string())
                                                                                        .checked(current_style == crate::session::config::TitleBarStyle::Integrated)
                                                                                        .on_click(window.listener_for(&view, |this, _, _, cx| {
                                                                                            this.config.set_title_bar_style(crate::session::config::TitleBarStyle::Integrated);
                                                                                            this.save_preferences_background();
                                                                                            cx.notify();
                                                                                        }))
                                                                                );
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                        )
                                        .group(
                                            SettingGroup::new()
                                                .title(t!("settings_group_font").to_string())
                                                .item(
                                                    SettingItem::new(
                                                        t!("ui_font_size").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, window, cx| {
                                                                h_flex()
                                                                    .items_center()
                                                                    .gap_3()
                                                                    .child(pointer_button("ui-font-size-down").label("-").on_click(window.listener_for(&view, |this, _, _, cx| this.change_ui_font_size(-1.0, cx))))
                                                                    .child(div().min_w(px(64.)).text_center().child(format!("{:.0}px", view.read(cx).ui_font_size)))
                                                                    .child(pointer_button("ui-font-size-up").label("+").on_click(window.listener_for(&view, |this, _, _, cx| this.change_ui_font_size(1.0, cx))))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("terminal_font_size").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, window, cx| {
                                                                h_flex()
                                                                    .items_center()
                                                                    .gap_3()
                                                                    .child(pointer_button("terminal-font-size-down").label("-").on_click(window.listener_for(&view, |this, _, _, cx| this.change_terminal_font_size(-1.0, cx))))
                                                                    .child(div().min_w(px(64.)).text_center().child(format!("{:.0}px", view.read(cx).terminal_font_size)))
                                                                    .child(pointer_button("terminal-font-size-up").label("+").on_click(window.listener_for(&view, |this, _, _, cx| this.change_terminal_font_size(1.0, cx))))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("ui_font_family").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                pointer_button("ui-font-dropdown")

                                                                    .icon(IconName::ChevronsUpDown)
                                                                    .label({
                                                                        let current = view.read(cx).ui_font_family.to_string();
                                                                        let names = cx.text_system().all_font_names();
                                                                        let using_system_maple = crate::app::theme::USING_SYSTEM_MAPLE.load(std::sync::atomic::Ordering::Relaxed);
                                                                        if current == *".SystemUIFont" || current.is_empty() || !names.contains(&current) {
                                                                            t!("system_default").to_string()
                                                                        } else if !using_system_maple && current == "Maple Mono NF CN" {
                                                                            format!("Maple Mono NF CN ({})", t!("software_builtin"))
                                                                        } else {
                                                                            current
                                                                        }
                                                                    })
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current = view.read(cx).ui_font_family.to_string();
                                                                            let mut names = cx.text_system().all_font_names();
                                                                            menu = menu.min_w(0.).max_h(px(320.)).scrollable(true);
                                                                            menu = menu.item(
                                                                                PopupMenuItem::new(t!("system_default").to_string())
                                                                                    .checked(current == *".SystemUIFont" || current.is_empty())
                                                                                    .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                                                                        this.change_ui_font_family(".SystemUIFont", window, cx);
                                                                                    }))
                                                                            );
                                                                            let maple_font = "Maple Mono NF CN".to_string();
                                                                            let using_system_maple = crate::app::theme::USING_SYSTEM_MAPLE.load(std::sync::atomic::Ordering::Relaxed);
                                                                            if !using_system_maple && names.contains(&maple_font) {
                                                                                names.retain(|n| n != &maple_font);
                                                                                menu = menu.item(
                                                                                    PopupMenuItem::new(format!("{} ({})", maple_font, t!("software_builtin")))
                                                                                        .checked(current == maple_font)
                                                                                        .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                                                                            this.change_ui_font_family("Maple Mono NF CN", window, cx);
                                                                                        }))
                                                                                ).separator();
                                                                            }
                                                                            for name in names {
                                                                                let checked = name == current;
                                                                                menu = menu.item(
                                                                                    PopupMenuItem::new(name.clone())
                                                                                        .checked(checked)
                                                                                        .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                                                                            this.change_ui_font_family(&name, window, cx);
                                                                                        }))
                                                                                );
                                                                            }
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("terminal_font_family").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                pointer_button("terminal-font-dropdown")

                                                                    .icon(IconName::ChevronsUpDown)
                                                                    .label({
                                                                        let current = view.read(cx).terminal_font_family.to_string();
                                                                        let using_system_maple = crate::app::theme::USING_SYSTEM_MAPLE.load(std::sync::atomic::Ordering::Relaxed);
                                                                        if !using_system_maple && current == "Maple Mono NF CN" {
                                                                            format!("Maple Mono NF CN ({})", t!("software_builtin"))
                                                                        } else {
                                                                            current
                                                                        }
                                                                    })
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current = view.read(cx).terminal_font_family.to_string();
                                                                            let mut names = cx.text_system().all_font_names();
                                                                            menu = menu.min_w(0.).max_h(px(320.)).scrollable(true);
                                                                            let maple_font = "Maple Mono NF CN".to_string();
                                                                            let using_system_maple = crate::app::theme::USING_SYSTEM_MAPLE.load(std::sync::atomic::Ordering::Relaxed);
                                                                            if !using_system_maple && names.contains(&maple_font) {
                                                                                names.retain(|n| n != &maple_font);
                                                                                menu = menu.item(
                                                                                    PopupMenuItem::new(format!("{} ({})", maple_font, t!("software_builtin")))
                                                                                        .checked(current == maple_font)
                                                                                        .on_click(window.listener_for(&view, move |this, _, _window, cx| {
                                                                                            this.change_terminal_font_family("Maple Mono NF CN", cx);
                                                                                        }))
                                                                                ).separator();
                                                                            }
                                                                            for name in names {
                                                                                let checked = name == current;
                                                                                menu = menu.item(
                                                                                    PopupMenuItem::new(name.clone())
                                                                                        .checked(checked)
                                                                                        .on_click(window.listener_for(&view, move |this, _, _window, cx| {
                                                                                            this.change_terminal_font_family(&name, cx);
                                                                                        }))
                                                                                );
                                                                            }
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("local_terminal_encoding").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                let current = view.read(cx).config.local_terminal_encoding();
                                                                pointer_button("local-terminal-encoding-dropdown")

                                                                    .icon(IconName::Globe)
                                                                    .label(current.label())
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current = view.read(cx).config.local_terminal_encoding();
                                                                            menu = menu.min_w(0.);
                                                                            for encoding in TERMINAL_ENCODINGS.iter().copied() {
                                                                                menu = menu.item(
                                                                                    PopupMenuItem::new(encoding.label())
                                                                                        .checked(encoding == current)
                                                                                        .on_click(window.listener_for(&view, move |this, _, _, cx| {
                                                                                            this.change_local_terminal_encoding(encoding, cx);
                                                                                        })),
                                                                                );
                                                                            }
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("cursor_style").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                use crate::session::config::CursorStyle;
                                                                let current = view.read(cx).cursor_style;
                                                                pointer_button("cursor-style-dropdown")

                                                                    .icon(IconName::ChevronsUpDown)
                                                                    .label(match current {
                                                                        CursorStyle::Default => t!("cursor_style_default").to_string(),
                                                                        CursorStyle::Blink => t!("cursor_style_blink").to_string(),
                                                                        CursorStyle::Beam => t!("cursor_style_beam").to_string(),
                                                                        CursorStyle::BeamBlink => t!("cursor_style_beam_blink").to_string(),
                                                                    })
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            use crate::session::config::CursorStyle;
                                                                            let current = view.read(cx).cursor_style;
                                                                            menu = menu.min_w(0.).max_h(px(320.)).scrollable(true);
                                                                            for style in [
                                                                                CursorStyle::Default,
                                                                                CursorStyle::Blink,
                                                                                CursorStyle::Beam,
                                                                                CursorStyle::BeamBlink,
                                                                            ] {
                                                                                let checked = style == current;
                                                                                let label = match style {
                                                                                    CursorStyle::Default => t!("cursor_style_default").to_string(),
                                                                                    CursorStyle::Blink => t!("cursor_style_blink").to_string(),
                                                                                    CursorStyle::Beam => t!("cursor_style_beam").to_string(),
                                                                                    CursorStyle::BeamBlink => t!("cursor_style_beam_blink").to_string(),
                                                                                };
                                                                                menu = menu.item(
                                                                                    PopupMenuItem::new(label)
                                                                                        .checked(checked)
                                                                                        .on_click(window.listener_for(&view, move |this, _, _window, cx| {
                                                                                            this.change_cursor_style(style, cx);
                                                                                        }))
                                                                                );
                                                                            }
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                        )
                                        .group(
                                            SettingGroup::new()
                                                .title(t!("settings_group_other").to_string())
                                                .item(
                                                    SettingItem::new(
                                                        t!("right_click_copy_paste").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, window, cx| {
                                                                pointer_switch("right-click-copy-paste")

                                                                    .checked(view.read(cx).config.right_click_copy_paste())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_right_click_copy_paste(*checked);
                                                                        this.save_preferences_background();
                                                                        cx.notify();
                                                                    }))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    ).description(t!("copy_paste_hint").to_string())
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("keyword_highlight").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, window, cx| {
                                                                pointer_switch("keyword-highlight")

                                                                    .checked(view.read(cx).config.keyword_highlight())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_keyword_highlight(*checked);
                                                                        this.save_preferences_background();
                                                                        cx.notify();
                                                                    }))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("remember_tabs").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, window, cx| {
                                                                pointer_switch("remember-tabs")

                                                                    .checked(view.read(cx).config.remember_tabs())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_remember_tabs(*checked);
                                                                        this.capture_tabs_state();
                                                                        this.save_preferences_background();
                                                                        cx.notify();
                                                                    }))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("lock_layout").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, window, cx| {
                                                                pointer_switch("lock-layout")

                                                                    .checked(view.read(cx).config.lock_layout())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_lock_layout(*checked);
                                                                        this.save_preferences_background();
                                                                        cx.notify();
                                                                    }))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    ).description(t!("lock_layout_hint").to_string())
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("monitoring_position").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                pointer_button("monitoring-position-dropdown")

                                                                    .icon(IconName::PanelLeftOpen)
                                                                    .label({
                                                                        let pos = view.read(cx).config.monitoring_position().to_string();
                                                                        if pos == "Sidebar" {
                                                                            t!("position_sidebar").to_string()
                                                                        } else if pos == "Hidden" {
                                                                            t!("position_hidden").to_string()
                                                                        } else {
                                                                            t!("position_bottom").to_string()
                                                                        }
                                                                    })
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let pos = view.read(cx).config.monitoring_position().to_string();
                                                                            menu = menu.min_w(0.)
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("position_bottom").to_string())
                                                                                        .checked(pos == "Bottom")
                                                                                        .on_click(window.listener_for(&view, |this, _, _window, cx| {
                                                                                            this.config.set_monitoring_position("Bottom");
                                                                                            this.save_preferences_background();
                                                                                            cx.notify();
                                                                                        }))
                                                                                )
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("position_sidebar").to_string())
                                                                                        .checked(pos == "Sidebar")
                                                                                        .on_click(window.listener_for(&view, |this, _, _window, cx| {
                                                                                            this.config.set_monitoring_position("Sidebar");
                                                                                            this.save_preferences_background();
                                                                                            cx.notify();
                                                                                        }))
                                                                                )
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("position_hidden").to_string())
                                                                                        .checked(pos == "Hidden")
                                                                                        .on_click(window.listener_for(&view, |this, _, _window, cx| {
                                                                                            this.config.set_monitoring_position("Hidden");
                                                                                            this.save_preferences_background();
                                                                                            cx.notify();
                                                                                        }))
                                                                                );
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("language").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                pointer_button("language-dropdown")

                                                                    .icon(IconName::Globe)
                                                                    .label({
                                                                        let current_locale = view.read(cx).config.locale().to_string();
                                                                        if current_locale == "en" {
                                                                            t!("english").to_string()
                                                                        } else if current_locale == "zh-CN" {
                                                                            t!("chinese").to_string()
                                                                        } else {
                                                                            t!("follow_system").to_string()
                                                                        }
                                                                    })
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current_locale = view.read(cx).config.locale().to_string();
                                                                            menu = menu.min_w(0.)
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("follow_system").to_string())
                                                                                        .checked(current_locale == "system")
                                                                                        .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                                            this.set_display_language("system", window, cx)
                                                                                        }))
                                                                                )
                                                                                .separator()
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("english").to_string())
                                                                                        .checked(current_locale == "en")
                                                                                        .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                                            this.set_display_language("en", window, cx)
                                                                                        }))
                                                                                )
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("chinese").to_string())
                                                                                        .checked(current_locale == "zh-CN")
                                                                                        .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                                            this.set_display_language("zh-CN", window, cx)
                                                                                        }))
                                                                                );
                                                                            menu
                                                                        }
                                                                    })
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("reset_layout").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, window, _cx| {
                                                                pointer_button("reset-layout")

                                                                    .label(t!("reset").to_string())
                                                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                        this.reset_layout(window, cx);
                                                                    }))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    ).description(t!("reset_layout_hint").to_string())
                                                )
                                        )
                                )
                                .page(
                                    SettingPage::new(t!("settings_config_file").to_string())
                                        .icon(IconName::File)
                                        .group(
                                            SettingGroup::new()
                                                .title(t!("settings_config_file").to_string())
                                                .item(SettingItem::new(
                                                    t!("settings_backup_local_desc").to_string(),
                                                    SettingField::render({
                                                        let view = view.clone();
                                                        move |_, window, _cx| {
                                                            h_flex()
                                                                .gap_2()
                                                                .child(
                                                                    pointer_button("backup-export")

                                                                        .label(t!("backup_export").to_string())
                                                                        .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                            this.export_local_config(window, cx);
                                                                        }))
                                                                )
                                                                .child(
                                                                    pointer_button("backup-import")

                                                                        .label(t!("backup_import").to_string())
                                                                        .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                            this.import_local_config(window, cx);
                                                                        }))
                                                                )
                                                                .into_any_element()
                                                        }
                                                    })
                                                ))
                                        )
                                )
                                .page(
                                    SettingPage::new(t!("settings_sync").to_string())
                                        .icon(IconName::Globe)
                                        .group(
                                            SettingGroup::new()
                                                .title(t!("settings_sync").to_string())
                                                .item(SettingItem::render({
                                                    let view = view.clone();
                                                    let endpoint = sync_endpoint_input.clone();
                                                    let username = sync_username_input.clone();
                                                    let webdav_password = sync_webdav_password_input.clone();
                                                    let s3_endpoint = sync_s3_endpoint_input.clone();
                                                    let s3_region = sync_s3_region_input.clone();
                                                    let s3_bucket = sync_s3_bucket_input.clone();
                                                    let s3_object_key = sync_s3_object_key_input.clone();
                                                    let s3_access_key = sync_s3_access_key_input.clone();
                                                    let s3_secret_key = sync_s3_secret_key_input.clone();
                                                    let s3_session_token = sync_s3_session_token_input.clone();
                                                    let encryption_password = sync_encryption_password_input.clone();
                                                    move |_, window, cx| {
                                                        let in_progress = view.read(cx).sync_in_progress;
                                                        let status = view.read(cx).sync_status.clone();
                                                        let is_s3 = view.read(cx).config.sync_backend() == "s3";
                                                        v_flex()
                                                            .w_full()
                                                            .gap_3()
                                                            .child(
                                                                h_flex()
                                                                    .gap_2()
                                                                    .child(
                                                                        pointer_button("sync-backend-webdav")

                                                                            .label("WebDAV")
                                                                            .when(!is_s3, |button| button.primary())
                                                                            .on_click(window.listener_for(&view, |this, _, _, cx| this.set_sync_backend("webdav", cx)))
                                                                    )
                                                                    .child(
                                                                        pointer_button("sync-backend-s3")

                                                                            .label("S3")
                                                                            .when(is_s3, |button| button.primary())
                                                                            .on_click(window.listener_for(&view, |this, _, _, cx| this.set_sync_backend("s3", cx)))
                                                                    )
                                                            )
                                                            .when(!is_s3, |this| this
                                                                .child(v_flex().gap_1().child(div().text_sm().child(t!("sync_endpoint").to_string())).child(Input::new(&endpoint).w_full()))
                                                                .child(v_flex().gap_1().child(div().text_sm().child(t!("sync_username").to_string())).child(Input::new(&username).w_full()))
                                                                .child(v_flex().gap_1().child(div().text_sm().child(t!("sync_webdav_password").to_string())).child(Input::new(&webdav_password).w_full())))
                                                            .when(is_s3, |this| this
                                                                .child(v_flex().gap_1().child(div().text_sm().child(t!("sync_s3_endpoint").to_string())).child(Input::new(&s3_endpoint).w_full()))
                                                                .child(h_flex().gap_2()
                                                                    .child(v_flex().flex_1().gap_1().child(div().text_sm().child(t!("sync_s3_region").to_string())).child(Input::new(&s3_region).w_full()))
                                                                    .child(v_flex().flex_1().gap_1().child(div().text_sm().child(t!("sync_s3_bucket").to_string())).child(Input::new(&s3_bucket).w_full())))
                                                                .child(v_flex().gap_1().child(div().text_sm().child(t!("sync_s3_object_key").to_string())).child(Input::new(&s3_object_key).w_full()))
                                                                .child(v_flex().gap_1().child(div().text_sm().child(t!("sync_s3_access_key").to_string())).child(Input::new(&s3_access_key).w_full()))
                                                                .child(v_flex().gap_1().child(div().text_sm().child(t!("sync_s3_secret_key").to_string())).child(Input::new(&s3_secret_key).w_full()))
                                                                .child(v_flex().gap_1().child(div().text_sm().child(t!("sync_s3_session_token").to_string())).child(Input::new(&s3_session_token).w_full())))
                                                            .child(v_flex().gap_1().child(div().text_sm().child(t!("sync_encryption_password").to_string())).child(Input::new(&encryption_password).w_full()))
                                                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(t!("sync_security_hint").to_string()))
                                                            .child(
                                                                h_flex()
                                                                    .gap_2()
                                                                    .child(pointer_button("sync-download").disabled(in_progress).label(t!("sync_download").to_string()).on_click(window.listener_for(&view, |this, _, _, cx| this.download_sync_config(cx))))
                                                                    .child(pointer_button("sync-upload").disabled(in_progress).label(t!("sync_upload").to_string()).on_click(window.listener_for(&view, |this, _, _, cx| this.upload_sync_config(cx)))),
                                                            )
                                                            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(status))
                                                    }
                                                }))
                                        )
                                )
                                .page(
                                    SettingPage::new(t!("settings_proxy").to_string())
                                        .icon(IconName::Network)
                                        .group(
                                            SettingGroup::new()
                                                .title(t!("settings_proxy").to_string())
                                                .item(
                                                    SettingItem::new(
                                                        t!("enable_proxy").to_string(),
                                                        SettingField::render({
                                                            let view = view.clone();
                                                            move |_, window, cx| {
                                                                pointer_switch("use-proxy")

                                                                    .checked(view.read(cx).config.use_proxy())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_use_proxy(*checked);
                                                                        this.save_preferences_background();
                                                                        cx.notify();
                                                                    }))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    )
                                                )
                                                .item(
                                                    SettingItem::new(
                                                        t!("read_env_proxy").to_string(),
                                                        SettingField::render({
                                                            let view = view.clone();
                                                            move |_, window, cx| {
                                                                pointer_switch("read-env-proxy")

                                                                    .checked(view.read(cx).config.read_env_proxy())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_read_env_proxy(*checked);
                                                                        this.save_preferences_background();
                                                                        cx.notify();
                                                                    }))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    ).description(t!("read_env_proxy_desc").to_string())
                                                )
                                                .item(SettingItem::render({
                                                    let view = view.clone();
                                                    let global_proxy_host_input = view.read(cx).global_proxy_host_input.clone();
                                                    let global_proxy_port_input = view.read(cx).global_proxy_port_input.clone();
                                                    let global_proxy_user_input = view.read(cx).global_proxy_user_input.clone();
                                                    let global_proxy_password_input = view.read(cx).global_proxy_password_input.clone();
                                                    move |_, window, cx| {
                                                        let proxy_type = view.read(cx).global_proxy_type.clone();
                                                        v_flex()
                                                            .w_full()
                                                            .gap_3()
                                                            .child(div().text_sm().font_weight(FontWeight::BOLD).child(t!("global_proxy_settings").to_string()))
                                                            .child(
                                                                h_flex()
                                                                    .gap_2()
                                                                    .child(
                                                                        pointer_button("global-proxy-type-socks5")

                                                                            .label("SOCKS5")
                                                                            .when(proxy_type == "socks5", |b| b.primary())
                                                                            .on_click(window.listener_for(&view, |this, _, _, cx| {
                                                                                this.global_proxy_type = "socks5".to_string();
                                                                                cx.notify();
                                                                            }))
                                                                    )
                                                                    .child(
                                                                        pointer_button("global-proxy-type-http")

                                                                            .label("HTTP")
                                                                            .when(proxy_type == "http", |b| b.primary())
                                                                            .on_click(window.listener_for(&view, |this, _, _, cx| {
                                                                                this.global_proxy_type = "http".to_string();
                                                                                cx.notify();
                                                                            }))
                                                                    )
                                                            )
                                                            .child(v_flex().gap_1().child(div().text_sm().child(t!("global_proxy_host").to_string())).child(Input::new(&global_proxy_host_input).w_full()))
                                                            .child(v_flex().gap_1().child(div().text_sm().child(t!("global_proxy_port").to_string())).child(Input::new(&global_proxy_port_input).w_full()))
                                                            .child(v_flex().gap_1().child(div().text_sm().child(t!("global_proxy_user").to_string())).child(Input::new(&global_proxy_user_input).w_full()))
                                                            .child(v_flex().gap_1().child(div().text_sm().child(t!("global_proxy_password").to_string())).child(Input::new(&global_proxy_password_input).w_full()))
                                                            .child(
                                                                pointer_button("save-global-proxy")

                                                                    .primary()
                                                                    .label(t!("save_proxy").to_string())
                                                                    .on_click(window.listener_for(&view, |this, _, _, cx| {
                                                                        let host = this.global_proxy_host_input.read(cx).value().trim().to_string();
                                                                        let port_str = this.global_proxy_port_input.read(cx).value();
                                                                        let port = port_str.trim().parse::<u16>().ok();
                                                                        let user = this.global_proxy_user_input.read(cx).value().trim().to_string();
                                                                        let password = this.global_proxy_password_input.read(cx).value().to_string();

                                                                        if host.is_empty() || port.is_none() {
                                                                            return;
                                                                        }

                                                                        this.config.set_global_proxy_type(this.global_proxy_type.clone());
                                                                        this.config.set_global_proxy_host(host);
                                                                        this.config.set_global_proxy_port(port);
                                                                        this.config.set_global_proxy_user(user);
                                                                        this.config.set_global_proxy_password(password);
                                                                        this.save_preferences_background();
                                                                        cx.notify();
                                                                    }))
                                                            )
                                                    }
                                                }))
                                        )
                                )
                                .page({
                                    let mut page = SettingPage::new(t!("settings_key_bindings").to_string())
                                        .icon(IconName::SquareTerminal)
                                        .default_open(true);
                                    for group in crate::app::keybinding_recorder::KeybindingsPage::render_groups(&view, cx) {
                                        page = page.group(group);
                                    }
                                    page
                                })
                                .page(
                                    SettingPage::new(t!("settings_help").to_string())
                                        .icon(IconName::BookOpen)
                                )
                                .page(
                                    SettingPage::new(t!("settings_about").to_string())
                                        .icon(IconName::Info)
                                        .group(
                                            SettingGroup::new()
                                                .item(SettingItem::render(move |_, _window, cx| {
                                                    v_flex()
                                                        .gap_2()
                                                        .items_center()
                                                        .child(div().text_size(ui_rems(1.5)).font_weight(FontWeight::BOLD).child("Ashell"))
                                                        .child(div().text_size(ui_rems(0.9)).child(format!("Version {}", version)))
                                                        .child(
                                                            div()
                                                                .text_size(ui_rems(0.9))
                                                                .text_color(cx.theme().muted_foreground)
                                                                .child("A GPUI Component based SSH and local terminal client"),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(ui_rems(0.9))
                                                                .text_color(cx.theme().muted_foreground)
                                                                .child(t!("about_feedback_hint")),
                                                        )
                                                        .child(
                                                            pointer_button("github-link")
                                                                .label("https://github.com/rust-kotlin/ashell")
                                                                .ghost()
                                                                .on_click(|_, _window, _cx| {
                                                                    let _ = open::that("https://github.com/rust-kotlin/ashell");
                                                                }),
                                                        )
                                                }))
                                        )
                                )
                                )
                        )
                    }
                })
        });
    }
}
