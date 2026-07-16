use crate::app::resizable::{h_resizable, resizable_panel, v_resizable};
use gpui::{
    AppContext as _, Context, DispatchPhase, ElementId, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, PathBuilder, Pixels, Render, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, hsla, point, prelude::FluentBuilder as _, px, relative, rems,
    uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, ElementExt, Icon, IconName, InteractiveElementExt as _, Root,
    Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::Input,
    menu::{ContextMenuExt as _, PopupMenuItem},
    progress::Progress,
    scroll::{ScrollableElement as _, Scrollbar, ScrollbarShow},
    tab::{Tab, TabBar},
    v_flex,
};
use rust_i18n::t;

use crate::{
    Ashell, PaneLayout,
    app::TabContextMenuState,
    app::constants::{COLLAPSED_SIDEBAR_WIDTH, SIDEBAR_WIDTH, TERMINAL_KEY_CONTEXT},
    sftp::format_mtime,
    sftp::ops::is_editable_text_file,
    system::format_bytes,
    terminal::{self, TabKind},
};

impl Ashell {
    fn render_home_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .h_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .text_size(rems(2.333))
                    .font_weight(FontWeight::BOLD)
                    .child("Ashell"),
            )
            .child(
                div()
                    .text_size(rems(1.083))
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("open_local_or_ssh")),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("home-open-local")
                            .primary()
                            .label(t!("local_terminal").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.open_local(cx))),
                    )
                    .child(
                        Button::new("home-open-session")
                            .ghost()
                            .label(t!("open_session").to_string())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_selector_dialog(window, cx)
                            })),
                    ),
            )
    }

    pub(crate) fn toggle_sftp_minimized(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.body_panels.clone();
        let minimized = self.sftp_panel_minimized;

        if !minimized {
            let sizes = state.read(cx).sizes();
            if sizes.len() > 1 {
                self.prev_monitoring_size = Some(sizes[1]);
            }
            self.sftp_panel_minimized = true;
        } else {
            self.sftp_panel_minimized = false;
            let prev_size = self.prev_monitoring_size.unwrap_or(px(328.));

            cx.on_next_frame(
                window,
                move |_this: &mut crate::app::Ashell,
                      window: &mut gpui::Window,
                      cx: &mut gpui::Context<crate::app::Ashell>| {
                    cx.on_next_frame(
                        window,
                        move |this: &mut crate::app::Ashell,
                              window: &mut gpui::Window,
                              cx: &mut gpui::Context<crate::app::Ashell>| {
                            this.body_panels.update(cx, |state, cx| {
                                let sizes = state.sizes();
                                let c_size_f32: f32 = sizes.iter().map(|s| s.as_f32()).sum();
                                let c_size = px(c_size_f32);

                                if c_size > px(0.0) && prev_size < c_size {
                                    let target_p0 = c_size - prev_size;
                                    state.resize_panel(0, target_p0, window, cx);
                                }
                            });
                            cx.notify();
                        },
                    );
                },
            );
        }
        self.config
            .set_sftp_panel_minimized(self.sftp_panel_minimized);
        let _ = self.config.save();
        cx.notify();
    }

    fn render_sftp_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active_sftp = self.active_sftp();

        // Compute active download progress for status bar and minimized header
        let build_summary = |kind: crate::terminal::TransferType| -> Option<(String, String, f32)> {
            let active: Vec<&crate::terminal::Transfer> = self
                .transfers
                .iter()
                .filter(|t| {
                    matches!(
                        t.state,
                        crate::terminal::TransferState::Running
                            | crate::terminal::TransferState::Paused
                    ) && t.info.kind == kind
                })
                .collect();
            if active.is_empty() {
                return None;
            }
            Some(if active.len() == 1 {
                let t = &active[0];
                let pct = t.total.and_then(|total| {
                    if total > 0 {
                        Some((t.transferred as f64 / total as f64 * 100.0) as f32)
                    } else {
                        None
                    }
                });
                match pct {
                    Some(pct) => (t.info.name.clone(), format!("{:.0}%", pct), pct),
                    None => (t.info.name.clone(), "-".to_string(), 0.0),
                }
            } else {
                let total_transferred: u64 = active.iter().map(|t| t.transferred).sum();
                let total_total: u64 = active.iter().filter_map(|t| t.total).sum();
                let pct = if total_total > 0 {
                    Some((total_transferred as f64 / total_total as f64 * 100.0) as f32)
                } else {
                    None
                };
                let label = match kind {
                    crate::terminal::TransferType::Download => {
                        t!("files_downloading", count = active.len()).to_string()
                    }
                    crate::terminal::TransferType::Upload => {
                        t!("files_uploading", count = active.len()).to_string()
                    }
                };
                match pct {
                    Some(pct) => (label, format!("{:.0}%", pct), pct),
                    None => (label, "-".to_string(), 0.0),
                }
            })
        };
        let dl_summary = build_summary(crate::terminal::TransferType::Download);
        let ul_summary = build_summary(crate::terminal::TransferType::Upload);
        let has_transfers = dl_summary.is_some() || ul_summary.is_some();

        let header = h_flex()
            .flex_none()
            .h(px(34.))
            .px_2()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tab_bar)
            .child(
                div()
                    .text_size(rems(1.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().primary)
                    .child(t!("remote_files")),
            )
            .child(div().flex_1())
            .when_some(active_sftp.clone(), |this, sftp| {
                let selected_entries = sftp.selected_entries.clone();
                this.child(
                    Button::new("sftp-sync-cwd")
                        .ghost()
                        .small()
                        .icon(IconName::SquareTerminal)
                        .label(t!("sync_cwd").to_string())
                        .tooltip(t!("sync_cwd_tooltip").to_string())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.sync_cwd_from_terminal(window, cx);
                        })),
                )
                .child(
                    Button::new("sftp-refresh")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowRight)
                        .label(t!("refresh").to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_sftp(cx))),
                )
                .child(
                    Button::new("sftp-new-folder")
                        .ghost()
                        .small()
                        .icon(IconName::Folder)
                        .label(t!("new_folder").to_string())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.sftp_creating_folder = true;
                            this.sftp_new_folder_input.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                                input.focus_handle(cx).focus(window, cx);
                            });
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("sftp-delete-selected")
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .label(if selected_entries.is_empty() {
                            t!("delete_selected").to_string()
                        } else {
                            format!(
                                "{} ({})",
                                t!("delete_selected").to_string(),
                                selected_entries.len()
                            )
                        })
                        .disabled(selected_entries.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_delete_confirm_dialog(window, cx);
                        })),
                )
                .child(
                    Button::new("sftp-upload-file")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label(t!("upload_file").to_string())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.upload_sftp_files(window, cx)),
                        ),
                )
                .child(
                    Button::new("sftp-upload-folder")
                        .ghost()
                        .small()
                        .icon(IconName::Folder)
                        .label(t!("upload_folder").to_string())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.upload_sftp_folder(window, cx)),
                        ),
                )
                .child(
                    Button::new("sftp-download-selected")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowDown)
                        .label(if selected_entries.is_empty() {
                            t!("download").to_string()
                        } else {
                            t!("download_count", count = selected_entries.len()).to_string()
                        })
                        .disabled(selected_entries.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.download_selected_sftp_entries(window, cx);
                        })),
                )
                .child(
                    Checkbox::new("sftp-show-hidden")
                        .small()
                        .label(t!("hidden").to_string())
                        .checked(self.show_hidden_files)
                        .tab_stop(false)
                        .on_click(cx.listener(|this, checked, _, cx| {
                            this.show_hidden_files = *checked;
                            this.config.set_show_hidden_files(*checked);
                            let _ = this.config.save();
                            cx.notify();
                        })),
                )
            });
        let Some(sftp) = active_sftp else {
            let mut outer = v_flex()
                .gap_0()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .flex_1()
                .child(
                    v_flex()
                        .flex_1()
                        .min_h(px(0.))
                        .when(self.sftp_panel_minimized, |this| this.hidden())
                        .child(header)
                        .child(
                            v_flex()
                                .flex_1()
                                .items_center()
                                .justify_center()
                                .p_3()
                                .child(
                                    div()
                                        .text_size(rems(1.0))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("open_ssh_tab_sftp")),
                                ),
                        ),
                );
            outer = outer.child(
                h_flex()
                    .flex_none()
                    .h(px(24.))
                    .px_3()
                    .items_center()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().tab_bar)
                    .child(div().flex_1())
                    .child(
                        Button::new("open-transfers")
                            .ghost()
                            .small()
                            .when(has_transfers, |this| {
                                let mut content = h_flex().items_center().gap_2();
                                if let Some((ref label, ref pct_display, pct)) = dl_summary {
                                    content = content.child(
                                        h_flex()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                Icon::new(IconName::ArrowDown)
                                                    .with_size(Size::Small)
                                                    .text_color(cx.theme().primary),
                                            )
                                            .child(
                                                div()
                                                    .text_size(rems(0.833))
                                                    .text_color(cx.theme().primary)
                                                    .italic()
                                                    .child(label.clone()),
                                            )
                                            .child(
                                                Progress::new("sftp-status-dl")
                                                    .with_size(px(4.))
                                                    .value(pct)
                                                    .color(cx.theme().primary)
                                                    .w(px(50.0)),
                                            )
                                            .child(
                                                div()
                                                    .text_size(rems(0.833))
                                                    .text_color(cx.theme().primary)
                                                    .italic()
                                                    .child(pct_display.clone()),
                                            ),
                                    );
                                }
                                if let Some((ref label, ref pct_display, pct)) = ul_summary {
                                    if dl_summary.is_some() {
                                        content = content.child(div().w(px(6.)));
                                    }
                                    content = content.child(
                                        h_flex()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                Icon::new(IconName::ArrowUp)
                                                    .with_size(Size::Small)
                                                    .text_color(cx.theme().primary),
                                            )
                                            .child(
                                                div()
                                                    .text_size(rems(0.833))
                                                    .text_color(cx.theme().primary)
                                                    .italic()
                                                    .child(label.clone()),
                                            )
                                            .child(
                                                Progress::new("sftp-status-ul")
                                                    .with_size(px(4.))
                                                    .value(pct)
                                                    .color(cx.theme().primary)
                                                    .w(px(50.0)),
                                            )
                                            .child(
                                                div()
                                                    .text_size(rems(0.833))
                                                    .text_color(cx.theme().primary)
                                                    .italic()
                                                    .child(pct_display.clone()),
                                            ),
                                    );
                                }
                                this.child(content)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_transfers_dialog(window, cx);
                                    }))
                            })
                            .when(!has_transfers, |this| {
                                this.icon(IconName::ArrowDown)
                                    .label(t!("transfers").to_string())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_transfers_dialog(window, cx);
                                    }))
                            }),
                    )
                    .child(
                        Button::new("sftp-minimize-toggle")
                            .ghost()
                            .small()
                            .icon(if self.sftp_panel_minimized {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .label(if self.sftp_panel_minimized {
                                t!("panel_expand").to_string()
                            } else {
                                t!("panel_minimize").to_string()
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_sftp_minimized(window, cx);
                            })),
                    ),
            );
            return outer.into_any_element();
        };

        let selected_path = sftp.selected_path.clone();
        let entries = sftp
            .entries
            .clone()
            .into_iter()
            .filter(|entry| self.show_hidden_files || !entry.name.starts_with('.'))
            .collect::<Vec<_>>();
        let status = sftp.status.clone();
        let selected_entries = sftp.selected_entries.clone();
        let all_selected = !entries.is_empty()
            && entries
                .iter()
                .all(|e| selected_entries.contains(&e.full_path));
        let parent_path = Self::sftp_parent_path(&sftp.current_path);
        let view = cx.entity();
        let icon_col_width = px(14.);
        let size_col_width = px(96.);
        let modified_col_width = px(152.);

        let mut outer = v_flex()
            .gap_0()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .flex_1()
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, _window, cx| {
                    let paths_to_upload: Vec<String> = paths
                        .0
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    this.upload_sftp_files_batch(paths_to_upload, cx);
                }),
            );

        outer = outer.child(
            v_flex()
                .flex_1()
                .min_h(px(0.))
                .when(self.sftp_panel_minimized, |this| this.hidden())
                .child(header)
                .child(
                    h_flex()
                        .h(px(36.))
                        .items_center()
                        .gap_2()
                        .px_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted)
                        .child(
                            Button::new("sftp-up")
                                .ghost()
                                .small()
                                .icon(IconName::ChevronUp)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.navigate_sftp(parent_path.clone(), cx);
                                })),
                        )
                        .child(Input::new(&self.sftp_path_input).flex_1().tab_index(0))
                        .child(div().flex_none()),
                )
                .child(
                    h_flex()
                        .h(px(26.))
                        .px_3()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted.opacity(0.8))
                        .child(
                            h_flex()
                                .w(px(24.))
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .child(
                                    Checkbox::new("sftp-select-all")
                                        .checked(all_selected)
                                        .on_click(cx.listener(move |this, checked, _, cx| {
                                            this.toggle_all_sftp_entries(*checked, cx);
                                        })),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .items_center()
                                .gap_2()
                                .child(div().w(icon_col_width).flex_none())
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(rems(0.917))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("name")),
                                ),
                        )
                        .child(
                            div()
                                .w(size_col_width)
                                .flex_none()
                                .text_size(rems(0.917))
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("size")),
                        )
                        .child(
                            div()
                                .w(modified_col_width)
                                .flex_none()
                                .text_size(rems(0.917))
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("modified")),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .relative()
                        .min_h(px(0.))
                        .child({
                            let entries = entries.clone();
                            let selected_entries = selected_entries.clone();
                            let selected_path = selected_path.clone();
                            let view = view.clone();
                            let theme = cx.theme().clone();
                            let icon_col_width = icon_col_width;
                            let size_col_width = size_col_width;
                            let modified_col_width = modified_col_width;
                            uniform_list(
                                "sftp-entries-list",
                                entries.len(),
                                move |range, window, _cx| {
                                    range
                                        .into_iter()
                                        .filter_map(|ix| {
                                            let entry = entries.get(ix)?;
                                            let entry = entry.clone();
                                            let is_checked =
                                                selected_entries.contains(&entry.full_path);
                                            let is_selected = selected_path.as_deref()
                                                == Some(entry.full_path.as_str());
                                            let name_color = if entry.is_dir {
                                                theme.primary
                                            } else {
                                                theme.foreground
                                            };
                                            let bg = if is_selected {
                                                theme.secondary
                                            } else if ix % 2 == 0 {
                                                theme.background
                                            } else {
                                                theme.muted.opacity(0.5)
                                            };
                                            Some(
                                            h_flex()
                                                .w_full()
                                                .h(px(28.))
                                                .items_center()
                                                .px_3()
                                                .gap_2()
                                                .bg(bg)
                                                .hover(|style| style.bg(theme.muted.opacity(0.8)))
                                                .border_b_1()
                                                .border_color(theme.border.opacity(0.35))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    window.listener_for(&view, {
                                                        let entry = entry.clone();
                                                        move |this, event: &MouseDownEvent, _, cx| {
                                                            this.dismiss_sftp_context_menu(cx);
                                                            this.select_sftp_entry(
                                                                entry.clone(),
                                                                cx,
                                                            );
                                                            // 双击非目录文件 → 内置编辑器
                                                            if event.click_count >= 2 && !entry.is_dir {
                                                                this.open_file_in_editor(
                                                                    entry.full_path.clone(),
                                                                    cx,
                                                                );
                                                            }
                                                        }
                                                    }),
                                                )
                                                .on_mouse_down(
                                                    MouseButton::Right,
                                                    window.listener_for(&view, {
                                                        let entry = entry.clone();
                                                        let remote_path = entry.full_path.clone();
                                                        move |this, event: &MouseDownEvent, _, cx| {
                                                            this.mark_sftp_entry_selected(
                                                                &entry.full_path,
                                                                cx,
                                                            );
                                                            this.open_sftp_context_menu(
                                                                remote_path.clone(),
                                                                entry.is_dir,
                                                                event.position,
                                                                cx,
                                                            );
                                                        }
                                                    }),
                                                )
                                                .child(
                                                    h_flex()
                                                        .w(px(24.))
                                                        .flex_none()
                                                        .items_center()
                                                        .justify_center()
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            |_, _, cx| cx.stop_propagation(),
                                                        )
                                                        .on_mouse_down(
                                                            MouseButton::Right,
                                                            |_, _, cx| cx.stop_propagation(),
                                                        )
                                                        .child(
                                                            Checkbox::new(ElementId::Name(
                                                                format!(
                                                                    "check-{}",
                                                                    entry.full_path
                                                                )
                                                                .into(),
                                                            ))
                                                            .checked(is_checked)
                                                            .on_click(window.listener_for(&view, {
                                                                let path = entry.full_path.clone();
                                                                move |this, checked, _, cx| {
                                                                    this.toggle_sftp_entry(
                                                                        path.clone(),
                                                                        *checked,
                                                                        cx,
                                                                    );
                                                                }
                                                            })),
                                                        ),
                                                )
                                                .child(
                                                    h_flex()
                                                        .flex_1()
                                                        .min_w(px(0.))
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .w(icon_col_width)
                                                                .flex_none()
                                                                .text_size(rems(1.0))
                                                                .text_color(name_color)
                                                                .child(if entry.is_dir {
                                                                    "📁"
                                                                } else {
                                                                    "📄"
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w(px(0.))
                                                                .overflow_hidden()
                                                                .text_size(rems(1.0))
                                                                .text_color(name_color)
                                                                .child(entry.name),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .w(size_col_width)
                                                        .flex_none()
                                                        .text_size(rems(0.917))
                                                        .text_color(theme.muted_foreground)
                                                        .child(if entry.is_dir {
                                                            "-".to_string()
                                                        } else {
                                                            format_bytes(entry.size)
                                                        }),
                                                )
                                                .child(
                                                    div()
                                                        .w(modified_col_width)
                                                        .flex_none()
                                                        .text_size(rems(0.917))
                                                        .text_color(theme.muted_foreground)
                                                        .child(format_mtime(entry.modified)),
                                                )
                                                .child(div().w(px(12.)).flex_none())
                                                .into_any_element(),
                                        )
                                        })
                                        .collect::<Vec<_>>()
                                },
                            )
                            .size_full()
                            .track_scroll(&self.remote_files_scroll_handle)
                        })
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(px(16.))
                                .child(
                                    Scrollbar::vertical(&self.remote_files_scroll_handle)
                                        .scrollbar_show(ScrollbarShow::Always),
                                ),
                        ),
                ),
        );
        outer = outer.child(
            h_flex()
                .flex_none()
                .h(px(24.))
                .px_3()
                .items_center()
                .border_t_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().tab_bar)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_size(rems(0.833))
                        .text_color(cx.theme().primary)
                        .italic()
                        .child(status),
                )
                .child(
                    Button::new("open-transfers")
                        .ghost()
                        .small()
                        .when(has_transfers, |this| {
                            let mut content = h_flex().items_center().gap_2();
                            if let Some((ref label, ref pct_display, pct)) = dl_summary {
                                content = content.child(
                                    h_flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Icon::new(IconName::ArrowDown)
                                                .with_size(Size::Small)
                                                .text_color(cx.theme().primary),
                                        )
                                        .child(
                                            div()
                                                .text_size(rems(0.833))
                                                .text_color(cx.theme().primary)
                                                .italic()
                                                .child(label.clone()),
                                        )
                                        .child(
                                            Progress::new("sftp-status-dl")
                                                .with_size(px(4.))
                                                .value(pct)
                                                .color(cx.theme().primary)
                                                .w(px(50.0)),
                                        )
                                        .child(
                                            div()
                                                .text_size(rems(0.833))
                                                .text_color(cx.theme().primary)
                                                .italic()
                                                .child(pct_display.clone()),
                                        ),
                                );
                            }
                            if let Some((ref label, ref pct_display, pct)) = ul_summary {
                                if dl_summary.is_some() {
                                    content = content.child(div().w(px(6.)));
                                }
                                content = content.child(
                                    h_flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Icon::new(IconName::ArrowUp)
                                                .with_size(Size::Small)
                                                .text_color(cx.theme().primary),
                                        )
                                        .child(
                                            div()
                                                .text_size(rems(0.833))
                                                .text_color(cx.theme().primary)
                                                .italic()
                                                .child(label.clone()),
                                        )
                                        .child(
                                            Progress::new("sftp-status-ul")
                                                .with_size(px(4.))
                                                .value(pct)
                                                .color(cx.theme().primary)
                                                .w(px(50.0)),
                                        )
                                        .child(
                                            div()
                                                .text_size(rems(0.833))
                                                .text_color(cx.theme().primary)
                                                .italic()
                                                .child(pct_display.clone()),
                                        ),
                                );
                            }
                            this.child(content)
                        })
                        .when(!has_transfers, |this| {
                            this.icon(IconName::ArrowDown)
                                .label(t!("transfers").to_string())
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.show_transfers_dialog(window, cx);
                        })),
                )
                .child(
                    Button::new("sftp-minimize-toggle")
                        .ghost()
                        .small()
                        .icon(if self.sftp_panel_minimized {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .label(if self.sftp_panel_minimized {
                            t!("panel_expand").to_string()
                        } else {
                            t!("panel_minimize").to_string()
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_sftp_minimized(window, cx);
                        })),
                ),
        );

        outer.into_any_element()
    }

    fn render_monitoring_panel(
        &mut self,
        viewport_width: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cpu_color = cx.theme().chart_1;
        let mem_color = cx.theme().chart_2;
        let swap_color = cx.theme().chart_3;
        let net_color = cx.theme().chart_4;
        let disk_color = cx.theme().chart_5;
        let border_color = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;

        let cpu_pct = self.system.cpu_percent;
        // Dynamic CPU line color: green <30%, amber 30-80%, red >80%
        // NOTE: Hsla.h is normalized 0..=1 (not degrees)
        let cpu_path_color = {
            let pct = cpu_pct * 100.0;
            if pct < 30.0 {
                Hsla {
                    h: 120.0 / 360.0,
                    s: 0.65,
                    l: 0.45,
                    a: 1.0,
                }
            } else if pct < 80.0 {
                Hsla {
                    h: 45.0 / 360.0,
                    s: 0.8,
                    l: 0.55,
                    a: 1.0,
                }
            } else {
                Hsla {
                    h: 0.0,
                    s: 0.8,
                    l: 0.55,
                    a: 1.0,
                }
            }
        };
        // Network TX color: derived from net_color for visual distinction from RX
        let net_tx_color = if net_color.l > 0.5 {
            Hsla {
                l: net_color.l * 0.6,
                ..net_color
            }
        } else {
            Hsla {
                l: net_color.l * 1.5,
                ..net_color
            }
        };
        let mem_pct = self.system.mem_percent;
        let swap_pct = self.system.swap_percent;
        let mem_detail = self.system.mem_detail.clone();
        let swap_detail = self.system.swap_detail.clone();
        let net_rx = self.system.net_rx.clone();
        let net_tx = self.system.net_tx.clone();

        let (disk_used, disk_total) = self.system.disks.iter().fold((0u64, 0u64), |(u, t), d| {
            (u + (d.total_bytes - d.available_bytes), t + d.total_bytes)
        });
        let disk_pct = if disk_total > 0 {
            disk_used as f64 / disk_total as f64 * 100.0
        } else {
            0.0
        };

        let cpu_spark_data = self.cpu_history.clone();
        let net_rx_history = self.net_rx_history.clone();
        let net_tx_history = self.net_tx_history.clone();
        let disks = self.system.disks.clone();
        let card_min_w = px(110.);

        let show_net_card = viewport_width > px(750.);
        let show_disk_card = viewport_width > px(600.);

        // --- CPU card ---
        let cpu_card = v_flex()
            .min_w(card_min_w)
            .flex_1()
            .h_full()
            .px_1()
            .py_1()
            .gap_0p5()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .child(
                        div()
                            .text_size(rems(0.833))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cpu_color)
                            .child(t!("cpu").to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(rems(0.833))
                            .text_color(muted_fg)
                            .child(format!("{:.0}%", cpu_pct * 100.0)),
                    ),
            )
            .child(
                canvas(
                    move |bounds, _window, _cx| {
                        let n = cpu_spark_data.len();
                        if n < 2 {
                            return None;
                        }
                        let mut path = PathBuilder::stroke(px(1.5));
                        let w = bounds.size.width;
                        let h = bounds.size.height;
                        let max_val = cpu_spark_data
                            .iter()
                            .cloned()
                            .fold(0.0f32, f32::max)
                            .max(0.1);
                        for (i, &val) in cpu_spark_data.iter().enumerate() {
                            let x = bounds.origin.x + w * i as f32 / (n - 1).max(1) as f32;
                            let y = bounds.origin.y + h * (1.0 - val / max_val * 0.85);
                            let pt = point(x, y);
                            if i == 0 {
                                path.move_to(pt);
                            } else {
                                path.line_to(pt);
                            }
                        }
                        path.build().ok()
                    },
                    move |_bounds, path_opt, window, _cx| {
                        if let Some(path) = path_opt {
                            window.paint_path(path, cpu_path_color);
                        }
                    },
                )
                .flex_1()
                .w_full(),
            );

        // --- MEM card: mem + swap ---
        let mem_card = v_flex()
            .min_w(card_min_w)
            .flex_1()
            .h_full()
            .px_1()
            .py_1()
            .gap_0p5()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .child(
                        div()
                            .text_size(rems(0.833))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(mem_color)
                            .child(t!("mem").to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(rems(0.833))
                            .text_color(muted_fg)
                            .child(format!("{:.0}%", mem_pct * 100.0)),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_1()
                    .child(
                        Progress::new("mem-progress")
                            .value(mem_pct * 100.0)
                            .color(mem_color)
                            .with_size(px(5.))
                            .flex_1(),
                    )
                    .child(
                        div()
                            .text_size(rems(0.7))
                            .text_color(muted_fg)
                            .child(mem_detail),
                    ),
            )
            .when(self.system.total_swap > 0, |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_1()
                        .child(
                            Progress::new("swap-progress")
                                .value(swap_pct * 100.0)
                                .color(swap_color)
                                .with_size(px(4.))
                                .flex_1(),
                        )
                        .child(
                            div()
                                .text_size(rems(0.7))
                                .text_color(muted_fg)
                                .child(swap_detail),
                        ),
                )
            });

        // --- NET card: rx/tx text + dual sparkline ---
        let net_card = if show_net_card {
            Some(
                v_flex()
                    .min_w(card_min_w)
                    .flex_1()
                    .h_full()
                    .px_1()
                    .py_1()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .child(
                                div()
                                    .text_size(rems(0.833))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(net_color)
                                    .child(t!("net").to_string()),
                            )
                            .child(div().flex_1())
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_size(rems(0.75))
                                            .text_color(net_color)
                                            .child(format!("↓{}", net_rx)),
                                    )
                                    .child(
                                        div()
                                            .text_size(rems(0.75))
                                            .text_color(net_tx_color)
                                            .child(format!("↑{}", net_tx)),
                                    ),
                            ),
                    )
                    .child(
                        canvas(
                            move |bounds, _window, _cx| {
                                let n_rx = net_rx_history.len();
                                let n_tx = net_tx_history.len();
                                if n_rx < 2 || n_tx < 2 {
                                    return None;
                                }
                                let all: Vec<f32> = net_rx_history
                                    .iter()
                                    .chain(net_tx_history.iter())
                                    .cloned()
                                    .collect();
                                let max_val = all.iter().cloned().fold(0.0f32, f32::max).max(1.0);
                                let w = bounds.size.width;
                                let h = bounds.size.height;
                                let mut paths = Vec::new();

                                let mut rx_path = PathBuilder::stroke(px(1.5));
                                for (i, &val) in net_rx_history.iter().enumerate() {
                                    let x =
                                        bounds.origin.x + w * i as f32 / (n_rx - 1).max(1) as f32;
                                    let y = bounds.origin.y + h * (1.0 - val / max_val * 0.85);
                                    let pt = point(x, y);
                                    if i == 0 {
                                        rx_path.move_to(pt);
                                    } else {
                                        rx_path.line_to(pt);
                                    }
                                }
                                if let Ok(path) = rx_path.build() {
                                    paths.push((path, net_color));
                                }

                                let mut tx_path = PathBuilder::stroke(px(1.0));
                                for (i, &val) in net_tx_history.iter().enumerate() {
                                    let x =
                                        bounds.origin.x + w * i as f32 / (n_tx - 1).max(1) as f32;
                                    let y = bounds.origin.y + h * (1.0 - val / max_val * 0.85);
                                    let pt = point(x, y);
                                    if i == 0 {
                                        tx_path.move_to(pt);
                                    } else {
                                        tx_path.line_to(pt);
                                    }
                                }
                                if let Ok(path) = tx_path.build() {
                                    paths.push((path, net_tx_color));
                                }

                                Some(paths)
                            },
                            move |_bounds, paths_opt, window, _cx| {
                                if let Some(paths) = paths_opt {
                                    for (path, color) in paths {
                                        window.paint_path(path, color);
                                    }
                                }
                            },
                        )
                        .flex_1()
                        .w_full(),
                    ),
            )
        } else {
            None
        };

        // --- DISK card ---
        let disk_card = if show_disk_card {
            Some(
                v_flex()
                    .min_w(card_min_w)
                    .flex_1()
                    .h_full()
                    .px_1()
                    .py_1()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .child(
                                div()
                                    .text_size(rems(0.833))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(disk_color)
                                    .child(t!("disk").to_string()),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(rems(0.833))
                                    .text_color(muted_fg)
                                    .child(format!("{:.0}%", disk_pct)),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .child(
                                v_flex()
                                    .id("disk-scroll")
                                    .track_scroll(&self.disk_scroll_handle)
                                    .overflow_y_scroll()
                                    .size_full()
                                    .children(disks.iter().map(|disk| {
                                        let pct = if disk.total_bytes > 0 {
                                            (disk.total_bytes - disk.available_bytes) as f64
                                                / disk.total_bytes as f64
                                                * 100.0
                                        } else {
                                            0.0
                                        };
                                        let mount_short = disk.mount.clone();
                                        let mount_id = format!("disk-{}", mount_short);
                                        h_flex()
                                            .w_full()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_size(rems(0.667))
                                                    .text_color(muted_fg)
                                                    .child(mount_short),
                                            )
                                            .child(
                                                Progress::new(mount_id)
                                                    .value(pct as f32)
                                                    .color(disk_color)
                                                    .with_size(px(4.))
                                                    .flex_1(),
                                            )
                                            .child(
                                                div()
                                                    .text_size(rems(0.667))
                                                    .text_color(muted_fg)
                                                    .child(format!("{:.0}%", pct)),
                                            )
                                    })),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .bottom_0()
                                    .w(px(8.))
                                    .child(
                                        Scrollbar::vertical(&self.disk_scroll_handle)
                                            .scrollbar_show(ScrollbarShow::Scrolling),
                                    ),
                            )
                            .into_any_element(),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };

        let mut panel = h_flex()
            .h(px(80.))
            .w_full()
            .flex_none()
            .px_3()
            .gap_3()
            .border_b_1()
            .border_color(border_color)
            .bg(cx.theme().muted);

        panel = panel.child(cpu_card);
        panel = panel.child(mem_card);
        if let Some(card) = net_card {
            panel = panel.child(card);
        }
        if let Some(card) = disk_card {
            panel = panel.child(card);
        }
        panel
    }

    fn render_sidebar_monitoring_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cpu_pct = self.system.cpu_percent;
        let mem_pct = self.system.mem_percent;
        let swap_pct = self.system.swap_percent;

        let cpu_color = cx.theme().chart_1;
        let mem_color = cx.theme().chart_2;
        let swap_color = cx.theme().chart_3;
        let disk_color = cx.theme().chart_5;
        let net_color = cx.theme().chart_4;
        let muted_fg = cx.theme().muted_foreground;

        v_flex()
            .gap_4()
            .w_full()
            .p_2()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(rems(0.85))
                                    .text_color(cpu_color)
                                    .child(t!("cpu").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(rems(0.85))
                                    .text_color(muted_fg)
                                    .child(format!("{:.1}%", cpu_pct * 100.0)),
                            ),
                    )
                    .child(
                        Progress::new("sidebar-cpu")
                            .value(cpu_pct * 100.0)
                            .color(cpu_color)
                            .with_size(px(4.))
                            .w_full(),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(rems(0.85))
                                    .text_color(mem_color)
                                    .child(t!("mem").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(rems(0.85))
                                    .text_color(muted_fg)
                                    .child(self.system.mem_detail.clone()),
                            ),
                    )
                    .child(
                        Progress::new("sidebar-mem")
                            .value(mem_pct * 100.0)
                            .color(mem_color)
                            .with_size(px(4.))
                            .w_full(),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(rems(0.85))
                                    .text_color(swap_color)
                                    .child(t!("swap").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(rems(0.85))
                                    .text_color(muted_fg)
                                    .child(self.system.swap_detail.clone()),
                            ),
                    )
                    .child(
                        Progress::new("sidebar-swap")
                            .value(swap_pct * 100.0)
                            .color(swap_color)
                            .with_size(px(4.))
                            .w_full(),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_size(rems(0.85))
                                    .text_color(disk_color)
                                    .child(t!("disk").to_string()),
                            )
                            .children(if self.system.disks.len() > 3 {
                                Some(
                                    div()
                                        .text_size(rems(0.65))
                                        .text_color(muted_fg)
                                        .child(t!("scroll").to_string()),
                                )
                            } else {
                                None
                            }),
                    )
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .child(
                                v_flex()
                                    .id("sidebar-disk-scroll")
                                    .track_scroll(&self.disk_scroll_handle)
                                    .overflow_y_scroll()
                                    .max_h(px(90.))
                                    .gap_2()
                                    .children(self.system.disks.iter().map(|disk| {
                                        let pct = if disk.total_bytes > 0 {
                                            (disk.total_bytes - disk.available_bytes) as f64
                                                / disk.total_bytes as f64
                                                * 100.0
                                        } else {
                                            0.0
                                        };
                                        let mount_short = disk.mount.clone();
                                        let mount_id = format!("sidebar-disk-{}", mount_short);
                                        v_flex()
                                            .gap_0p5()
                                            .child(
                                                h_flex()
                                                    .justify_between()
                                                    .child(
                                                        div()
                                                            .text_size(rems(0.75))
                                                            .text_color(muted_fg)
                                                            .child(mount_short),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(rems(0.75))
                                                            .text_color(muted_fg)
                                                            .child(format!("{:.1}%", pct)),
                                                    ),
                                            )
                                            .child(
                                                Progress::new(mount_id)
                                                    .value(pct as f32)
                                                    .color(disk_color)
                                                    .with_size(px(4.))
                                                    .w_full(),
                                            )
                                    })),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .bottom_0()
                                    .w(px(8.))
                                    .child(
                                        Scrollbar::vertical(&self.disk_scroll_handle)
                                            .scrollbar_show(ScrollbarShow::Scrolling),
                                    ),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(rems(0.85))
                                    .text_color(net_color)
                                    .child(t!("net").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(rems(0.85))
                                    .text_color(muted_fg)
                                    .child(t!("live")),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(rems(0.75))
                                            .text_color(net_color)
                                            .child("↓"),
                                    )
                                    .child(
                                        div()
                                            .text_size(rems(0.75))
                                            .child(self.system.net_rx.clone()),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(rems(0.75))
                                            .text_color(cx.theme().chart_5)
                                            .child("↑"),
                                    )
                                    .child(
                                        div()
                                            .text_size(rems(0.75))
                                            .child(self.system.net_tx.clone()),
                                    ),
                            ),
                    ),
            )
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sessions = self.config.sessions().to_vec();
        let active_session_id = self.active_session_id().map(ToOwned::to_owned);

        v_flex()
            .gap_4()
            .w_full()
            .h_full()
            .min_w(px(0.))
            .p_4()
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().sidebar)
            .overflow_hidden()
            .child(
                v_flex()
                    .gap_1()
                    .min_w(px(0.))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::BOLD)
                                    .text_size(rems(1.667))
                                    .text_color(cx.theme().primary)
                                    .child("Ashell"),
                            )
                            .child(div().flex_1())
                            .child(
                                Button::new("sidebar-collapse-toggle")
                                    .ghost()
                                    .icon(IconName::PanelLeftClose)
                                    .tooltip(t!("settings_toggle_sidebar").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sidebar_collapsed = true;
                                        this.config.set_sidebar_collapsed(true);
                                        let _ = this.config.save();
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("sidebar-settings")
                                    .ghost()
                                    .icon(IconName::Settings)
                                    .tooltip(t!("settings_open_settings").to_string())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_settings_dialog(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_size(rems(0.917))
                            .text_color(cx.theme().muted_foreground)
                            .child({
                                if let Some(kind) = self.active_kind() {
                                    match kind {
                                        TabKind::Local => t!("local_terminal").to_string(),
                                        TabKind::Ssh => {
                                            if let Some((_, session)) = self.active_ssh_session() {
                                                format!("ssh / {}", session.name)
                                            } else {
                                                "ssh".to_string()
                                            }
                                        }
                                    }
                                } else {
                                    self.active_title()
                                }
                            }),
                    ),
            )
            .when(self.config.monitoring_position() == "Sidebar", |this| {
                this.child(self.render_sidebar_monitoring_panel(cx))
            })
            .child(
                Button::new("open-ssh-panel")
                    .primary()
                    .label(t!("add_ssh").to_string())
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_new_ssh_dialog(window, cx)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .gap_2()
                    .child(
                        div()
                            .text_size(rems(1.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().primary)
                            .child(t!("saved")),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .size_full()
                            .child(
                                v_flex()
                                    .size_full()
                                    .id("saved-sessions-scroll")
                                    .track_scroll(&self.saved_scroll_handle)
                                    .overflow_y_scroll()
                                    .gap_2()
                                    .children(sessions.into_iter().enumerate().map(
                                        |(ix, session)| {
                                            let connect_id = session.id.clone();
                                            let edit_id = session.id.clone();
                                            let delete_id = session.id.clone();
                                            let is_active = active_session_id.as_deref()
                                                == Some(session.id.as_str());
                                            let name = session.name.clone();
                                            let detail = self.session_detail(&session);
                                            div()
                                                .id(("saved-connect", ix))
                                                .w_full()
                                                .p_2()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(if is_active {
                                                    cx.theme().primary
                                                } else {
                                                    cx.theme().border
                                                })
                                                .bg(if is_active {
                                                    cx.theme().tab_active
                                                } else {
                                                    cx.theme().muted
                                                })
                                                .cursor_pointer()
                                                .hover(|this| this.bg(cx.theme().secondary))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, _, _, cx| {
                                                        this.connect_saved_session(
                                                            connect_id.clone(),
                                                            cx,
                                                        )
                                                    }),
                                                )
                                                .context_menu({
                                                    let view = cx.entity();
                                                    move |menu, window, _| {
                                                        let edit_value = edit_id.clone();
                                                        let clone_value = edit_id.clone();
                                                        let delete_value = delete_id.clone();
                                                        menu.item(
                                                            PopupMenuItem::new(
                                                                t!("clone").to_string(),
                                                            )
                                                            .on_click(window.listener_for(
                                                                &view,
                                                                move |this, _, window, cx| {
                                                                    this.clone_saved_session(
                                                                        clone_value.clone(),
                                                                        window,
                                                                        cx,
                                                                    )
                                                                },
                                                            )),
                                                        )
                                                        .item(
                                                            PopupMenuItem::new(
                                                                t!("edit").to_string(),
                                                            )
                                                            .on_click(window.listener_for(
                                                                &view,
                                                                move |this, _, window, cx| {
                                                                    this.edit_saved_session(
                                                                        edit_value.clone(),
                                                                        window,
                                                                        cx,
                                                                    )
                                                                },
                                                            )),
                                                        )
                                                        .item(
                                                            PopupMenuItem::new(
                                                                t!("delete").to_string(),
                                                            )
                                                            .on_click(window.listener_for(
                                                                &view,
                                                                move |this, _, _, cx| {
                                                                    this.remove_saved_session(
                                                                        delete_value.clone(),
                                                                        cx,
                                                                    )
                                                                },
                                                            )),
                                                        )
                                                    }
                                                })
                                                .child(
                                                    v_flex()
                                                        .gap_1()
                                                        .child(
                                                            div()
                                                                .text_size(rems(1.0))
                                                                .font_weight(FontWeight::SEMIBOLD)
                                                                .child(name),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(rems(0.917))
                                                                .text_color(
                                                                    cx.theme().muted_foreground,
                                                                )
                                                                .child(detail),
                                                        ),
                                                )
                                        },
                                    )),
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
                                            &self.saved_scroll_handle,
                                        )
                                        .id("saved-scrollbar")
                                        .axis(gpui_component::scroll::ScrollbarAxis::Vertical)
                                        .scrollbar_show(
                                            gpui_component::scroll::ScrollbarShow::Always,
                                        ),
                                    ),
                            ),
                    ),
            )
    }

    fn render_collapsed_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sessions = self.config.sessions().to_vec();
        let active_session_id = self.active_session_id().map(ToOwned::to_owned);

        v_flex()
            .w_full()
            .h_full()
            .min_w(px(0.))
            .p_2()
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().sidebar)
            .overflow_hidden()
            .items_center()
            // Top: expand button only
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .pb_2()
                    .child(
                        Button::new("sidebar-expand-toggle")
                            .ghost()
                            .icon(IconName::PanelLeftOpen)
                            .tooltip(t!("settings_toggle_sidebar").to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_collapsed = false;
                                this.config.set_sidebar_collapsed(false);
                                let _ = this.config.save();
                                cx.notify();
                            })),
                    ),
            )
            // Saved sessions as compact cards
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .child(
                        v_flex()
                            .size_full()
                            .id("collapsed-saved-sessions-scroll")
                            .track_scroll(&self.collapsed_saved_scroll_handle)
                            .overflow_y_scroll()
                            .gap_2()
                            .items_center()
                            .children(sessions.into_iter().enumerate().map(|(ix, session)| {
                                let connect_id = session.id.clone();
                                let is_active =
                                    active_session_id.as_deref() == Some(session.id.as_str());
                                let name = session.name.clone();

                                // Abbreviate: first 1 char for CJK, first 2 chars for Latin
                                let abbrev = {
                                    let mut chars = name.chars();
                                    if let Some(first) = chars.next() {
                                        if first > '\u{2E7F}' {
                                            // CJK character range — show 1 char
                                            first.to_string()
                                        } else {
                                            // Latin / ASCII — show first 2 chars
                                            let mut s = first.to_string();
                                            if let Some(second) = chars.next() {
                                                s.push(second);
                                            }
                                            s
                                        }
                                    } else {
                                        "?".to_string()
                                    }
                                };

                                let edit_id = session.id.clone();
                                let delete_id = session.id.clone();
                                div()
                                    .id(("collapsed-saved", ix))
                                    .w(px(36.))
                                    .h(px(36.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(if is_active {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().border
                                    })
                                    .bg(if is_active {
                                        cx.theme().tab_active
                                    } else {
                                        cx.theme().muted
                                    })
                                    .cursor_pointer()
                                    .hover(|this| this.bg(cx.theme().secondary))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.connect_saved_session(connect_id.clone(), cx)
                                        }),
                                    )
                                    .tooltip({
                                        let tooltip_text = format!("{} {}", name, session.user);
                                        move |window, cx| {
                                            gpui_component::tooltip::Tooltip::new(
                                                tooltip_text.clone(),
                                            )
                                            .build(window, cx)
                                        }
                                    })
                                    .context_menu({
                                        let view = cx.entity();
                                        move |menu, window, _| {
                                            let edit_value = edit_id.clone();
                                            let clone_value = edit_id.clone();
                                            let delete_value = delete_id.clone();
                                            menu.item(
                                                PopupMenuItem::new(t!("clone").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        move |this, _, window, cx| {
                                                            this.clone_saved_session(
                                                                clone_value.clone(),
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            )
                                            .item(
                                                PopupMenuItem::new(t!("edit").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        move |this, _, window, cx| {
                                                            this.edit_saved_session(
                                                                edit_value.clone(),
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            )
                                            .item(
                                                PopupMenuItem::new(t!("delete").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        move |this, _, _, cx| {
                                                            this.remove_saved_session(
                                                                delete_value.clone(),
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            )
                                        }
                                    })
                                    .child(
                                        div()
                                            .text_size(rems(0.833))
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(if is_active {
                                                cx.theme().primary
                                            } else {
                                                cx.theme().foreground
                                            })
                                            .child(abbrev),
                                    )
                            })),
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
                                    &self.collapsed_saved_scroll_handle,
                                )
                                .id("collapsed-saved-scrollbar")
                                .axis(gpui_component::scroll::ScrollbarAxis::Vertical)
                                .scrollbar_show(gpui_component::scroll::ScrollbarShow::Scrolling),
                            ),
                    ),
            )
    }

    fn render_window_controls(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_macos = cfg!(target_os = "macos");
        let is_fullscreen = window.is_fullscreen();

        let is_active = cx.active_window() == Some(window.window_handle());

        h_flex()
            .group("window-controls")
            .flex_none()
            .items_center()
            .px_3()
            .gap_2()
            .when(!is_macos || is_fullscreen, |this| {
                this.child(
                    h_flex()
                        .id("window-close")
                        .size(px(12.))
                        .rounded_full()
                        .bg(if is_active {
                            hsla(3.0 / 360.0, 1.0, 0.67, 1.0)
                        } else {
                            hsla(0.0, 0.0, 0.8, 1.0)
                        }) // Red or Inactive Grey
                        .group_hover("window-controls", |s| {
                            s.bg(hsla(3.0 / 360.0, 1.0, 0.67, 1.0))
                        })
                        .when(!is_macos, |this| {
                            this.window_control_area(gpui::WindowControlArea::Close)
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.save_layout_state(window, cx);
                            window.remove_window();
                        }))
                        .hover(|s| s.bg(hsla(3.0 / 360.0, 1.0, 0.55, 1.0)))
                        .active(|s| s.bg(hsla(3.0 / 360.0, 1.0, 0.45, 1.0)))
                        .items_center()
                        .justify_center()
                        .child(
                            h_flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(7.))
                                .font_weight(FontWeight::BOLD)
                                .line_height(relative(1.0))
                                .text_color(hsla(3.0 / 360.0, 1.0, 0.15, 0.7))
                                .opacity(0.0)
                                .group_hover("window-controls", |s| s.opacity(1.0))
                                .child("✕"),
                        ),
                )
                .child(
                    h_flex()
                        .id("window-minimize")
                        .size(px(12.))
                        .rounded_full()
                        .bg(if is_active {
                            hsla(39.0 / 360.0, 1.0, 0.59, 1.0)
                        } else {
                            hsla(0.0, 0.0, 0.8, 1.0)
                        }) // Yellow or Inactive Grey
                        .group_hover("window-controls", |s| {
                            s.bg(hsla(39.0 / 360.0, 1.0, 0.59, 1.0))
                        })
                        .when(!is_macos, |this| {
                            this.window_control_area(gpui::WindowControlArea::Min)
                        })
                        .on_click(|_, window, _| window.minimize_window())
                        .hover(|s| s.bg(hsla(39.0 / 360.0, 1.0, 0.49, 1.0)))
                        .active(|s| s.bg(hsla(39.0 / 360.0, 1.0, 0.39, 1.0)))
                        .items_center()
                        .justify_center()
                        .child(
                            h_flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(7.))
                                .font_weight(FontWeight::BOLD)
                                .line_height(relative(1.0))
                                .text_color(hsla(39.0 / 360.0, 1.0, 0.15, 0.8))
                                .opacity(0.0)
                                .group_hover("window-controls", |s| s.opacity(1.0))
                                .child("−"),
                        ),
                )
                .child(
                    h_flex()
                        .id("window-maximize")
                        .size(px(12.))
                        .rounded_full()
                        .bg(if is_active {
                            hsla(127.0 / 360.0, 0.68, 0.47, 1.0)
                        } else {
                            hsla(0.0, 0.0, 0.8, 1.0)
                        }) // Green or Inactive Grey
                        .group_hover("window-controls", |s| {
                            s.bg(hsla(127.0 / 360.0, 0.68, 0.47, 1.0))
                        })
                        .when(!is_macos, |this| {
                            this.window_control_area(gpui::WindowControlArea::Max)
                        })
                        .on_click(|_, window, _| {
                            if window.is_fullscreen() {
                                window.toggle_fullscreen();
                            } else {
                                #[cfg(target_os = "macos")]
                                window.titlebar_double_click();
                                #[cfg(not(target_os = "macos"))]
                                window.zoom_window();
                            }
                        })
                        .hover(|s| s.bg(hsla(127.0 / 360.0, 0.68, 0.37, 1.0)))
                        .active(|s| s.bg(hsla(127.0 / 360.0, 0.68, 0.27, 1.0)))
                        .items_center()
                        .justify_center()
                        .child(
                            h_flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(7.))
                                .font_weight(FontWeight::BOLD)
                                .line_height(relative(1.0))
                                .text_color(hsla(127.0 / 360.0, 1.0, 0.15, 0.8))
                                .opacity(0.0)
                                .group_hover("window-controls", |s| s.opacity(1.0))
                                .child("+"),
                        ),
                )
            })
            .when(is_macos, |this| {
                this.when(!is_fullscreen, |this| this.w(px(80.)))
            })
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let active_tab_index = self
            .active_tab
            .as_ref()
            .and_then(|active_id| self.tabs.iter().position(|tab| &tab.id == active_id));
        let active_group_index = self
            .active_group
            .as_ref()
            .and_then(|gid| self.tab_groups.iter().position(|g| g.id == *gid));
        let selected = active_group_index.unwrap_or(active_tab_index.unwrap_or(0));
        let groups_data: Vec<(String, String, Vec<String>)> = self
            .tab_groups
            .iter()
            .map(|g| {
                let pane_ids: Vec<String> = g
                    .pane_root
                    .tab_ids()
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                (g.id.clone(), g.title.clone(), pane_ids)
            })
            .collect();
        let is_integrated =
            self.active_title_bar_style == crate::session::config::TitleBarStyle::Integrated;

        h_flex()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .on_prepaint({
                        let view = view.clone();
                        move |bounds, _window, cx| {
                            let _ = view.update(cx, |this, _| {
                                this.tab_bar_bounds = Some(bounds);
                            });
                        }
                    })
                    .when(is_integrated, |this| {
                        this.window_control_area(gpui::WindowControlArea::Drag)
                    })
                    .overflow_x_hidden()
                    .child({
                        TabBar::new("ashell-tab-bar")
                            .track_scroll(&self.tabs_scroll_handle)
                            .selected_index(selected)
                            .children(groups_data.iter().enumerate().map(
                                |(ix, (group_id, title, pane_ids))| {
                                    let gid = group_id.clone();
                                    let label = if pane_ids.len() > 1 {
                                        format!("{} ({})", title, pane_ids.len())
                                    } else {
                                        title.clone()
                                    };
                                    let close_id = if self.active_group.as_ref() == Some(&gid) {
                                        self.active_tab.clone().unwrap_or_else(|| {
                                            pane_ids.first().cloned().unwrap_or_default()
                                        })
                                    } else {
                                        pane_ids.first().cloned().unwrap_or_default()
                                    };

                                    let dot_color = pane_ids
                                        .first()
                                        .and_then(|id| self.tabs.iter().find(|t| t.id == *id))
                                        .map(|tab| {
                                            if tab.connected {
                                                cx.theme().success
                                            } else {
                                                cx.theme().danger
                                            }
                                        })
                                        .unwrap_or(cx.theme().success);
                                    let drag_gid = gid.clone();
                                    let context_gid = gid.clone();
                                    let bounds_gid = gid.clone();
                                    let bounds_view = view.clone();
                                    Tab::new()
                                        .on_prepaint(move |bounds, _window, cx| {
                                            let _ = bounds_view.update(cx, |this, _| {
                                                this.tab_group_bounds
                                                    .insert(bounds_gid.clone(), bounds);
                                            });
                                        })
                                        .min_w(px(80.))
                                        .prefix(div().w(px(5.)).h(px(32.)).bg(dot_color))
                                        .child(
                                            div()
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    cx.listener(move |this, event: &MouseDownEvent, _, _| {
                                                        this.tab_drag.begin(drag_gid.clone(), event.position);
                                                    }),
                                                )
                                                .when(ix == selected, |this| {
                                                    this.font_weight(FontWeight::BOLD)
                                                        .text_color(cx.theme().primary)
                                                        .text_base()
                                                })
                                                .child(label),
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.activate_group(gid.clone(), window, cx)
                                        }))
                                        .suffix(
                                            Button::new(("tab-close", ix))
                                                .ghost()
                                                .xsmall()
                                                .icon(IconName::Close)
                                                .mr(px(5.))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    |_, window, cx| {
                                                        window.prevent_default();
                                                        cx.stop_propagation();
                                                    },
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        window.prevent_default();
                                                        cx.stop_propagation();
                                                        if !close_id.is_empty() {
                                                            this.close_tab(close_id.clone(), cx)
                                                        }
                                                    },
                                                )),
                                        )
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                                this.tab_context_menu = Some(
                                                    TabContextMenuState {
                                                        group_id: context_gid.clone(),
                                                        position: event.position,
                                                    },
                                                );
                                                cx.notify();
                                            }),
                                        )
                                },
                            ))
                            .last_empty_space(div().w_3())
                            .w_full()
                            .h_full()
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .pr(px(6.))
                    .child(
                        Button::new("open-selector")
                            .secondary()
                            .small()
                            .rounded(px(999.))
                            .icon(IconName::Plus)
                            .tooltip(t!("settings_open_session").to_string())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_selector_dialog(window, cx)
                            })),
                    )
                    .child(
                        Button::new("split-horizontal")
                            .secondary()
                            .small()
                            .rounded(px(999.))
                            .icon(IconName::PanelBottom)
                            .tooltip(t!("settings_split_pane_down").to_string())
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                this.split_current_pane("down", cx);
                            })),
                    )
                    .child(
                        Button::new("split-vertical")
                            .secondary()
                            .small()
                            .rounded(px(999.))
                            .icon(IconName::PanelRight)
                            .tooltip(t!("settings_split_pane_right").to_string())
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                this.split_current_pane("right", cx);
                            })),
                    )
                    .child(
                        Button::new("new-window")
                            .secondary()
                            .small()
                            .rounded(px(999.))
                            .icon(IconName::ExternalLink)
                            .tooltip(t!("settings_new_window").to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.open_new_window(cx);
                            })),
                    )
                    .child(self.render_search_button(cx)),
            )
    }

    fn render_terminal_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_active = self.active_tab.is_some();
        let pane_tree = self.pane_root.clone();
        let view = cx.entity();

        div()
            .size_full()
            .relative()
            .child(
                div()
                    .size_full()
                    .on_prepaint(move |bounds, _window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            if this.terminal_panel_bounds != Some(bounds) {
                                this.terminal_panel_bounds = Some(bounds);
                                cx.notify();
                            }
                        });
                    })
                    .overflow_hidden()
                    .track_focus(&self.focus_handle)
                    .key_context(TERMINAL_KEY_CONTEXT)
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::focus_terminal))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(Self::on_terminal_right_click),
                    )
                    .on_mouse_move(cx.listener(Self::on_terminal_mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_terminal_mouse_up))
                    .on_key_down(cx.listener(Self::on_terminal_key_down))
                    .on_action(cx.listener(Self::on_terminal_tab_action))
                    .on_action(cx.listener(Self::on_terminal_backtab_action))
                    .on_scroll_wheel(cx.listener(Self::on_terminal_scroll))
                    .child(if has_active {
                        Self::render_pane_tree(self, &pane_tree, &[], cx).into_any_element()
                    } else {
                        self.render_home_page(cx).into_any_element()
                    }),
            )
            // Search bar overlay — only when search is active.
            .when(self.search_active, |el| {
                el.child(self.render_search_bar(window, cx))
            })
            // Every non-reorder drag has visible feedback. A neutral destination
            // explicitly states that releasing will cancel instead of moving data.
            .when(
                (self.tab_drag.is_dragging() && self.tab_drag.reorder_index().is_none())
                    || self.incoming_tab_drag.is_some(),
                |el| el.child(self.render_tab_drag_overlay(cx)),
            )
    }

    fn render_tab_drag_overlay(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let scrim = hsla(220. / 360., 0.25, 0.08, 0.28);
        let card_bg = hsla(217. / 360., 0.88, 0.40, 0.98);
        let card_border = hsla(199. / 360., 0.95, 0.72, 1.0);
        let card_text = hsla(0., 0., 1.0, 1.0);
        let neutral_bg = hsla(32. / 360., 0.82, 0.34, 0.98);
        let neutral_border = hsla(42. / 360., 0.95, 0.68, 1.0);

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .bottom_0()
            .bg(scrim)
            .when(
                self.tab_drag.outside() && self.tab_drag.merge_target().is_none(),
                |this| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .px(px(20.))
                                    .py(px(12.))
                                    .rounded_lg()
                                    .border_2()
                                    .border_color(card_border)
                                    .bg(card_bg)
                                    .shadow_lg()
                                    .text_color(card_text)
                                    .child(
                                        Icon::new(IconName::ExternalLink)
                                            .with_size(Size::Small)
                                            .text_color(card_text),
                                    )
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(FontWeight::BOLD)
                                            .child(t!("drag_detach_hint").to_string()),
                                    ),
                            ),
                    )
                },
            )
            .when(
                self.tab_drag.is_dragging()
                    && !self.tab_drag.outside()
                    && self.tab_drag.merge_target().is_none()
                    && self.tab_drag.reorder_index().is_none()
                    && self.incoming_tab_drag.is_none(),
                |this| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .px(px(20.))
                                    .py(px(12.))
                                    .rounded_lg()
                                    .border_2()
                                    .border_color(neutral_border)
                                    .bg(neutral_bg)
                                    .shadow_lg()
                                    .text_color(card_text)
                                    .child(
                                        Icon::new(IconName::Close)
                                            .with_size(Size::Small)
                                            .text_color(card_text),
                                    )
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(FontWeight::BOLD)
                                            .child(t!("drag_cancel_hint").to_string()),
                                    ),
                            ),
                    )
                },
            )
            .when(
                self.incoming_tab_drag.is_some() || self.tab_drag.merge_target().is_some(),
                |this| {
                    this.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .px(px(20.))
                                    .py(px(12.))
                                    .rounded_lg()
                                    .border_2()
                                    .border_color(card_border)
                                    .bg(card_bg)
                                    .shadow_lg()
                                    .text_color(card_text)
                                    .child(
                                        Icon::new(IconName::ArrowDown)
                                            .with_size(Size::Small)
                                            .text_color(card_text),
                                    )
                                    .child(
                                        div()
                                            .text_base()
                                            .font_weight(FontWeight::BOLD)
                                            .child(t!("drag_merge_hint").to_string()),
                                    ),
                            ),
                    )
                },
            )
    }

    fn render_pane_tree(
        this: &mut Ashell,
        layout: &PaneLayout,
        path: &[usize],
        cx: &mut Context<Ashell>,
    ) -> impl IntoElement {
        match layout {
            PaneLayout::Single(tab_id) => {
                if tab_id.is_empty() {
                    return this.render_home_page(cx).into_any_element();
                }
                let is_focused = path == this.focused_pane_path.as_slice();
                let keyword_highlight = this.config.keyword_highlight();
                let snapshot = this
                    .tabs
                    .iter()
                    .find(|t| &t.id == tab_id)
                    .map(|t| t.render_snapshot(keyword_highlight));
                let Some(snapshot) = snapshot else {
                    return div().into_any_element();
                };
                let tab_id_clone2 = tab_id.clone();
                let focus_handle = this.focus_handle.clone();
                let marked_text = if is_focused {
                    this.terminal_marked_text.clone()
                } else {
                    None
                };
                let font_family = this.terminal_font_family.clone();
                let font_size = px(this.terminal_font_size);
                let line_height = px(this.terminal_line_height());
                let cell_width = px(this.terminal_cell_width());
                let is_url_hovered = this
                    .hovered_url
                    .as_ref()
                    .map_or(false, |hu| hu.tab_id == *tab_id);
                let mut el = div()
                    .size_full()
                    .overflow_hidden()
                    .when(is_url_hovered, |d| d.cursor_pointer())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.focus_pane_with_id(tab_id_clone2.clone());
                            cx.notify();
                        }),
                    )
                    .child(terminal::element::TerminalElement::new(
                        cx.entity(),
                        focus_handle,
                        snapshot,
                        marked_text,
                        font_family,
                        font_size,
                        line_height,
                        cell_width,
                        tab_id.to_string(),
                        this.search_highlight_map(
                            tab_id,
                            cx.theme().danger.opacity(0.35),
                            cx.theme().danger.opacity(0.70),
                        ),
                    ));
                let scrollbar = this.terminal_scrollbars.entry(tab_id.clone()).or_default();
                el = el.vertical_scrollbar(scrollbar);

                // When disconnected, overlay a reconnect bar at the bottom of the terminal.
                // Uses absolute positioning so the terminal element itself is unchanged,
                // keeping panel size stable in multi-panel layouts.
                let disconnected_reason = this
                    .tabs
                    .iter()
                    .find(|t| t.id == *tab_id)
                    .and_then(|tab| tab.disconnected_reason.clone());
                if let Some(reason) = disconnected_reason {
                    let tab_id_for_reconnect = tab_id.clone();
                    el = div().size_full().relative().child(el).child(
                        div().absolute().bottom_0().left_0().right_0().child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_1()
                                .bg(cx.theme().danger.opacity(0.15))
                                .child(
                                    div()
                                        .text_size(rems(0.85))
                                        .text_color(cx.theme().danger)
                                        .child(
                                            t!("session_disconnected", "reason" = reason)
                                                .to_string(),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(rems(0.85))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("— {}", t!("press_enter_to_reconnect"))),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.retry_disconnected_tab(&tab_id_for_reconnect, cx);
                                    }),
                                ),
                        ),
                    );
                }
                let indicator_color = this
                    .tabs
                    .iter()
                    .find(|t| t.id == *tab_id)
                    .map(|tab| {
                        if tab.connected {
                            cx.theme().success
                        } else {
                            cx.theme().danger
                        }
                    })
                    .unwrap_or(cx.theme().success);
                let has_multiple_panes = this.pane_root.tab_ids().len() > 1;

                if !is_focused {
                    el = el.opacity(0.85);
                }

                let mut wrapper = div().size_full();
                if has_multiple_panes {
                    if is_focused {
                        wrapper = wrapper
                            .relative()
                            .child(
                                div()
                                    .absolute()
                                    .top(px(1.))
                                    .left(px(1.))
                                    .right(px(1.))
                                    .h(px(1.))
                                    .bg(indicator_color),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .bottom(px(1.))
                                    .left(px(1.))
                                    .right(px(1.))
                                    .h(px(1.))
                                    .bg(indicator_color),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(px(1.))
                                    .top(px(1.))
                                    .bottom(px(1.))
                                    .w(px(1.))
                                    .bg(indicator_color),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .right(px(1.))
                                    .top(px(1.))
                                    .bottom(px(1.))
                                    .w(px(1.))
                                    .bg(indicator_color),
                            )
                            .p(px(4.))
                            .child(el);
                    } else {
                        wrapper = wrapper.p(px(4.)).child(el);
                    }
                } else {
                    wrapper = wrapper.child(el);
                }

                wrapper.into_any_element()
            }
            PaneLayout::Horizontal(children, ratio) => {
                v_flex()
                    .size_full()
                    .children(children.iter().enumerate().flat_map(|(i, child)| {
                        let mut items: Vec<gpui::AnyElement> = Vec::new();
                        if i > 0 {
                            let splitter_path = path.to_vec(); // path to the CONTAINER that has the ratio
                            items.push(
                                div()
                                    .h(px(4.))
                                    .w_full()
                                    .flex_none()
                                    .cursor_row_resize()
                                    .bg(cx.theme().border)
                                    .hover(|s| s.bg(cx.theme().accent))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, event, window, cx| {
                                            window.prevent_default();
                                            cx.stop_propagation();
                                            this.start_drag_split(
                                                splitter_path.clone(),
                                                i,
                                                event,
                                                window,
                                                cx,
                                            );
                                        }),
                                    )
                                    .into_any_element(),
                            );
                        }
                        let mut child_path = path.to_vec();
                        child_path.push(i);
                        items.push(
                            div()
                                .flex_grow(if children.len() == 2 {
                                    if i == 0 { *ratio } else { 1.0 - *ratio }
                                } else {
                                    1.0
                                })
                                .min_h(px(0.))
                                .overflow_hidden()
                                .child(Self::render_pane_tree(this, child, &child_path, cx))
                                .into_any_element(),
                        );
                        items
                    }))
                    .into_any_element()
            }
            PaneLayout::Vertical(children, ratio) => h_flex()
                .items_stretch()
                .size_full()
                .children(children.iter().enumerate().flat_map(|(i, child)| {
                    let mut items: Vec<gpui::AnyElement> = Vec::new();
                    if i > 0 {
                        let splitter_path = path.to_vec(); // path to the CONTAINER that has the ratio
                        items.push(
                            div()
                                .w(px(4.))
                                .h_full()
                                .flex_none()
                                .cursor_col_resize()
                                .bg(cx.theme().border)
                                .hover(|s| s.bg(cx.theme().accent))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event, window, cx| {
                                        window.prevent_default();
                                        cx.stop_propagation();
                                        this.start_drag_split(
                                            splitter_path.clone(),
                                            i,
                                            event,
                                            window,
                                            cx,
                                        );
                                    }),
                                )
                                .into_any_element(),
                        );
                    }
                    let mut child_path = path.to_vec();
                    child_path.push(i);
                    items.push(
                        div()
                            .flex_grow(if children.len() == 2 {
                                if i == 0 { *ratio } else { 1.0 - *ratio }
                            } else {
                                1.0
                            })
                            .min_w(px(0.))
                            .overflow_hidden()
                            .child(Self::render_pane_tree(this, child, &child_path, cx))
                            .into_any_element(),
                    );
                    items
                }))
                .into_any_element(),
        }
    }
}

impl Render for Ashell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 检查 SftpEditor 是否请求关闭
        if let Some(editor) = &self.sftp_editor {
            if editor.read(cx).should_close {
                self.sftp_editor = None;
            }
        }
        // 若有待打开的编辑请求,在此处(有 window)构造 SftpEditor
        if let Some((remote_path, content, handle)) = self.pending_edit.take() {
            let editor = cx.new(|cx| {
                crate::app::sftp_editor::SftpEditor::new(
                    remote_path, content, handle, window, cx,
                )
            });
            self.sftp_editor = Some(editor);
        }
        if self
            .active_tab
            .as_ref()
            .is_some_and(|active_id| !self.tabs.iter().any(|tab| &tab.id == active_id))
        {
            self.active_tab = self.tabs.first().map(|tab| tab.id.clone());
        }
        self.sync_sftp_path_input(window, cx);

        let view = cx.entity();
        let move_view = view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Capture {
                let _ = move_view.update(cx, |this, cx| {
                    this.on_tab_drag_mouse_move(event, window, cx);
                });
            }
        });
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                let _ = view.update(cx, |this, cx| {
                    this.on_tab_drag_mouse_up(event, window, cx);
                });
            }
        });

        // Refresh this window's screen-space bounds in the cross-window
        // registry so other windows can hit-test against it during a
        // cross-window tab drag.
        {
            let handle = window.window_handle();
            let screen_bounds = match window.window_bounds() {
                gpui::WindowBounds::Fullscreen(b)
                | gpui::WindowBounds::Maximized(b)
                | gpui::WindowBounds::Windowed(b) => b,
            };
            crate::app::update_window_bounds(handle, screen_bounds);
            if window.is_window_active() {
                crate::app::mark_window_active(handle);
            }
        }

        if self.show_transfers_dialog {
            self.show_transfers_dialog = false;
            self.show_transfers_dialog(window, cx);
        }
        if let Some(active_id) = self.active_tab.clone() {
            if let Some(scrollbar) = self.terminal_scrollbars.get(&active_id) {
                if let Some(new_display_offset) = scrollbar.future_display_offset.take() {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
                        let current = tab.display_offset();
                        match new_display_offset.cmp(&current) {
                            std::cmp::Ordering::Greater => {
                                tab.scroll_up_by(new_display_offset - current)
                            }
                            std::cmp::Ordering::Less => {
                                tab.scroll_down_by(current - new_display_offset)
                            }
                            std::cmp::Ordering::Equal => {}
                        }
                    }
                }
            }
            if let Some(snapshot) = self.active_snapshot().as_ref() {
                if let Some(scrollbar) = self.terminal_scrollbars.get(&active_id) {
                    scrollbar.update(snapshot, px(self.terminal_line_height()));
                }
            }
        }

        let monitoring_contents = v_flex()
            .size_full()
            .when(self.config.monitoring_position() == "Bottom", |this| {
                this.child(self.render_monitoring_panel(window.viewport_size().width, cx))
            })
            .child(self.render_sftp_panel(window, cx));

        let is_monitor_bottom = self.config.monitoring_position() == "Bottom";
        let minimized_height = if is_monitor_bottom { 104. } else { 24. };
        let min_panel_height = if is_monitor_bottom { 260. } else { 180. };
        let default_panel_height = if is_monitor_bottom { 328. } else { 248. };

        let sftp_size = if self.sftp_panel_minimized {
            px(minimized_height)
        } else {
            px(self
                .config
                .body_panels()
                .and_then(|s| s.get(1).copied())
                .unwrap_or(default_panel_height))
        };

        let body_panel = v_resizable("ashell-body")
            .lock(self.config.lock_layout())
            .with_state(&self.body_panels)
            .child(resizable_panel().child(self.render_terminal_panel(window, cx)))
            .child(
                resizable_panel()
                    .size(sftp_size)
                    .size_range(if self.sftp_panel_minimized {
                        px(minimized_height)..px(minimized_height)
                    } else {
                        px(min_panel_height)..px(1200.)
                    })
                    .child(monitoring_contents),
            )
            .into_any_element();

        let workspace = if self.sidebar_collapsed {
            h_flex()
                .size_full()
                .child(
                    div()
                        .flex_none()
                        .w(px(COLLAPSED_SIDEBAR_WIDTH))
                        .h_full()
                        .child(self.render_collapsed_sidebar(cx)),
                )
                .child(
                    div().flex_1().h_full().min_w(px(0.)).child(
                        v_flex()
                            .size_full()
                            .relative()
                            .overflow_hidden()
                            .when(
                                self.active_title_bar_style
                                    == crate::session::config::TitleBarStyle::Native,
                                |this| {
                                    this.child(
                                        div()
                                            .flex_none()
                                            .h(px(32.))
                                            .w_full()
                                            .bg(cx.theme().tab_bar)
                                            .border_b_1()
                                            .border_color(cx.theme().border)
                                            .child(self.render_tab_bar(cx)),
                                    )
                                },
                            )
                            .child(body_panel),
                    ),
                )
                .into_any_element()
        } else {
            let sidebar_area = resizable_panel()
                .size(px(self
                    .config
                    .workspace_panels()
                    .and_then(|s| s.first().copied())
                    .unwrap_or(SIDEBAR_WIDTH)))
                .size_range(px(240.)..px(520.))
                .flex_none()
                .child(self.sidebar(cx));

            let main_area = resizable_panel().child(
                v_flex()
                    .size_full()
                    .relative()
                    .overflow_hidden()
                    .when(
                        self.active_title_bar_style
                            == crate::session::config::TitleBarStyle::Native,
                        |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .h(px(32.))
                                    .w_full()
                                    .bg(cx.theme().tab_bar)
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .child(self.render_tab_bar(cx)),
                            )
                        },
                    )
                    .child(body_panel),
            );

            h_resizable("ashell-workspace")
                .lock(self.config.lock_layout())
                .with_state(&self.workspace_panels)
                .child(sidebar_area)
                .child(main_area)
                .into_any_element()
        };

        v_flex()
            .id("ashell-root")
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(self.ui_font_family.clone())
            // Tab dragging is tracked by window-level capture listeners so
            // movement continues after the cursor leaves the source element.
            .on_action(cx.listener(|this, _: &crate::OpenSettings, window, cx| this.show_settings_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &crate::OpenSession, window, cx| this.show_selector_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &crate::OpenTransfers, window, cx| this.show_transfers_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &crate::NewSsh, window, cx| this.show_ssh_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &crate::NewWindow, _, cx| this.open_new_window(cx)))
            .on_action(cx.listener(|this, _: &crate::DetachTabToWindow, _, cx| this.detach_tab_to_new_window(cx)))
            .on_action(cx.listener(|this, _: &crate::OpenSearch, window, cx| this.toggle_search(window, cx)))
            .on_action(cx.listener(|this, _: &crate::ToggleSidebar, _, cx| {
                this.sidebar_collapsed = !this.sidebar_collapsed;
                this.config.set_sidebar_collapsed(this.sidebar_collapsed);
                let _ = this.config.save();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &crate::ToggleSftpZoom, window, cx| {
                this.toggle_sftp_minimized(window, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::FocusPaneLeft, _, _| this.focus_adjacent_pane("left")))
            .on_action(cx.listener(|this, _: &crate::FocusPaneRight, _, _| this.focus_adjacent_pane("right")))
            .on_action(cx.listener(|this, _: &crate::FocusPaneUp, _, _| this.focus_adjacent_pane("up")))
            .on_action(cx.listener(|this, _: &crate::FocusPaneDown, _, _| this.focus_adjacent_pane("down")))
            .on_action(cx.listener(|this, _: &crate::SplitPaneLeft, _, cx| this.split_current_pane("left", cx)))
            .on_action(cx.listener(|this, _: &crate::SplitPaneRight, _, cx| this.split_current_pane("right", cx)))
            .on_action(cx.listener(|this, _: &crate::SplitPaneUp, _, cx| this.split_current_pane("up", cx)))
            .on_action(cx.listener(|this, _: &crate::SplitPaneDown, _, cx| this.split_current_pane("down", cx)))
            .on_action(cx.listener(|this, _: &crate::ClosePane, _, cx| {
                if let Some(active_id) = this.active_tab.clone() {
                    this.close_tab(active_id, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &crate::Copy, window, cx| {
                if window.focused(cx) == Some(this.focus_handle.clone()) {
                    if let Some(text) = this.active_terminal_selection_text() {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        if let Some(active_id) = &this.active_tab {
                            if let Some(tab) = this.tabs.iter_mut().find(|tab| &tab.id == active_id) {
                                tab.clear_selection();
                            }
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &crate::Paste, window, cx| {
                if window.focused(cx) == Some(this.focus_handle.clone()) {
                    if let Some(clipboard) = cx.read_from_clipboard() {
                        if let Some(text) = clipboard.text() {
                            this.paste_into_terminal(&text, window, cx);
                        }
                    }
                } else {
                    cx.propagate();
                }
            }))
            .when(self.active_title_bar_style == crate::session::config::TitleBarStyle::Integrated, |this| {
                this.child(
                    div()
                        .id("title-bar")
                        .flex()
                        .items_center()
                        .h(px(34.))
                        .w_full()
                        .bg(cx.theme().tab_bar)
                        .child(self.render_window_controls(window, cx))
                        .child(
                            div()
                                .id("tab-bar-drag")
                                .flex_1()
                                .min_w(px(0.))
                                .h_full()
                                .on_double_click(|_, window, _| {
                                    #[cfg(target_os = "macos")]
                                    window.titlebar_double_click();
                                    #[cfg(not(target_os = "macos"))]
                                    window.zoom_window();
                                })
                                .when(cfg!(target_os = "linux"), |this| {
                                    this.on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, _| {
                                            // Don't start window move if the user
                                            // might be initiating a tab drag
                                            if !this.tab_drag.is_pending()
                                                && !this.tab_drag.is_dragging()
                                            {
                                                this.should_move_window = true;
                                            }
                                        }),
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, _| {
                                            this.should_move_window = false;
                                        }),
                                    )
                                    .on_mouse_down_out(cx.listener(|this, _, _, _| {
                                        this.should_move_window = false;
                                    }))
                                    .on_mouse_move(cx.listener(|this, _, window, _| {
                                        if this.should_move_window {
                                            this.should_move_window = false;
                                            window.start_window_move();
                                        }
                                    }))
                                })
                                .child(self.render_tab_bar(cx)),
                        ),
                )
            })
            .child(
                div().flex_1().min_h_0().child(workspace),
            )
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
            // SFTP 内置编辑器 overlay
            .when_some(self.sftp_editor.clone(), |this, editor| {
                this.child(editor.clone())
            })
            .when_some(self.sftp_context_menu.clone(), |this, menu| {
                let label = if menu.is_dir {
                    t!("download_folder").to_string()
                } else {
                    t!("download").to_string()
                };
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.dismiss_sftp_context_menu(cx);
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _, _, cx| {
                                this.dismiss_sftp_context_menu(cx);
                            }),
                        )
                        .child(
                            div()
                                .absolute()
                                .left(menu.position.x)
                                .top(menu.position.y)
                                .w(px(172.))
                                .p_1()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().popover)
                                .shadow_lg()
                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                })
                                .on_mouse_down(MouseButton::Right, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                })
                                .child(
                                    v_flex()
                                        .w_full()
                                        .child(
                                            Button::new("sftp-context-download")
                                                .ghost()
                                                .w_full()
                                                .justify_start()
                                                .label(label)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.trigger_sftp_context_download(window, cx);
                                                })),
                                        )
                                        .when(
                                            !menu.is_dir
                                                && is_editable_text_file(&menu.remote_path),
                                            |this| {
                                                this.child(
                                                    Button::new("sftp-context-edit")
                                                        .ghost()
                                                        .w_full()
                                                        .justify_start()
                                                        .label(t!("edit_file"))
                                                        .tooltip(
                                                            t!("edit_file_tooltip").to_string(),
                                                        )
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.trigger_sftp_context_edit(cx);
                                                        })),
                                                )
                                            },
                                        ),
                                ),
                        ),
                )
            })
            .when_some(self.tab_context_menu.clone(), |this, menu| {
                let detach_gid = menu.group_id.clone();
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.tab_context_menu = None;
                                cx.notify();
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _, _, cx| {
                                this.tab_context_menu = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .absolute()
                                .left(menu.position.x)
                                .top(menu.position.y)
                                .w(px(200.))
                                .p_1()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().popover)
                                .shadow_lg()
                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                })
                                .on_mouse_down(MouseButton::Right, |_, window, cx| {
                                    window.prevent_default();
                                    cx.stop_propagation();
                                })
                                .child(
                                    v_flex()
                                        .w_full()
                                        .child(
                                            Button::new("tab-context-detach")
                                                .ghost()
                                                .w_full()
                                                .justify_start()
                                                .label(t!("settings_detach_tab").to_string())
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    this.tab_context_menu = None;
                                                    this.activate_group(
                                                        detach_gid.clone(),
                                                        window,
                                                        cx,
                                                    );
                                                    this.detach_tab_to_new_window(cx);
                                                })),
                                        ),
                                ),
                        ),
                )
            })
            .when_some(self.connection_progress.clone(), |this, progress| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .bg(gpui::Hsla {
                            h: 0.0,
                            s: 0.0,
                            l: 0.0,
                            a: 0.48,
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w(px(420.))
                                .p_5()
                                .rounded_lg()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().popover)
                                .shadow_lg()
                                .child(
                                    v_flex()
                                        .gap_4()
                                        .child(
                                            Button::new("ssh-connect-progress")
                                                .primary()
                                                .loading(!progress.failed)
                                                .label(progress.title.clone()),
                                        )
                                        .child(
                                            div()
                                                .relative()
                                                .min_h(px(0.))
                                                .max_h(px(220.))
                                                .child(
                                                    div()
                                                        .id("connection-progress-scroll")
                                                        .max_h(px(220.))
                                                        .overflow_hidden()
                                                        .overflow_y_scroll()
                                                        .track_scroll(&self.connection_scroll_handle)
                                                        .child(
                                                            v_flex().gap_2().children(
                                                                progress.lines.iter().cloned().map(|line| {
                                                                    div()
                                                                        .text_size(rems(1.0))
                                                                        .text_color(if progress.failed {
                                                                            cx.theme().danger
                                                                        } else {
                                                                            cx.theme().muted_foreground
                                                                        })
                                                                        .child(line)
                                                                }),
                                                            ),
                                                        )
                                                )
                                                .child(
                                                    div()
                                                        .absolute()
                                                        .top_0()
                                                        .right_0()
                                                        .bottom_0()
                                                        .w(px(16.))
                                                        .child(
                                                            Scrollbar::vertical(&self.connection_scroll_handle)
                                                                .scrollbar_show(ScrollbarShow::Scrolling)
                                                        )
                                                )
                                        )
                                        .when(progress.failed, |this| {
                                            this.child(
                                                h_flex()
                                                    .justify_end()
                                                    .gap_2()
                                                    .child(
                                                        Button::new("ssh-connect-progress-retry")
                                                            .primary()
                                                            .label(t!("retry").to_string())
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.retry_connection_progress(
                                                                        cx,
                                                                    )
                                                                },
                                                            )),
                                                    )
                                                    .child(
                                                        Button::new("ssh-connect-progress-close")
                                                            .label(t!("cancel").to_string())
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.cancel_connection_progress(
                                                                        cx,
                                                                    )
                                                                },
                                                            )),
                                                    ),
                                            )
                                        }),
                                ),
                        ),
                )
            })
            .on_prepaint({
                let view = cx.entity().clone();
                move |_, window, cx| {
                    view.update(cx, |this, cx| {
                        let current_win_size = window.viewport_size();
                        let size_changed = this.last_window_size.map_or(true, |prev| prev != current_win_size);
                        this.last_window_size = Some(current_win_size);

                        let current_sizes = this.workspace_panels.read(cx).sizes().clone();
                        if let Some(current_first_size) = current_sizes.first().copied() {
                            if size_changed {
                                if let Some(target_width) = this.last_sidebar_width {
                                    if current_first_size != target_width {
                                        this.workspace_panels.update(cx, |state, cx| {
                                            state.resize_panel(0, target_width, window, cx);
                                        });
                                    }
                                }
                            } else {
                                this.last_sidebar_width = Some(current_first_size);
                            }
                        }
                    });
                }
            })
    }
}
