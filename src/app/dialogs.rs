use gpui::{
    Anchor, Context, Focusable as _, FontWeight, InteractiveElement as _, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px, rems,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::Dialog,
    h_flex,
    input::Input,
    menu::{DropdownMenu as _, PopupMenuItem},
    progress::Progress,
    scroll::{Scrollbar, ScrollbarShow},
    switch::Switch,
    v_flex,
};
use rust_i18n::t;

use crate::{
    Ashell, app::ssh_key_import::KeyImportValidation, session::config::AuthMethod,
    system::format_bytes,
};

impl Ashell {
    pub(crate) fn show_ssh_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_dialog.is_some() {
            return;
        }
        self.active_dialog = Some(crate::app::DialogKind::NewSsh);

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

        window.open_dialog(cx, move |dialog: Dialog, _window, _cx| {
            dialog
                .title(t!("new_ssh_connection"))
                .w(px(520.))
                .overlay_closable(true)
                .on_close({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.active_dialog = None;
                            this.key_import.close();
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
                    move |content, window, cx| {
                        let auth_method = view.read(cx).ssh_auth_method;
                        let is_password = auth_method == AuthMethod::Password;
                        let is_key = auth_method == AuthMethod::Key;
                        let is_config = auth_method == AuthMethod::Config;
                        let is_editing = view.read(cx).editing_session_id.is_some();
                        let proxy_type = view.read(cx).ssh_proxy_type.clone();
                        let show_proxy_fields = proxy_type != "none";
                        let key_auth_incomplete = is_key
                            && !view.read(cx).using_custom_key_path
                            && view.read(cx).managed_key_selected.is_none();
                        content.child(
                            v_flex()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Button::new("ssh-auth-password")
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
                                            Button::new("ssh-auth-key")
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
                                            Button::new("ssh-auth-config")
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
                                        .child(Input::new(&host_input).tab_index(1))
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .child(
                                                    Input::new(&port_input).w(px(96.)).tab_index(2),
                                                )
                                                .child(
                                                    Input::new(&user_input).flex_1().tab_index(3),
                                                ),
                                        )
                                })
                                .when(is_password, |this| {
                                    this.child(
                                        Input::new(&password_input).mask_toggle().tab_index(4),
                                    )
                                })
                                .when(is_key, |this| {
                                    let managed_keys = view.read(cx).managed_keys.clone();
                                    let managed_key_selected =
                                        view.read(cx).managed_key_selected.clone();
                                    let using_custom_key_path = view.read(cx).using_custom_key_path;
                                    let theme = cx.theme();
                                    let using_managed_key = !using_custom_key_path;

                                    let key_label = if let Some(mk_id) = &managed_key_selected {
                                        managed_keys
                                            .iter()
                                            .find(|k| &k.id == mk_id)
                                            .map(|mk| format!("{} ({})", mk.name, mk.key_type))
                                            .unwrap_or_else(|| t!("select_managed_key").to_string())
                                    } else {
                                        t!("select_managed_key").to_string()
                                    };

                                    let this =
                                        this.child(
                                            h_flex()
                                                .w_full()
                                                .gap_1()
                                                .p(px(3.))
                                                .rounded_md()
                                                .border_1()
                                                .border_color(theme.border)
                                                .bg(theme.muted)
                                                .child(
                                                    Button::new("use-managed-key")
                                                        .small()
                                                        .flex_1()
                                                        .when(using_managed_key, |button| {
                                                            button.primary()
                                                        })
                                                        .label(t!("select_managed_key").to_string())
                                                        .on_click(window.listener_for(
                                                            &view,
                                                            |this, _, _, cx| {
                                                                this.use_managed_key(cx)
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    Button::new("use-custom-path")
                                                        .small()
                                                        .flex_1()
                                                        .when(using_custom_key_path, |button| {
                                                            button.primary()
                                                        })
                                                        .label(t!("use_custom_path").to_string())
                                                        .on_click(window.listener_for(
                                                            &view,
                                                            |this, _, _, cx| {
                                                                this.use_custom_key_path(cx)
                                                            },
                                                        )),
                                                ),
                                        );

                                    if using_managed_key {
                                        let this = this.child(
                                            Button::new("managed-key-select")
                                                .small()
                                                .w_full()
                                                .label(key_label)
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.open_managed_key_selector(window, cx);
                                                    },
                                                )),
                                        );
                                        let info = managed_key_selected
                                            .as_ref()
                                            .and_then(|id| {
                                                managed_keys.iter().find(|k| &k.id == id)
                                            })
                                            .map(|mk| {
                                                format!(
                                                    "{}: {}  |  {}: {}",
                                                    t!("key_type"),
                                                    mk.key_type,
                                                    t!("key_fingerprint"),
                                                    mk.fingerprint
                                                )
                                            });
                                        let this = if let Some(info) = info {
                                            this.child(
                                                div()
                                                    .px(px(8.))
                                                    .py(px(6.))
                                                    .rounded_md()
                                                    .border_1()
                                                    .border_color(theme.border)
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(info),
                                            )
                                        } else {
                                            this.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(
                                                        t!("select_managed_key_hint").to_string(),
                                                    ),
                                            )
                                        };
                                        this
                                    } else {
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
                                                                    this.pick_ssh_key_path(
                                                                        window, cx,
                                                                    );
                                                                },
                                                            ),
                                                        )
                                                        .child(
                                                            Input::new(&key_path_input)
                                                                .tab_index(4),
                                                        ),
                                                )
                                                .child(
                                                    Button::new("clear-key-path")
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
                                        .child(
                                            Input::new(&key_inline_input).h(px(128.)).tab_index(5),
                                        )
                                        .child(
                                            Input::new(&passphrase_input)
                                                .mask_toggle()
                                                .tab_index(6),
                                        )
                                    }
                                })
                                .when(is_config, |this| {
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
                                                Button::new("proxy-none")
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
                                                Button::new("proxy-socks5")
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
                                                Button::new("proxy-http")
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
                                .child(
                                    h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("connect-ssh-cancel")
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
                                                Button::new("connect-ssh-confirm")
                                                    .primary()
                                                    .disabled(key_auth_incomplete)
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
    pub(crate) fn show_managed_key_selector_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_dialog.is_some() {
            return;
        }
        self.active_dialog = Some(crate::app::DialogKind::ManagedKeySelector);

        let view = cx.entity();
        let rename_input = self.key_import_remark_input.clone();
        window.open_dialog(cx, move |dialog: Dialog, _window, _cx| {
            dialog
                .title(t!("select_private_key").to_string())
                .w(px(760.))
                .close_button(false)
                .overlay_closable(false)
                .content({
                    let view = view.clone();
                    let rename_input = rename_input.clone();
                    move |content, window, cx| {
                        let keys = view.read(cx).managed_keys.clone();
                        let selected = view.read(cx).managed_key_dialog_selection.clone();
                        let is_renaming = view.read(cx).editing_managed_key_id.is_some();
                        let has_selection = selected.is_some();

                        let mut rows = v_flex()
                            .h(px(220.))
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_md()
                            .overflow_hidden()
                            .child(
                                h_flex()
                                    .px_2()
                                    .py_1()
                                    .bg(cx.theme().muted)
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .child(
                                        div()
                                            .w(px(220.))
                                            .flex_shrink_0()
                                            .text_sm()
                                            .child(t!("name").to_string()),
                                    )
                                    .child(
                                        div()
                                            .w(px(110.))
                                            .flex_shrink_0()
                                            .text_sm()
                                            .child(t!("key_type").to_string()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .overflow_hidden()
                                            .text_sm()
                                            .child(t!("key_fingerprint").to_string()),
                                    ),
                            );

                        if keys.is_empty() {
                            rows = rows.child(
                                div()
                                    .flex_1()
                                    .items_center()
                                    .justify_center()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("no_managed_keys").to_string()),
                            );
                        } else {
                            for (index, key) in keys.into_iter().enumerate() {
                                let key_id = key.id.clone();
                                let is_selected = selected.as_deref() == Some(key.id.as_str());
                                let fingerprint = if key.fingerprint.len() > 24 {
                                    format!("{}…", &key.fingerprint[..24])
                                } else {
                                    key.fingerprint.clone()
                                };
                                rows = rows.child(
                                    h_flex()
                                        .id(("managed-key-choice", index))
                                        .px_2()
                                        .py_2()
                                        .cursor_pointer()
                                        .border_b_1()
                                        .border_color(cx.theme().border)
                                        .when(is_selected, |row| row.bg(cx.theme().selection))
                                        .hover(|row| row.bg(cx.theme().selection))
                                        .child(
                                            div()
                                                .w(px(220.))
                                                .flex_shrink_0()
                                                .min_w(px(0.))
                                                .overflow_hidden()
                                                .text_sm()
                                                .child(key.name),
                                        )
                                        .child(
                                            div()
                                                .w(px(110.))
                                                .flex_shrink_0()
                                                .text_sm()
                                                .child(key.key_type),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .overflow_hidden()
                                                .text_sm()
                                                .child(fingerprint),
                                        )
                                        .on_click(window.listener_for(
                                            &view,
                                            move |this, _, _, cx| {
                                                this.select_managed_key_candidate(
                                                    key_id.clone(),
                                                    cx,
                                                );
                                            },
                                        )),
                                );
                            }
                        }

                        content.child(
                            v_flex()
                                .gap_3()
                                .child(
                                    h_flex().gap_3().child(rows.flex_1()).child(
                                        v_flex()
                                            .w(px(104.))
                                            .gap_2()
                                            .child(
                                                Button::new("selector-import-key")
                                                    .w_full()
                                                    .label(t!("import_key").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, window, cx| {
                                                            this.open_key_import(window, cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("selector-edit-key")
                                                    .w_full()
                                                    .disabled(!has_selection)
                                                    .label(t!("edit").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, window, cx| {
                                                            this.begin_managed_key_rename(
                                                                window, cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("selector-delete-key")
                                                    .w_full()
                                                    .disabled(!has_selection)
                                                    .label(t!("delete").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, _, cx| {
                                                            this.delete_selected_managed_key(cx);
                                                        },
                                                    )),
                                            ),
                                    ),
                                )
                                .when(is_renaming, |this| {
                                    this.child(
                                        h_flex()
                                            .gap_2()
                                            .child(Input::new(&rename_input).flex_1())
                                            .child(
                                                Button::new("save-key-rename")
                                                    .primary()
                                                    .label(t!("save").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, _, cx| {
                                                            this.save_managed_key_rename(cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("cancel-key-rename")
                                                    .label(t!("cancel").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, _, cx| {
                                                            this.cancel_managed_key_rename(cx);
                                                        },
                                                    )),
                                            ),
                                    )
                                })
                                .child(
                                    h_flex()
                                        .justify_center()
                                        .gap_2()
                                        .child(
                                            Button::new("confirm-key-selection")
                                                .primary()
                                                .disabled(!has_selection)
                                                .label(t!("confirm").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.confirm_managed_key_selection(
                                                            window, cx,
                                                        );
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new("cancel-key-selection")
                                                .label(t!("cancel").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.return_to_ssh_dialog(window, cx);
                                                    },
                                                )),
                                        ),
                                ),
                        )
                    }
                })
        });
    }

    pub(crate) fn show_managed_key_import_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_dialog.is_some() {
            return;
        }
        self.active_dialog = Some(crate::app::DialogKind::ManagedKeyImport);

        let view = cx.entity();
        let remark_input = self.key_import_remark_input.clone();
        let passphrase_input = self.key_import_passphrase_input.clone();
        window.open_dialog(cx, move |dialog: Dialog, _window, _cx| {
            dialog
                .title(t!("key_import_dialog_title").to_string())
                .w(px(440.))
                .close_button(false)
                .overlay_closable(false)
                .content({
                    let view = view.clone();
                    let remark_input = remark_input.clone();
                    let passphrase_input = passphrase_input.clone();
                    move |content, window, cx| {
                        let path = view.read(cx).key_import.path.clone();
                        let validation = view.read(cx).key_import.validation.clone();
                        let can_confirm = validation.can_confirm();
                        let (status, status_color) = match &validation {
                            KeyImportValidation::WaitingForFile => (
                                t!("key_import_select_file_hint").to_string(),
                                cx.theme().muted_foreground,
                            ),
                            KeyImportValidation::Validating => (
                                t!("key_import_validating").to_string(),
                                cx.theme().muted_foreground,
                            ),
                            KeyImportValidation::Invalid(error) => (
                                format!("{}: {error}", t!("key_import_failed")),
                                cx.theme().danger,
                            ),
                            KeyImportValidation::Duplicate => (
                                t!("key_duplicate_fingerprint").to_string(),
                                cx.theme().danger,
                            ),
                            KeyImportValidation::Valid {
                                key_type,
                                fingerprint,
                            } => (
                                format!(
                                    "{} · {}: {}",
                                    key_type,
                                    t!("key_fingerprint"),
                                    fingerprint
                                ),
                                cx.theme().success,
                            ),
                        };

                        content.child(
                            v_flex()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .items_center()
                                        .child(
                                            div()
                                                .w(px(80.))
                                                .text_sm()
                                                .child(t!("name").to_string()),
                                        )
                                        .child(Input::new(&remark_input).flex_1()),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .w(px(80.))
                                                .text_sm()
                                                .child(t!("private_key").to_string()),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .px_3()
                                                .py_2()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(cx.theme().border)
                                                .text_sm()
                                                .overflow_hidden()
                                                .text_color(if path.is_empty() {
                                                    cx.theme().muted_foreground
                                                } else {
                                                    cx.theme().foreground
                                                })
                                                .child(if path.is_empty() {
                                                    t!("key_import_choose_file").to_string()
                                                } else {
                                                    path
                                                }),
                                        )
                                        .child(
                                            Button::new("browse-key-import")
                                                .label(t!("browse").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.pick_managed_key_import_file(
                                                            window, cx,
                                                        );
                                                    },
                                                )),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .child(
                                            div()
                                                .w(px(80.))
                                                .text_sm()
                                                .child(t!("key_passphrase").to_string()),
                                        )
                                        .child(
                                            Input::new(&passphrase_input).flex_1().mask_toggle(),
                                        ),
                                )
                                .child(
                                    div()
                                        .pl(px(80.))
                                        .text_xs()
                                        .text_color(status_color)
                                        .child(status),
                                )
                                .child(
                                    h_flex()
                                        .justify_center()
                                        .gap_2()
                                        .child(
                                            Button::new("confirm-key-import")
                                                .primary()
                                                .disabled(!can_confirm)
                                                .label(t!("confirm").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.confirm_managed_key_import(window, cx);
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new("cancel-key-import")
                                                .label(t!("cancel").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.close_key_import(window, cx);
                                                    },
                                                )),
                                        ),
                                ),
                        )
                    }
                })
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
                                                        .text_size(rems(1.0))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(t!("local_terminal")),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(rems(0.917))
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
                                                        .text_size(rems(1.0))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child(t!("new_ssh_connection")),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(rems(0.917))
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
                                                            let detail = format!(
                                                                "{}@{}:{}",
                                                                session.user,
                                                                session.host,
                                                                session.port
                                                            );
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
                                                                    .text_size(rems(1.0))
                                                                    .font_weight(
                                                                        FontWeight::SEMIBOLD,
                                                                    )
                                                                    .child(name),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_size(rems(0.917))
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
                                Button::new("clear_transfers_btn")
                                    .small()
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
                                    Button::new("close_dialog")
                                        .small()
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

                            let (status_text, actions) = match t.state {
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
                                    let btn_pause = Button::new(SharedString::from(format!(
                                        "pause-{}",
                                        t.info.id
                                    )))
                                    .ghost()
                                    .small()
                                    .icon(IconName::Pause)
                                    .on_click(window.listener_for(&view, {
                                        let id = t.info.id.clone();
                                        move |this, _, _, _| {
                                            if let Some(handle) = this.active_sftp_handle() {
                                                handle.pause_transfer(id.clone());
                                            }
                                        }
                                    }));
                                    let btn_cancel = Button::new(SharedString::from(format!(
                                        "cancel-{}",
                                        t.info.id
                                    )))
                                    .ghost()
                                    .small()
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
                                    let btn_resume = Button::new(SharedString::from(format!(
                                        "resume-{}",
                                        t.info.id
                                    )))
                                    .ghost()
                                    .small()
                                    .icon(IconName::Play)
                                    .on_click(window.listener_for(&view, {
                                        let id = t.info.id.clone();
                                        move |this, _, _, _| {
                                            if let Some(handle) = this.active_sftp_handle() {
                                                handle.resume_transfer(id.clone());
                                            }
                                        }
                                    }));
                                    let btn_cancel = Button::new(SharedString::from(format!(
                                        "cancel-{}",
                                        t.info.id
                                    )))
                                    .ghost()
                                    .small()
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
                                    let btn_remove = Button::new(SharedString::from(format!(
                                        "remove-{}",
                                        t.info.id
                                    )))
                                    .ghost()
                                    .small()
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
                                        let btn_folder = Button::new(SharedString::from(format!(
                                            "folder-{}",
                                            t.info.id
                                        )))
                                        .ghost()
                                        .small()
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
                                    let btn_remove = Button::new(SharedString::from(format!(
                                        "remove-{}",
                                        t.info.id
                                    )))
                                    .ghost()
                                    .small()
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
                                    let btn_remove = Button::new(SharedString::from(format!(
                                        "remove-{}",
                                        t.info.id
                                    )))
                                    .ghost()
                                    .small()
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
                                    let btn_remove = Button::new(SharedString::from(format!(
                                        "remove-{}",
                                        t.info.id
                                    )))
                                    .ghost()
                                    .small()
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
                                            Button::new(SharedString::from(format!(
                                                "icon-{}",
                                                t.info.id
                                            )))
                                            .icon(icon)
                                            .ghost()
                                            .small()
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
                                            .text_size(rems(0.917))
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
                                    div().text_size(rems(1.0)).mb_2().child(
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
                            Button::new("cancel")
                                .ghost()
                                .label(t!("cancel").to_string())
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                }),
                        )
                        .child(
                            Button::new("confirm")
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
    pub(crate) fn show_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_dialog.is_some() {
            return;
        }
        self.active_dialog = Some(crate::app::DialogKind::Settings);

        let view = cx.entity();

        // Unbind all workspace keys so they don't interfere with keybinding recording
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
                                                cx.notify();
                                                return;
                                            }

                                            this.recording_action = None;
                                            this.keybind_error = None;
                                            this.config.set_key_binding(&action, &new_key);
                                            if let Err(err) = this.config.save() {
                                                tracing::error!("failed to save key binding: {err:#}");
                                            }
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
                                                                Button::new("theme-mode-dropdown")
                                                                    .small()
                                                                    .icon(if follow_system { IconName::Sun } else if is_dark_mode { IconName::Moon } else { IconName::Sun })
                                                                    .label(if follow_system { t!("follow_system").to_string() } else if is_dark_mode { t!("use_dark_mode").to_string() } else { t!("use_light_mode").to_string() })
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let (follow_system, is_dark_mode) = {
                                                                                let state = view.read(cx);
                                                                                (state.follow_system_theme, state.theme_mode.is_dark())
                                                                            };
                                                                            menu = menu.min_w(160.)
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
                                                                Button::new("light-theme-dropdown")
                                                                    .small()
                                                                    .icon(IconName::Sun)
                                                                    .label(current_theme.clone())
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current_theme = view.read(cx).light_theme_name.to_string();
                                                                            let themes = gpui_component::ThemeRegistry::global(cx).sorted_themes();
                                                                            let light_themes: Vec<_> = themes.into_iter().filter(|t| !t.mode.is_dark()).map(|t| t.name.clone()).collect();
                                                                            menu = menu.min_w(160.).max_h(px(320.)).scrollable(true);
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
                                                                Button::new("dark-theme-dropdown")
                                                                    .small()
                                                                    .icon(IconName::Moon)
                                                                    .label(current_theme.clone())
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current_theme = view.read(cx).dark_theme_name.to_string();
                                                                            let themes = gpui_component::ThemeRegistry::global(cx).sorted_themes();
                                                                            let dark_themes: Vec<_> = themes.into_iter().filter(|t| t.mode.is_dark()).map(|t| t.name.clone()).collect();
                                                                            menu = menu.min_w(160.).max_h(px(320.)).scrollable(true);
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
                                                                Button::new("title-bar-style-dropdown")
                                                                    .small()
                                                                    .label(match current_style {
                                                                        crate::session::config::TitleBarStyle::Native => t!("title_bar_native").to_string(),
                                                                        crate::session::config::TitleBarStyle::Integrated => t!("title_bar_integrated").to_string(),
                                                                    })
                                                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                                                        let view = view.clone();
                                                                        move |mut menu, window, cx| {
                                                                            let current_style = view.read(cx).config.title_bar_style();
                                                                            menu = menu.min_w(160.)
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("title_bar_native").to_string())
                                                                                        .checked(current_style == crate::session::config::TitleBarStyle::Native)
                                                                                        .on_click(window.listener_for(&view, |this, _, _, cx| {
                                                                                            this.config.set_title_bar_style(crate::session::config::TitleBarStyle::Native);
                                                                                            let _ = this.config.save();
                                                                                            cx.notify();
                                                                                        }))
                                                                                )
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("title_bar_integrated").to_string())
                                                                                        .checked(current_style == crate::session::config::TitleBarStyle::Integrated)
                                                                                        .on_click(window.listener_for(&view, |this, _, _, cx| {
                                                                                            this.config.set_title_bar_style(crate::session::config::TitleBarStyle::Integrated);
                                                                                            let _ = this.config.save();
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
                                                                    .child(Button::new("ui-font-size-down").small().label("-").on_click(window.listener_for(&view, |this, _, _, cx| this.change_ui_font_size(-1.0, cx))))
                                                                    .child(div().min_w(px(64.)).text_center().child(format!("{:.0}px", view.read(cx).ui_font_size)))
                                                                    .child(Button::new("ui-font-size-up").small().label("+").on_click(window.listener_for(&view, |this, _, _, cx| this.change_ui_font_size(1.0, cx))))
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
                                                                    .child(Button::new("terminal-font-size-down").small().label("-").on_click(window.listener_for(&view, |this, _, _, cx| this.change_terminal_font_size(-1.0, cx))))
                                                                    .child(div().min_w(px(64.)).text_center().child(format!("{:.0}px", view.read(cx).terminal_font_size)))
                                                                    .child(Button::new("terminal-font-size-up").small().label("+").on_click(window.listener_for(&view, |this, _, _, cx| this.change_terminal_font_size(1.0, cx))))
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
                                                                Button::new("ui-font-dropdown")
                                                                    .small()
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
                                                                            menu = menu.min_w(200.).max_h(px(320.)).scrollable(true);
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
                                                                Button::new("terminal-font-dropdown")
                                                                    .small()
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
                                                                            menu = menu.min_w(200.).max_h(px(320.)).scrollable(true);
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
                                                        t!("cursor_style").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, _window, cx| {
                                                                use crate::session::config::CursorStyle;
                                                                let current = view.read(cx).cursor_style;
                                                                Button::new("cursor-style-dropdown")
                                                                    .small()
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
                                                                            menu = menu.min_w(160.).max_h(px(320.)).scrollable(true);
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
                                                                Switch::new("right-click-copy-paste")
                                                                    .small()
                                                                    .checked(view.read(cx).config.right_click_copy_paste())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_right_click_copy_paste(*checked);
                                                                        let _ = this.config.save();
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
                                                                Switch::new("keyword-highlight")
                                                                    .small()
                                                                    .checked(view.read(cx).config.keyword_highlight())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_keyword_highlight(*checked);
                                                                        let _ = this.config.save();
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
                                                                Switch::new("lock-layout")
                                                                    .small()
                                                                    .checked(view.read(cx).config.lock_layout())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_lock_layout(*checked);
                                                                        let _ = this.config.save();
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
                                                                Button::new("monitoring-position-dropdown")
                                                                    .small()
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
                                                                            menu = menu.min_w(160.)
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("position_bottom").to_string())
                                                                                        .checked(pos == "Bottom")
                                                                                        .on_click(window.listener_for(&view, |this, _, _window, cx| {
                                                                                            this.config.set_monitoring_position("Bottom");
                                                                                            let _ = this.config.save();
                                                                                            cx.notify();
                                                                                        }))
                                                                                )
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("position_sidebar").to_string())
                                                                                        .checked(pos == "Sidebar")
                                                                                        .on_click(window.listener_for(&view, |this, _, _window, cx| {
                                                                                            this.config.set_monitoring_position("Sidebar");
                                                                                            let _ = this.config.save();
                                                                                            cx.notify();
                                                                                        }))
                                                                                )
                                                                                .item(
                                                                                    PopupMenuItem::new(t!("position_hidden").to_string())
                                                                                        .checked(pos == "Hidden")
                                                                                        .on_click(window.listener_for(&view, |this, _, _window, cx| {
                                                                                            this.config.set_monitoring_position("Hidden");
                                                                                            let _ = this.config.save();
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
                                                                Button::new("language-dropdown")
                                                                    .small()
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
                                                                            menu = menu.min_w(160.)
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
                                                                Button::new("reset-layout")
                                                                    .small()
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
                                        // ── SSH Keys management group ──
                                        .group(
                                            SettingGroup::new()
                                                .title(t!("settings_group_keys").to_string())
                                                .item(
                                                    SettingItem::new(
                                                        t!("key_management").to_string(),
                                                        SettingField::render({
                                                            let view = view_clone_for_general.clone();
                                                            move |_, window, _cx| {
                                                                Button::new("import-key")
                                                                    .small()
                                                                    .primary()
                                                                    .label(t!("import_key").to_string())
                                                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                                                        this.import_managed_key(window, cx);
                                                                    }))
                                                                    .into_any_element()
                                                            }
                                                        })
                                                    ).description(t!("key_management_desc").to_string())
                                                )
                                                .item(
                                                    SettingItem::render({
                                                        let view = view_clone_for_general.clone();
                                                        move |_, _window, cx| {
                                                            let keys = view.read(cx).managed_keys.clone();
                                                            if keys.is_empty() {
                                                                return div()
                                                                    .py(px(12.))
                                                                    .text_color(cx.theme().muted_foreground)
                                                                    .child(t!("no_managed_keys").to_string())
                                                                    .into_any_element();
                                                            }
                                                            let mut list = v_flex().gap_2();
                                                            for key in keys {
                                                                let view = view.clone();
                                                                let key_id = key.id.clone();
                                                                let key_name = key.name.clone();
                                                                let key_type = key.key_type.clone();
                                                                let fingerprint = {
                                                                    let fp = key.fingerprint.clone();
                                                                    if fp.len() > 30 {
                                                                        format!("{}…", &fp[..30])
                                                                    } else {
                                                                        fp
                                                                    }
                                                                };
                                                                let is_editing = view.read(cx).editing_managed_key_id.as_deref() == Some(&key.id);
                                                                let rename_input = view.read(cx).key_inline_input.clone();
                                                                list = list.child(
                                                                    h_flex()
                                                                        .items_center()
                                                                        .gap_2()
                                                                        .px(px(8.))
                                                                        .py(px(6.))
                                                                        .rounded_md()
                                                                        .border_1()
                                                                        .border_color(cx.theme().border)
                                                                        .child(
                                                                            div()
                                                                                .px(px(6.))
                                                                                .py(px(2.))
                                                                                .rounded(px(4.))
                                                                                .text_xs()
                                                                                .text_color(cx.theme().primary)
                                                                                .bg(cx.theme().accent.opacity(0.15))
                                                                                .child(key_type)
                                                                        )
                                                                        .child(
                                                                            if is_editing {
                                                                                div().flex_1().child(
                                                                                    Input::new(&rename_input)
                                                                                        .small()
                                                                                )
                                                                            } else {
                                                                                div().flex_1().child(key_name.clone())
                                                                            }
                                                                        )
                                                                        .child(
                                                                            div()
                                                                                .text_xs()
                                                                                .text_color(cx.theme().muted_foreground)
                                                                                .child(fingerprint)
                                                                        )
                                                                        .child(
                                                                            if is_editing {
                                                                                Button::new(SharedString::from(format!("key-save-{}", key_id)))
                                                                                    .ghost()
                                                                                    .xsmall()
                                                                                    .icon(IconName::Check)
                                                                                    .on_click({
                                                                                        let view = view.clone();
                                                                                        let key_id = key_id.clone();
                                                                                        move |_, _window, cx| {
                                                                                            let new_name = view.read(cx).key_inline_input.read(cx).value().trim().to_string();
                                                                                            if !new_name.is_empty() {
                                                                                                let key_id = key_id.clone();
                                                                                                view.update(cx, |this, cx| {
                                                                                                    this.rename_managed_key(key_id, new_name, cx);
                                                                                                });
                                                                                            }
                                                                                        }
                                                                                    })
                                                                            } else {
                                                                                Button::new(SharedString::from(format!("key-rename-{}", key_id)))
                                                                                    .ghost()
                                                                                    .xsmall()
                                                                                    .label(t!("key_rename").to_string())
                                                                                    .on_click({
                                                                                        let view = view.clone();
                                                                                        let key_id = key_id.clone();
                                                                                        let key_name = key_name.clone();
                                                                                        move |_, window, cx| {
                                                                                            let key_name = key_name.clone();
                                                                                            view.update(cx, |this, cx| {
                                                                                                this.editing_managed_key_id = Some(key_id.clone());
                                                                                                Self::set_input_value(&this.key_inline_input, key_name, window, cx);
                                                                                                cx.notify();
                                                                                            });
                                                                                        }
                                                                                    })
                                                                            }
                                                                        )
                                                                        .child(
                                                                            Button::new(SharedString::from(format!("key-delete-{}", key_id)))
                                                                                .ghost()
                                                                                .xsmall()
                                                                                .icon(IconName::Delete)
                                                                                .on_click({
                                                                                    let view = view.clone();
                                                                                    let key_id = key_id.clone();
                                                                                    move |_, _window, cx| {
                                                                                        let key_id = key_id.clone();
                                                                                        view.update(cx, |this, cx| {
                                                                                            this.delete_managed_key(key_id, cx);
                                                                                        });
                                                                                    }
                                                                                })
                                                                        )
                                                                );
                                                            }
                                                            list.into_any_element()
                                                        }
                                                    })
                                                )
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
                                                                        Button::new("sync-backend-webdav")
                                                                            .small()
                                                                            .label("WebDAV")
                                                                            .when(!is_s3, |button| button.primary())
                                                                            .on_click(window.listener_for(&view, |this, _, _, cx| this.set_sync_backend("webdav", cx)))
                                                                    )
                                                                    .child(
                                                                        Button::new("sync-backend-s3")
                                                                            .small()
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
                                                                    .child(Button::new("sync-download").small().disabled(in_progress).label(t!("sync_download").to_string()).on_click(window.listener_for(&view, |this, _, _, cx| this.download_sync_config(cx))))
                                                                    .child(Button::new("sync-upload").small().disabled(in_progress).label(t!("sync_upload").to_string()).on_click(window.listener_for(&view, |this, _, _, cx| this.upload_sync_config(cx)))),
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
                                                                Switch::new("use-proxy")
                                                                    .small()
                                                                    .checked(view.read(cx).config.use_proxy())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_use_proxy(*checked);
                                                                        let _ = this.config.save();
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
                                                                Switch::new("read-env-proxy")
                                                                    .small()
                                                                    .checked(view.read(cx).config.read_env_proxy())
                                                                    .on_click(window.listener_for(&view, |this, checked, _, cx| {
                                                                        this.config.set_read_env_proxy(*checked);
                                                                        let _ = this.config.save();
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
                                                                        Button::new("global-proxy-type-socks5")
                                                                            .small()
                                                                            .label("SOCKS5")
                                                                            .when(proxy_type == "socks5", |b| b.primary())
                                                                            .on_click(window.listener_for(&view, |this, _, _, cx| {
                                                                                this.global_proxy_type = "socks5".to_string();
                                                                                cx.notify();
                                                                            }))
                                                                    )
                                                                    .child(
                                                                        Button::new("global-proxy-type-http")
                                                                            .small()
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
                                                                Button::new("save-global-proxy")
                                                                    .small()
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
                                                                        let _ = this.config.save();
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
                                                        .child(div().text_size(rems(1.5)).font_weight(FontWeight::BOLD).child("Ashell"))
                                                        .child(div().text_size(rems(0.9)).child(format!("Version {}", version)))
                                                        .child(
                                                            div()
                                                                .text_size(rems(0.9))
                                                                .text_color(cx.theme().muted_foreground)
                                                                .child("A GPUI Component based SSH and local terminal client"),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(rems(0.9))
                                                                .text_color(cx.theme().muted_foreground)
                                                                .child(t!("about_feedback_hint")),
                                                        )
                                                        .child(
                                                            Button::new("github-link")
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
