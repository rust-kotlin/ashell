use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::app::resizable::{h_resizable, resizable_panel, v_resizable};
use gpui::{
    Anchor, Animation, AnimationExt as _, AppContext as _, Context, DismissEvent, ElementId, Empty,
    Focusable as _, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, ParentElement as _, PathBuilder, Pixels, Render,
    StatefulInteractiveElement as _, Styled as _, Window, bounce, canvas, div, ease_in_out, hsla,
    point, prelude::FluentBuilder as _, px, relative, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable as _, ElementExt, Icon, IconName, InteractiveElementExt as _, Root,
    Sizable as _, Size,
    button::ButtonVariants as _,
    h_flex,
    input::Input,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
    popover::Popover,
    progress::Progress,
    scroll::{ScrollableElement as _, Scrollbar, ScrollbarShow},
    spinner::Spinner,
    v_flex,
};
use rust_i18n::t;

use crate::{
    Ashell, PaneLayout,
    app::{
        ServerMonitorView,
        constants::{SIDEBAR_WIDTH, TERMINAL_KEY_CONTEXT, TERMINAL_SCROLLBAR_GUTTER},
        controls::{
            PointerClipboard, PointerSelectionCheckbox, SelectionState, pointer_button,
            pointer_checkbox, ui_rems,
        },
    },
    sftp::format_mtime,
    system::{RemotePort, RemoteProcess, format_bytes},
    terminal::{self, TabKind, TerminalTab},
    text_encoding::TERMINAL_ENCODINGS,
};

fn flashing_terminal_notification_icon(tab_index: usize, color: Hsla) -> impl IntoElement {
    Icon::new(IconName::Bell)
        .xsmall()
        .text_color(color)
        .with_animation(
            ("terminal-notification-bell", tab_index),
            Animation::new(Duration::from_millis(800))
                .repeat()
                .with_easing(bounce(ease_in_out)),
            |icon, delta| icon.opacity(0.25 + delta * 0.75),
        )
}

fn process_matches_filter(process: &RemoteProcess, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }

    format!(
        "{} {} {} {:.2} {}",
        process.pid,
        process.user,
        process.command,
        process.cpu_percent,
        format_bytes(process.memory_bytes)
    )
    .to_lowercase()
    .contains(filter)
}

fn port_matches_filter(port: &RemotePort, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }

    format!(
        "{} {} {} {} {} {}",
        port.protocol,
        port.address,
        port.port,
        port.state,
        port.pid.map(|pid| pid.to_string()).unwrap_or_default(),
        port.process
    )
    .to_lowercase()
    .contains(filter)
}

fn connection_matches_filter(session: &crate::session::config::Session, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }

    let detail = if session.protocol == "serial" {
        format!("serial {} {}", session.host, session.baud_rate)
    } else {
        format!("ssh {} {} {}", session.user, session.host, session.port)
    };
    format!("{} {} {}", session.name, session.group, detail)
        .to_lowercase()
        .contains(filter)
}

#[derive(Clone)]
struct ConnectionGroupSection {
    name: String,
    sessions: Vec<crate::session::config::Session>,
}

#[derive(Clone)]
struct TabGroupDrag {
    group_id: String,
}

fn compact_menu_width(labels: &[&str]) -> Pixels {
    let display_units = labels
        .iter()
        .map(|label| {
            label
                .chars()
                .map(|character| if character.is_ascii() { 1.0 } else { 2.0 })
                .sum::<f32>()
        })
        .fold(0.0, f32::max);
    px((display_units * 7.2 + 28.0).clamp(72.0, 240.0))
}

const TAB_SCROLL_ANIMATION_DURATION: Duration = Duration::from_millis(180);
const TAB_SCROLL_LAYOUT_RETRY_FRAMES: u8 = 3;

impl Ashell {
    fn tab_scroll_target_x(&self, index: usize) -> Option<Pixels> {
        let scroll_handle = &self.tabs_scroll_handle;
        let scroll_bounds = scroll_handle.bounds();
        let viewport = self
            .tabs_viewport_bounds
            .map(|bounds| bounds.intersect(&scroll_bounds))
            .unwrap_or(scroll_bounds);
        let tab_bounds = scroll_handle.bounds_for_item(index)?;

        if viewport.size.width <= px(0.) {
            return None;
        }

        let offset = scroll_handle.offset();
        let visible_left = tab_bounds.left() + offset.x;
        let visible_right = tab_bounds.right() + offset.x;
        let target_x = if tab_bounds.size.width >= viewport.size.width {
            viewport.left() - tab_bounds.left()
        } else if visible_left < viewport.left() {
            offset.x + viewport.left() - visible_left
        } else if visible_right > viewport.right() {
            offset.x + viewport.right() - visible_right
        } else {
            offset.x
        };

        let max_offset = scroll_handle.max_offset();
        Some(target_x.clamp(-max_offset.x, px(0.)))
    }

    fn animate_tab_scroll(
        &mut self,
        start_x: Pixels,
        target_x: Pixels,
        started_at: Instant,
        animation_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.on_next_frame(window, move |this, window, cx| {
            if this.tab_scroll_animation_id != animation_id {
                return;
            }

            let progress = (started_at.elapsed().as_secs_f32()
                / TAB_SCROLL_ANIMATION_DURATION.as_secs_f32())
            .clamp(0., 1.);
            let eased = 1. - (1. - progress).powi(3);
            let current_x = if progress >= 1. {
                target_x
            } else {
                px(start_x.as_f32() + (target_x.as_f32() - start_x.as_f32()) * eased)
            };
            let offset = this.tabs_scroll_handle.offset();
            this.tabs_scroll_handle
                .set_offset(point(current_x, offset.y));
            cx.notify();

            if progress < 1. {
                this.animate_tab_scroll(start_x, target_x, started_at, animation_id, window, cx);
            }
        });
    }

    pub(crate) fn ensure_tab_visible(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab_scroll_animation_id = self.tab_scroll_animation_id.wrapping_add(1);
        let animation_id = self.tab_scroll_animation_id;
        self.ensure_tab_visible_after_layout(
            index,
            animation_id,
            TAB_SCROLL_LAYOUT_RETRY_FRAMES,
            window,
            cx,
        );
    }

    fn ensure_tab_visible_after_layout(
        &mut self,
        index: usize,
        animation_id: u64,
        retries_remaining: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.on_next_frame(window, move |this, window, cx| {
            if this.tab_scroll_animation_id != animation_id {
                return;
            }

            let Some(target_x) = this.tab_scroll_target_x(index) else {
                if retries_remaining > 0 {
                    cx.notify();
                    this.ensure_tab_visible_after_layout(
                        index,
                        animation_id,
                        retries_remaining - 1,
                        window,
                        cx,
                    );
                }
                return;
            };
            let start_x = this.tabs_scroll_handle.offset().x;
            if (target_x.as_f32() - start_x.as_f32()).abs() <= 0.5 {
                return;
            }

            this.animate_tab_scroll(start_x, target_x, Instant::now(), animation_id, window, cx);
        });
    }

    fn tab_group_display_name(&self, group: &crate::app::TabGroup) -> String {
        let pane_ids = group.pane_root.tab_ids();
        let configured_title = group.title.trim();
        let base_title = if configured_title.is_empty() {
            pane_ids
                .iter()
                .find_map(|tab_id| {
                    self.tabs.iter().find(|tab| tab.id == *tab_id).map(|tab| {
                        let tab_title = tab.title.trim();
                        if !tab_title.is_empty() {
                            return tab_title.to_string();
                        }

                        if let Some(session) = tab.session.as_ref() {
                            if !session.name.trim().is_empty() {
                                return session.name.trim().to_string();
                            }

                            if session.protocol == "serial" {
                                return format!("serial://{}", session.host);
                            }

                            return format!("{}@{}:{}", session.user, session.host, session.port);
                        }

                        t!("local_terminal").to_string()
                    })
                })
                .unwrap_or_else(|| t!("local_terminal").to_string())
        } else {
            configured_title.to_string()
        };

        if pane_ids.len() > 1 {
            format!("{} ({})", base_title, pane_ids.len())
        } else {
            base_title
        }
    }

    fn render_home_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .h_full()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                h_flex()
                    .size(px(40.))
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(cx.theme().secondary)
                    .child(
                        Icon::new(IconName::SquareTerminal)
                            .with_size(Size::Large)
                            .text_color(cx.theme().primary),
                    ),
            )
            .child(
                div()
                    .text_size(ui_rems(1.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Ashell"),
            )
            .child(
                div()
                    .text_size(ui_rems(1.083))
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("open_local_or_ssh")),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        pointer_button("home-open-local")
                            .primary()
                            .label(t!("local_terminal").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.open_local(cx))),
                    )
                    .child(
                        pointer_button("home-open-session")
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

        // Changing the SFTP panel is an explicit layout change after a reset.
        self.is_layout_reset = false;

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
        self.save_preferences_background();
        cx.notify();
    }

    fn render_transfers_button(
        &self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        pointer_button(id)
            .ghost()
            .icon(IconName::ChevronsUpDown)
            .label(t!("transfers").to_string())
            .tooltip(t!("transfers").to_string())
            .on_click(cx.listener(|this, _, window, cx| {
                this.show_transfers_dialog(window, cx);
            }))
    }

    fn render_command_history_popover(
        &self,
        popover_id: &'static str,
        trigger_id: &'static str,
        anchor: Anchor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Popover::new(popover_id)
            .anchor(anchor)
            .open(self.show_command_history)
            .on_open_change(cx.listener(|this, open, _, cx| {
                let open = *open && matches!(this.active_kind(), Some(TabKind::Ssh));
                if this.show_command_history != open {
                    this.show_command_history = open;
                    if !open {
                        this.selected_command_history.clear();
                    }
                    cx.notify();
                }
            }))
            .trigger(
                pointer_button(trigger_id)
                    .ghost()
                    .icon(IconName::Menu)
                    .label(t!("command_history_short").to_string())
                    .tooltip(t!("command_history").to_string()),
            )
            .w(px(480.))
            .p_0()
            .child(self.render_command_history_popover_content(cx))
    }

    fn render_terminal_encoding_button(
        &self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active_terminal_encoding = self.active_tab.as_ref().and_then(|active_id| {
            self.tabs
                .iter()
                .find(|tab| tab.id == *active_id && tab.kind == TabKind::Ssh)
                .map(|tab| (tab.id.clone(), tab.text_encoding()))
        });

        h_flex().when_some(active_terminal_encoding, |this, (tab_id, encoding)| {
            this.child(
                pointer_button(id)
                    .ghost()
                    .icon(IconName::Globe)
                    .label(encoding.label())
                    .tooltip(t!("terminal_encoding").to_string())
                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                        let view = cx.entity();
                        move |menu, window, _| {
                            TERMINAL_ENCODINGS.iter().copied().fold(
                                menu.min_w(0.),
                                |menu, candidate| {
                                    let tab_id = tab_id.clone();
                                    menu.item(
                                        PopupMenuItem::new(candidate.label())
                                            .checked(candidate == encoding)
                                            .on_click(window.listener_for(
                                                &view,
                                                move |this, _, _, cx| {
                                                    this.set_terminal_encoding(
                                                        tab_id.clone(),
                                                        candidate,
                                                        cx,
                                                    );
                                                },
                                            )),
                                    )
                                },
                            )
                        }
                    }),
            )
        })
    }

    fn render_sftp_minimized_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .flex_none()
            .w_full()
            .min_w(px(0.))
            .overflow_hidden()
            .h(px(24.))
            .px_3()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tab_bar)
            .child(div().flex_1())
            .child(self.render_transfers_button("sftp-transfers-minimized", cx))
            .child(self.render_command_history_popover(
                "sftp-command-history-minimized-popover",
                "sftp-command-history-minimized",
                Anchor::BottomRight,
                cx,
            ))
            .child(self.render_terminal_encoding_button("sftp-terminal-encoding-minimized", cx))
            .child(
                pointer_button("sftp-minimize-toggle")
                    .ghost()
                    .icon(IconName::ChevronUp)
                    .label(t!("panel_expand_short").to_string())
                    .tooltip(t!("panel_expand").to_string())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_sftp_minimized(window, cx);
                    })),
            )
    }

    fn render_sftp_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active_sftp = self.active_sftp();

        let show_hidden_files = self.show_hidden_files;
        let header_view = cx.entity();
        let header = h_flex()
            .flex_none()
            .w_full()
            .min_w(px(0.))
            .overflow_hidden()
            .h(px(34.))
            .px_2()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tab_bar)
            .child(div().flex_1().min_w(px(0.)))
            .when(!self.sftp_panel_minimized, |this| {
                this.child(self.render_transfers_button("sftp-transfers-header", cx))
                    .child(self.render_command_history_popover(
                        "sftp-command-history-popover",
                        "sftp-command-history",
                        Anchor::TopRight,
                        cx,
                    ))
                    .child(self.render_terminal_encoding_button("sftp-terminal-encoding", cx))
            })
            .when(active_sftp.is_some(), |this| {
                this.child(
                    pointer_button("sftp-header-more")
                        .ghost()
                        .icon(IconName::Ellipsis)
                        .label(t!("sftp_more_actions_short").to_string())
                        .tooltip(t!("sftp_more_actions").to_string())
                        .dropdown_menu_with_anchor(Anchor::BottomRight, {
                            let view = header_view.clone();
                            move |menu, window, _| {
                                menu.min_w(0.)
                                    .item(
                                        PopupMenuItem::new(
                                            t!("reveal_current_directory").to_string(),
                                        )
                                        .on_click(
                                            window.listener_for(&view, |this, _, _, cx| {
                                                this.reveal_current_sftp_directory(cx);
                                            }),
                                        ),
                                    )
                                    .item(
                                        PopupMenuItem::new(
                                            t!("collapse_all_directories").to_string(),
                                        )
                                        .on_click(
                                            window.listener_for(&view, |this, _, _, cx| {
                                                this.collapse_sftp_tree(cx);
                                            }),
                                        ),
                                    )
                                    .separator()
                                    .item(
                                        PopupMenuItem::new(t!("hidden").to_string())
                                            .checked(show_hidden_files)
                                            .on_click(window.listener_for(
                                                &view,
                                                move |this, _, _, cx| {
                                                    this.show_hidden_files = !show_hidden_files;
                                                    this.config
                                                        .set_show_hidden_files(!show_hidden_files);
                                                    this.save_preferences_background();
                                                    cx.notify();
                                                },
                                            )),
                                    )
                            }
                        }),
                )
            })
            .child(
                pointer_button("sftp-minimize-toggle-header")
                    .ghost()
                    .icon(IconName::ChevronDown)
                    .label(t!("panel_minimize_short").to_string())
                    .tooltip(t!("panel_minimize").to_string())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_sftp_minimized(window, cx);
                    })),
            );
        let Some(sftp) = active_sftp else {
            let outer = v_flex()
                .gap_0()
                .min_w(px(0.))
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .flex_1()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
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
                                        .text_size(ui_rems(1.0))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("open_ssh_tab_sftp")),
                                ),
                        ),
                )
                .when(self.sftp_panel_minimized, |this| {
                    this.child(self.render_sftp_minimized_bar(cx))
                });
            return outer.into_any_element();
        };

        let selected_path = sftp.selected_path.clone();
        let entries = sftp
            .current_entries()
            .iter()
            .filter(|entry| self.show_hidden_files || !entry.name.starts_with('.'))
            .cloned()
            .collect::<Vec<_>>();
        let tree_rows = sftp.tree_rows(self.show_hidden_files);
        let current_path = sftp.current_path.clone();
        let current_loading = sftp.loading_directories.contains(&current_path);
        let current_error = sftp.directory_errors.get(&current_path).cloned();
        let selected_entries = sftp.selected_entries.clone();
        let all_selected = !entries.is_empty()
            && entries
                .iter()
                .all(|e| selected_entries.contains(&e.full_path));
        let parent_path = Self::sftp_parent_path(&sftp.current_path);
        let view = cx.entity();
        let icon_col_width = px(16.);
        let right_panel_width = self
            .sftp_tree_panels
            .read(cx)
            .sizes()
            .get(1)
            .map(|size| size.as_f32())
            .or_else(|| {
                self.config
                    .sftp_tree_panels()
                    .and_then(|sizes| sizes.get(1).copied())
            })
            .unwrap_or(800.);
        let estimated_file_columns_viewport_width = px((right_panel_width - 76.).max(1.));
        let file_columns_viewport_width = self
            .remote_files_columns_viewport_width
            .unwrap_or(estimated_file_columns_viewport_width);
        let default_size_col_width = 64.;
        let default_modified_col_width = 128.;
        let default_name_col_width = (file_columns_viewport_width.as_f32()
            - default_size_col_width
            - default_modified_col_width)
            .max(96.);
        let file_column_sizes = self.sftp_file_columns.read(cx).sizes().clone();
        let configured_file_column_sizes = if self.config.sftp_file_columns_customized() {
            self.config.sftp_file_columns().cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        let column_width = |index: usize, default: f32, min: f32| {
            let width = file_column_sizes
                .get(index)
                .map(|size| size.as_f32())
                .or_else(|| configured_file_column_sizes.get(index).copied())
                .unwrap_or(default)
                .max(min);
            px(width)
        };
        let name_col_width = column_width(0, default_name_col_width, 96.);
        let size_col_width = column_width(1, default_size_col_width, 56.);
        let modified_col_width = column_width(2, default_modified_col_width, 112.);
        let show_size_column = true;
        let show_modified_column = true;
        let configured_file_columns_width = name_col_width + size_col_width + modified_col_width;
        let file_columns_overflow =
            configured_file_columns_width > file_columns_viewport_width + px(1.);
        let show_file_columns_scrollbar = file_columns_overflow && !entries.is_empty();
        let file_columns_content_width = configured_file_columns_width;
        let file_columns_scroll_handle = self.remote_files_horizontal_scroll_handle.clone();
        let tree_panel_width = self
            .config
            .sftp_tree_panels()
            .and_then(|sizes| sizes.first().copied())
            .unwrap_or(220.)
            .clamp(160., 720.);
        let list_state_message = if current_loading {
            t!("loading_directory").to_string()
        } else if let Some(reason) = current_error {
            t!("directory_load_failed", reason = reason).to_string()
        } else {
            t!("empty_directory").to_string()
        };
        let column_resize_view = view.clone();
        let column_viewport_view = view.clone();
        let file_columns_header = h_resizable("sftp-file-columns")
            .independent_resize()
            .lock(self.config.lock_layout())
            .with_state(&self.sftp_file_columns)
            .on_resize(move |state, _, cx| {
                let column_sizes = state
                    .read(cx)
                    .sizes()
                    .iter()
                    .take(3)
                    .map(|size| size.as_f32())
                    .collect::<Vec<_>>();
                column_resize_view.update(cx, move |this, _| {
                    this.is_layout_reset = false;
                    this.config.set_sftp_file_columns(Some(column_sizes));
                    this.config.set_sftp_file_columns_customized(true);
                    this.save_preferences_background();
                });
            })
            .child(
                resizable_panel()
                    .size(name_col_width)
                    .size_range(px(96.)..Pixels::MAX)
                    .child(
                        h_flex()
                            .size_full()
                            .min_w(px(0.))
                            .items_center()
                            .gap_2()
                            .pr_2()
                            .child(div().w(icon_col_width).flex_none())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(ui_rems(0.917))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("name")),
                            ),
                    ),
            )
            .when(show_size_column, |this| {
                this.child(
                    resizable_panel()
                        .size(size_col_width)
                        .size_range(px(56.)..Pixels::MAX)
                        .flex_none()
                        .child(
                            h_flex()
                                .size_full()
                                .items_center()
                                .px_2()
                                .text_size(ui_rems(0.917))
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("size")),
                        ),
                )
            })
            .when(show_modified_column, |this| {
                this.child(
                    resizable_panel()
                        .size(modified_col_width)
                        .size_range(px(112.)..Pixels::MAX)
                        .flex_none()
                        .child(
                            h_flex()
                                .size_full()
                                .items_center()
                                .px_2()
                                .text_size(ui_rems(0.917))
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("modified")),
                        ),
                )
            });

        let mut outer = v_flex()
            .gap_0()
            .min_w(px(0.))
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
                .min_w(px(0.))
                .min_h(px(0.))
                .when(self.sftp_panel_minimized, |this| this.hidden())
                .child(header)
                .child(
                    h_flex()
                        .w_full()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .h(px(36.))
                        .items_center()
                        .gap_2()
                        .px_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted)
                        .child(
                            pointer_button("sftp-up")
                                .ghost()

                                .icon(IconName::ChevronUp)
                                .tooltip(t!("parent_directory").to_string())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.navigate_sftp(parent_path.clone(), cx);
                                })),
                        )
                        .child(
                            Input::new(&self.sftp_path_input)
                                .flex_1()
                                .min_w(px(0.))
                                .tab_index(0),
                        )
                        .child(
                            pointer_button("sftp-sync-cwd")
                                .ghost()

                                .icon(IconName::Replace)
                                .label(t!("sync_cwd").to_string())
                                .tooltip(t!("sync_cwd_tooltip").to_string())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.sync_cwd_from_terminal(window, cx);
                                })),
                        )
                        .child(
                            pointer_button("sftp-refresh")
                                .ghost()

                                .icon(IconName::Redo)
                                .label(t!("refresh").to_string())
                                .tooltip(t!("refresh").to_string())
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_sftp(cx))),
                        )
                        .child(
                            pointer_button("sftp-create-upload")
                                .ghost()

                                .icon(IconName::Plus)
                                .label(t!("add").to_string())
                                .tooltip(t!("create_or_upload").to_string())
                                .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                    let view = view.clone();
                                    move |menu, window, _| {
                                        menu.min_w(0.)
                                            .item(
                                                PopupMenuItem::new(t!("new_folder").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, window, cx| {
                                                            this.sftp_creating_folder = true;
                                                            this.sftp_new_folder_input.update(
                                                                cx,
                                                                |input, cx| {
                                                                    input.set_value(
                                                                        "", window, cx,
                                                                    );
                                                                    input
                                                                        .focus_handle(cx)
                                                                        .focus(window, cx);
                                                                },
                                                            );
                                                            cx.notify();
                                                        },
                                                    )),
                                            )
                                            .item(
                                                PopupMenuItem::new(t!("upload_file").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, window, cx| {
                                                            this.upload_sftp_files(window, cx);
                                                        },
                                                    )),
                                            )
                                            .item(
                                                PopupMenuItem::new(
                                                    t!("upload_folder").to_string(),
                                                )
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.upload_sftp_folder(window, cx);
                                                    },
                                                )),
                                            )
                                    }
                                }),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .min_h(px(0.))
                        .overflow_hidden()
                        .child(
                            h_resizable("sftp-browser")
                        .lock(self.config.lock_layout())
                        .with_state(&self.sftp_tree_panels)
                        .on_resize({
                            let view = view.clone();
                            move |_, _, cx| {
                                view.update(cx, |this, _| {
                                    this.is_layout_reset = false;
                                });
                            }
                        })
                        .child(
                            resizable_panel()
                                .size(px(tree_panel_width))
                                .size_range(px(160.)..px(720.))
                                .flex_none()
                                .child(
                                    v_flex()
                                        .size_full()
                                        .border_r_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            h_flex()
                                                .flex_none()
                                                .h(px(26.))
                                                .px_2()
                                                .items_center()
                                                .border_b_1()
                                                .border_color(cx.theme().border)
                                                .bg(cx.theme().muted.opacity(0.8))
                                                .text_size(ui_rems(0.917))
                                                .text_color(cx.theme().muted_foreground)
                                                .child(t!("directories")),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .relative()
                                                .min_h(px(0.))
                                                .child({
                                                    let rows = tree_rows.clone();
                                                    let selected_directory = current_path.clone();
                                                    let view = view.clone();
                                                    let theme = cx.theme().clone();
                                                    uniform_list(
                                                        "sftp-tree-list",
                                                        rows.len(),
                                                        move |range, window, _| {
                                                            range
                                                                .into_iter()
                                                                .filter_map(|ix| {
                                                                    let row = rows.get(ix)?.clone();
                                                                    let selected =
                                                                        row.path == selected_directory;
                                                                    let toggle_tooltip = if let Some(
                                                                        reason,
                                                                    ) = row.error.as_ref()
                                                                    {
                                                                        format!(
                                                                            "{}: {}",
                                                                            t!("retry_directory"),
                                                                            reason
                                                                        )
                                                                    } else if row.expanded {
                                                                        t!("collapse_directory")
                                                                            .to_string()
                                                                    } else {
                                                                        t!("expand_directory")
                                                                            .to_string()
                                                                    };
                                                                    let folder_icon = if row.expanded {
                                                                        IconName::FolderOpen
                                                                    } else {
                                                                        IconName::FolderClosed
                                                                    };
                                                                    let row_path = row.path.clone();
                                                                    Some(
                                                                        h_flex()
                                                                            .w_full()
                                                                            .h(px(28.))
                                                                            .items_center()
                                                                            .pl(px(
                                                                                2. + row.depth as f32
                                                                                    * 14.,
                                                                            ))
                                                                            .pr_1()
                                                                            .bg(if selected {
                                                                                theme.secondary
                                                                            } else {
                                                                                theme.background
                                                                            })
                                                                            .hover(|style| {
                                                                                style.bg(
                                                                                    theme
                                                                                        .muted
                                                                                        .opacity(0.8),
                                                                                )
                                                                            })
                                                                            .child(
                                                                                h_flex()
                                                                                    .size_5()
                                                                                    .flex_none()
                                                                                    .items_center()
                                                                                    .justify_center()
                                                                                    .child(
                                                                                        pointer_button((
                                                                                            "sftp-tree-toggle",
                                                                                            ix,
                                                                                        ))
                                                                                        .ghost()
                                                                                        .small()
                                                                                        .icon(if row.expanded {
                                                                                            IconName::ChevronDown
                                                                                        } else {
                                                                                            IconName::ChevronRight
                                                                                        })
                                                                                        .loading(row.loading)
                                                                                        .tooltip(toggle_tooltip)
                                                                                        .on_click(
                                                                                            window.listener_for(
                                                                                                &view,
                                                                                                {
                                                                                                    let path =
                                                                                                        row.path
                                                                                                            .clone();
                                                                                                    move |this,
                                                                                                          _,
                                                                                                          _,
                                                                                                          cx| {
                                                                                                        this.toggle_sftp_tree_directory(
                                                                                                            path.clone(),
                                                                                                            cx,
                                                                                                        );
                                                                                                    }
                                                                                                },
                                                                                            )
                                                                                        ),
                                                                                    ),
                                                                            )
                                                                            .child(
                                                                                h_flex()
                                                                                    .id((
                                                                                        "sftp-tree-open",
                                                                                        ix,
                                                                                    ))
                                                                                    .flex_1()
                                                                                    .min_w(px(0.))
                                                                                    .items_center()
                                                                                    .gap_1()
                                                                                    .cursor_pointer()
                                                                                    .tooltip({
                                                                                        let path =
                                                                                            row_path.clone();
                                                                                        move |window, cx| {
                                                                                            gpui_component::tooltip::Tooltip::new(
                                                                                                path.clone(),
                                                                                            )
                                                                                            .build(window, cx)
                                                                                        }
                                                                                    })
                                                                                    .on_mouse_down(
                                                                                        MouseButton::Left,
                                                                                        window.listener_for(
                                                                                            &view,
                                                                                            move |this,
                                                                                                  _,
                                                                                                  _,
                                                                                                  cx| {
                                                                                                this.navigate_sftp(
                                                                                                    row_path.clone(),
                                                                                                    cx,
                                                                                                );
                                                                                            },
                                                                                        ),
                                                                                    )
                                                                                    .child(
                                                                                        Icon::new(folder_icon)
                                                                                            .with_size(
                                                                                                Size::Small,
                                                                                            )
                                                                                            .text_color(
                                                                                                theme.primary,
                                                                                            ),
                                                                                    )
                                                                                    .child(
                                                                                        div()
                                                                                            .flex_1()
                                                                                            .min_w(px(0.))
                                                                                            .truncate()
                                                                                            .text_size(
                                                                                                ui_rems(0.917),
                                                                                            )
                                                                                            .child(row.label),
                                                                                    )
                                                                            )
                                                                            .into_any_element(),
                                                                    )
                                                                })
                                                                .collect::<Vec<_>>()
                                                        },
                                                    )
                                                    .size_full()
                                                    .track_scroll(
                                                        &self.remote_tree_scroll_handle,
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .absolute()
                                                        .top_0()
                                                        .right_0()
                                                        .bottom_0()
                                                        .w(px(12.))
                                                        .child(
                                                            Scrollbar::vertical(
                                                                &self.remote_tree_scroll_handle,
                                                            )
                                                            .scrollbar_show(
                                                                ScrollbarShow::Scrolling,
                                                            ),
                                                        ),
                                                ),
                                        ),
                                ),
                        )
                        .child(
                            resizable_panel()
                                .size_range(px(360.)..px(2400.))
                                .child(
                                    v_flex()
                                        .size_full()
                                        .when(self.sftp_creating_folder, |this| {
                                            this.child(
                                                h_flex()
                                                    .flex_none()
                                                    .h(px(32.))
                                                    .px_2()
                                                    .items_center()
                                                    .gap_2()
                                                    .border_b_1()
                                                    .border_color(cx.theme().border)
                                                    .child(
                                                        Icon::new(IconName::Folder)
                                                            .with_size(Size::Small)
                                                            .text_color(cx.theme().primary),
                                                    )
                                                    .child(
                                                        Input::new(&self.sftp_new_folder_input)
                                                            .flex_1()
                                                            .tab_index(0),
                                                    )
                                                    .child(
                                                        pointer_button("cancel-sftp-new-folder")
                                                            .ghost()
                                                            .small()
                                                            .icon(IconName::Close)
                                                            .tooltip(t!("cancel").to_string())
                                                            .on_click(cx.listener(
                                                                |this, _, _, cx| {
                                                                    this.sftp_creating_folder =
                                                                        false;
                                                                    cx.notify();
                                                                },
                                                            )),
                                                    ),
                                            )
                                        })
                                        .child(if selected_entries.is_empty() {
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
                                                            pointer_checkbox("sftp-select-all")
                                                                .checked(all_selected)
                                                                .on_click(cx.listener(
                                                                    |this, checked, _, cx| {
                                                                        this.toggle_all_sftp_entries(
                                                                            *checked, cx,
                                                                        );
                                                                    },
                                                                )),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .id("sftp-file-columns-header-scroll")
                                                        .flex()
                                                        .flex_1()
                                                        .min_w(px(0.))
                                                        .h_full()
                                                        .on_prepaint(move |bounds, _, cx| {
                                                            let width = bounds.size.width;
                                                            column_viewport_view.update(
                                                                cx,
                                                                |this, cx| {
                                                                    let width_changed = this
                                                                        .remote_files_columns_viewport_width
                                                                        .map(|current| {
                                                                            (current.as_f32()
                                                                                - width.as_f32())
                                                                            .abs()
                                                                                > 1.
                                                                        })
                                                                        .unwrap_or(true);
                                                                    if !width_changed {
                                                                        return;
                                                                    }

                                                                    this.remote_files_columns_viewport_width =
                                                                        Some(width);
                                                                    if !this
                                                                        .config
                                                                        .sftp_file_columns_customized()
                                                                    {
                                                                        this.sftp_file_columns = cx.new(|_| {
                                                                            crate::app::resizable::ResizableState::default()
                                                                        });
                                                                    }
                                                                    cx.notify();
                                                                },
                                                            );
                                                        })
                                                        .track_scroll(&file_columns_scroll_handle)
                                                        .overflow_x_scroll()
                                                        .map(|mut scrollable| {
                                                            scrollable
                                                                .style()
                                                                .restrict_scroll_to_axis =
                                                                Some(true);
                                                            scrollable
                                                        })
                                                        .child(
                                                            div()
                                                                .w(file_columns_content_width)
                                                                .min_w(file_columns_content_width)
                                                                .max_w(file_columns_content_width)
                                                                .h_full()
                                                                .flex_none()
                                                                .child(file_columns_header),
                                                        ),
                                                )
                                                .child(div().w(px(12.)).flex_none())
                                                .into_any_element()
                                        } else {
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
                                                            pointer_checkbox("sftp-select-all")
                                                                .checked(all_selected)
                                                                .on_click(cx.listener(
                                                                    |this, checked, _, cx| {
                                                                        this.toggle_all_sftp_entries(
                                                                            *checked, cx,
                                                                        );
                                                                    },
                                                                )),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w(px(0.))
                                                        .truncate()
                                                        .text_size(ui_rems(0.917))
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(
                                                            t!(
                                                                "selected_items",
                                                                count = selected_entries.len()
                                                            )
                                                            .to_string(),
                                                        ),
                                                )
                                                .child(
                                                    pointer_button("sftp-download-selected")
                                                        .ghost()
                                                        .small()
                                                        .icon(IconName::ArrowDown)
                                                        .label(t!("download").to_string())
                                                        .tooltip(
                                                            t!(
                                                                "download_count",
                                                                count = selected_entries.len()
                                                            )
                                                            .to_string(),
                                                        )
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.download_selected_sftp_entries(
                                                                    window, cx,
                                                                );
                                                            },
                                                        )),
                                                )
                                                .child(
                                                    pointer_button("sftp-delete-selected")
                                                        .danger()
                                                        .small()
                                                        .icon(IconName::Delete)
                                                        .label(t!("delete_selected").to_string())
                                                        .tooltip(t!("delete_selected").to_string())
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.show_delete_confirm_dialog(
                                                                    window, cx,
                                                                );
                                                            },
                                                        )),
                                                )
                                                .into_any_element()
                                        })
                                        .child(
                                            div()
                                                .flex_1()
                                                .relative()
                                                .min_h(px(0.))
                                                .child({
                                                    let entries = entries.clone();
                                                    let selected_entries =
                                                        selected_entries.clone();
                                                    let selected_path = selected_path.clone();
                                                    let view = view.clone();
                                                    let theme = cx.theme().clone();
                                                    let visible_entry_paths: Arc<[String]> = entries
                                                        .iter()
                                                        .map(|entry| entry.full_path.clone())
                                                        .collect::<Vec<_>>()
                                                        .into();
                                                    let horizontal_scroll_handle =
                                                        file_columns_scroll_handle.clone();
                                                    uniform_list(
                                                        "sftp-entries-list",
                                                        entries.len(),
                                                        move |range, window, _| {
                                                            range
                                                                .into_iter()
                                                                .filter_map(|ix| {
                                                                    let entry =
                                                                        entries.get(ix)?.clone();
                                                                    let modified_text =
                                                                        format_mtime(entry.modified);
                                                                    let is_checked =
                                                                        selected_entries.contains(
                                                                            &entry.full_path,
                                                                        );
                                                                    let is_selected = is_checked
                                                                        || selected_path.as_deref()
                                                                            == Some(
                                                                                entry
                                                                                    .full_path
                                                                                    .as_str(),
                                                                            );
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
                                                                            .hover(|style| {
                                                                                style.bg(
                                                                                    theme
                                                                                        .muted
                                                                                        .opacity(0.8),
                                                                                )
                                                                            })
                                                                            .border_b_1()
                                                                            .border_color(
                                                                                theme
                                                                                    .border
                                                                                    .opacity(0.35),
                                                                            )
                                                                            .on_mouse_down(
                                                                                MouseButton::Left,
                                                                                window.listener_for(
                                                                                    &view,
                                                                                    {
                                                                                        let entry =
                                                                                            entry
                                                                                                .clone();
                                                                                        let visible_entry_paths =
                                                                                            visible_entry_paths
                                                                                                .clone();
                                                                                        move |this,
                                                                                              event: &MouseDownEvent,
                                                                                              _,
                                                                                              cx| {
                                                                                            this.dismiss_sftp_context_menu(cx);
                                                                                            this.select_sftp_entry(
                                                                                                entry
                                                                                                    .clone(),
                                                                                                &visible_entry_paths,
                                                                                                event
                                                                                                    .modifiers
                                                                                                    .shift,
                                                                                                cx,
                                                                                            );
                                                                                        }
                                                                                    },
                                                                                ),
                                                                            )
                                                                            .on_mouse_down(
                                                                                MouseButton::Right,
                                                                                window.listener_for(
                                                                                    &view,
                                                                                    {
                                                                                        let entry =
                                                                                            entry
                                                                                                .clone();
                                                                                        let remote_path =
                                                                                            entry
                                                                                                .full_path
                                                                                                .clone();
                                                                                        move |this,
                                                                                              event: &MouseDownEvent,
                                                                                              _,
                                                                                              cx| {
                                                                                            this.mark_sftp_entry_selected(
                                                                                                &entry
                                                                                                    .full_path,
                                                                                                cx,
                                                                                            );
                                                                                            this.open_sftp_context_menu(
                                                                                                remote_path
                                                                                                    .clone(),
                                                                                                entry
                                                                                                    .is_dir,
                                                                                                event
                                                                                                    .position,
                                                                                                cx,
                                                                                            );
                                                                                        }
                                                                                    },
                                                                                ),
                                                                            )
                                                                            .child(
                                                                                h_flex()
                                                                                    .w(px(24.))
                                                                                    .flex_none()
                                                                                    .items_center()
                                                                                    .justify_center()
                                                                                    .on_mouse_down(
                                                                                        MouseButton::Left,
                                                                                        |_, _, cx| {
                                                                                            cx.stop_propagation()
                                                                                        },
                                                                                    )
                                                                                    .on_mouse_down(
                                                                                        MouseButton::Right,
                                                                                        |_, _, cx| {
                                                                                            cx.stop_propagation()
                                                                                        },
                                                                                    )
                                                                                    .child(
                                                                                        pointer_checkbox(
                                                                                            ElementId::Name(
                                                                                                format!(
                                                                                                    "check-{}",
                                                                                                    entry
                                                                                                        .full_path
                                                                                                )
                                                                                                .into(),
                                                                                            ),
                                                                                        )
                                                                                        .checked(
                                                                                            is_checked,
                                                                                        )
                                                                                        .on_click(
                                                                                            window.listener_for(
                                                                                                &view,
                                                                                                {
                                                                                                    let path = entry
                                                                                                        .full_path
                                                                                                        .clone();
                                                                                                    move |this,
                                                                                                          checked,
                                                                                                          _,
                                                                                                          cx| {
                                                                                                        this.toggle_sftp_entry(
                                                                                                            path.clone(),
                                                                                                            *checked,
                                                                                                            cx,
                                                                                                        );
                                                                                                    }
                                                                                                },
                                                                                            ),
                                                                                        ),
                                                                                    ),
                                                                            )
                                                                            .child(
                                                                                div()
                                                                                    .id((
                                                                                        "sftp-file-columns-row-scroll",
                                                                                        ix,
                                                                                    ))
                                                                                    .flex()
                                                                                    .flex_1()
                                                                                    .min_w(px(0.))
                                                                                    .h_full()
                                                                                    .track_scroll(
                                                                                        &horizontal_scroll_handle,
                                                                                    )
                                                                                    .overflow_x_scroll()
                                                                                    .map(|mut scrollable| {
                                                                                        // Preserve vertical wheel input for the outer file list.
                                                                                        scrollable
                                                                                            .style()
                                                                                            .restrict_scroll_to_axis = Some(true);
                                                                                        scrollable
                                                                                    })
                                                                                    .child(
                                                                                        h_flex()
                                                                                            .w(
                                                                                                file_columns_content_width,
                                                                                            )
                                                                                            .min_w(
                                                                                                file_columns_content_width,
                                                                                            )
                                                                                            .max_w(
                                                                                                file_columns_content_width,
                                                                                            )
                                                                                            .h_full()
                                                                                            .flex_none()
                                                                                            .items_center()
                                                                                            .child(
                                                                                                h_flex()
                                                                                            .w(name_col_width)
                                                                                            .flex_none()
                                                                                            .min_w(px(0.))
                                                                                            .items_center()
                                                                                            .gap_2()
                                                                                            .pr_2()
                                                                                            .child(
                                                                                                div()
                                                                                                    .w(
                                                                                                        icon_col_width,
                                                                                                    )
                                                                                                    .flex_none()
                                                                                                    .child(
                                                                                                        Icon::new(
                                                                                                            if entry
                                                                                                                .is_dir
                                                                                                            {
                                                                                                                IconName::FolderClosed
                                                                                                            } else {
                                                                                                                IconName::File
                                                                                                            },
                                                                                                        )
                                                                                                        .with_size(
                                                                                                            Size::Small,
                                                                                                        )
                                                                                                        .text_color(
                                                                                                            if entry
                                                                                                                .is_dir
                                                                                                            {
                                                                                                                theme.primary
                                                                                                            } else {
                                                                                                                theme
                                                                                                                    .muted_foreground
                                                                                                            },
                                                                                                        ),
                                                                                                    ),
                                                                                            )
                                                                                            .child(
                                                                                                div()
                                                                                                    .id(("sftp-file-entry-name", ix))
                                                                                                    .flex_1()
                                                                                                    .min_w(px(0.))
                                                                                                    .truncate()
                                                                                                    .tooltip({
                                                                                                        let path = entry
                                                                                                            .full_path
                                                                                                            .clone();
                                                                                                        move |window,
                                                                                                              cx| {
                                                                                                            gpui_component::tooltip::Tooltip::new(
                                                                                                                path.clone(),
                                                                                                            )
                                                                                                            .build(window, cx)
                                                                                                        }
                                                                                                    })
                                                                                                    .text_size(
                                                                                                        ui_rems(1.0),
                                                                                                    )
                                                                                                    .text_color(
                                                                                                        name_color,
                                                                                                    )
                                                                                                    .child(
                                                                                                        entry
                                                                                                            .name,
                                                                                                    ),
                                                                                            ),
                                                                                            )
                                                                                    .when(
                                                                                        show_size_column,
                                                                                        |this| {
                                                                                    this.child(
                                                                                        h_flex()
                                                                                            .w(
                                                                                                size_col_width,
                                                                                            )
                                                                                            .flex_none()
                                                                                            .h_full()
                                                                                            .items_center()
                                                                                            .px_2()
                                                                                            .text_size(
                                                                                                ui_rems(0.917),
                                                                                            )
                                                                                            .text_color(
                                                                                                theme
                                                                                                    .muted_foreground,
                                                                                            )
                                                                                            .child(
                                                                                                if entry
                                                                                                    .is_dir
                                                                                                {
                                                                                                    "-"
                                                                                                        .to_string()
                                                                                                } else {
                                                                                                    format_bytes(
                                                                                                        entry
                                                                                                            .size,
                                                                                                    )
                                                                                                },
                                                                                            ),
                                                                                    )
                                                                                        },
                                                                                    )
                                                                                    .when(
                                                                                        show_modified_column,
                                                                                        |this| {
                                                                                    this.child(
                                                                                        h_flex()
                                                                                            .w(
                                                                                                modified_col_width,
                                                                                            )
                                                                                            .flex_none()
                                                                                            .h_full()
                                                                                            .items_center()
                                                                                            .px_2()
                                                                                            .text_size(
                                                                                                ui_rems(0.917),
                                                                                            )
                                                                                            .text_color(
                                                                                                theme
                                                                                                    .muted_foreground,
                                                                                            )
                                                                                            .child(
                                                                                                modified_text,
                                                                                            ),
                                                                                    )
                                                                                        },
                                                                                    )
                                                                                    )
                                                                            )
                                                                            .child(
                                                                                div()
                                                                                    .w(px(12.))
                                                                                    .flex_none(),
                                                                            )
                                                                            .into_any_element(),
                                                                    )
                                                                })
                                                                .collect::<Vec<_>>()
                                                        },
                                                    )
                                                    .size_full()
                                                    .map(|mut list| {
                                                        // Keep horizontal gestures available to the file columns.
                                                        list.style().restrict_scroll_to_axis =
                                                            Some(true);
                                                        list
                                                    })
                                                    .when(show_file_columns_scrollbar, |this| {
                                                        this.pb(px(12.))
                                                    })
                                                    .track_scroll(
                                                        &self.remote_files_scroll_handle,
                                                    )
                                                })
                                                .when(entries.is_empty(), |this| {
                                                    this.child(
                                                        div()
                                                            .absolute()
                                                            .inset_0()
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .px_4()
                                                            .text_size(ui_rems(0.917))
                                                            .text_color(
                                                                cx.theme().muted_foreground,
                                                            )
                                                            .child(list_state_message),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .absolute()
                                                        .top_0()
                                                        .right_0()
                                                        .bottom(if show_file_columns_scrollbar {
                                                            px(12.)
                                                        } else {
                                                            px(0.)
                                                        })
                                                        .w(px(16.))
                                                        .child(
                                                            Scrollbar::vertical(
                                                                &self.remote_files_scroll_handle,
                                                            )
                                                            .scrollbar_show(
                                                                ScrollbarShow::Scrolling,
                                                            ),
                                                        ),
                                                )
                                                .when(show_file_columns_scrollbar, |this| {
                                                    this.child(
                                                        div()
                                                            .absolute()
                                                            .left(px(44.))
                                                            .right(px(32.))
                                                            .bottom_0()
                                                            .h(px(12.))
                                                            .bg(cx.theme().background)
                                                            .child(
                                                                Scrollbar::horizontal(
                                                                    &self.remote_files_horizontal_scroll_handle,
                                                                )
                                                                .scroll_size(gpui::size(
                                                                    file_columns_content_width,
                                                                    px(0.),
                                                                ))
                                                                .scrollbar_show(
                                                                    ScrollbarShow::Scrolling,
                                                                ),
                                                            ),
                                                    )
                                                }),
                                        ),
                                ),
                        ),
                ),
            ),
        );
        outer = outer.when(self.sftp_panel_minimized, |this| {
            this.child(self.render_sftp_minimized_bar(cx))
        });

        outer.into_any_element()
    }

    fn render_monitoring_panel(
        &mut self,
        viewport_width: Pixels,
        interactive: bool,
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
            .id("bottom-cpu-module")
            .min_w(card_min_w)
            .flex_1()
            .h_full()
            .px_1()
            .py_1()
            .gap_0p5()
            .when(interactive, |this| {
                this.rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().secondary))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.show_remote_processes_dialog(ServerMonitorView::Cpu, window, cx)
                        }),
                    )
            })
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .child(
                        div()
                            .text_size(ui_rems(0.833))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cpu_color)
                            .child(t!("cpu").to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(ui_rems(0.833))
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
            .id("bottom-memory-module")
            .min_w(card_min_w)
            .flex_1()
            .h_full()
            .px_1()
            .py_1()
            .gap_0p5()
            .when(interactive, |this| {
                this.rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().secondary))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.show_remote_processes_dialog(ServerMonitorView::Memory, window, cx)
                        }),
                    )
            })
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .child(
                        div()
                            .text_size(ui_rems(0.833))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(mem_color)
                            .child(t!("mem").to_string()),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(ui_rems(0.833))
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
                            .text_size(ui_rems(0.7))
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
                                .text_size(ui_rems(0.7))
                                .text_color(muted_fg)
                                .child(swap_detail),
                        ),
                )
            });

        // --- NET card: rx/tx text + dual sparkline ---
        let net_card = if show_net_card {
            Some(
                v_flex()
                    .id("bottom-network-module")
                    .min_w(card_min_w)
                    .flex_1()
                    .h_full()
                    .px_1()
                    .py_1()
                    .gap_0p5()
                    .when(interactive, |this| {
                        this.rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().secondary))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.show_remote_ports_dialog(window, cx)
                                }),
                            )
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .child(
                                div()
                                    .text_size(ui_rems(0.833))
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
                                            .text_size(ui_rems(0.75))
                                            .text_color(net_color)
                                            .child(format!("↓{}", net_rx)),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui_rems(0.75))
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
                                    .text_size(ui_rems(0.833))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(disk_color)
                                    .child(t!("disk").to_string()),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(ui_rems(0.833))
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
                                                    .text_size(ui_rems(0.667))
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
                                                    .text_size(ui_rems(0.667))
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

    fn render_sidebar_monitoring_panel(
        &self,
        interactive: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let cpu_pct = self.system.cpu_percent;
        let mem_pct = self.system.mem_percent;
        let swap_pct = self.system.swap_percent;

        let cpu_color = cx.theme().chart_1;
        let mem_color = cx.theme().chart_2;
        let swap_color = cx.theme().chart_3;
        let disk_color = cx.theme().chart_5;
        let net_color = cx.theme().chart_4;
        let muted_fg = cx.theme().muted_foreground;
        let active_is_ssh = matches!(self.active_kind(), Some(TabKind::Ssh));
        let (monitor_title, monitor_detail) = self
            .active_tab
            .as_ref()
            .and_then(|active_id| self.tabs.iter().find(|tab| tab.id == *active_id))
            .and_then(|tab| tab.session.as_ref())
            .map(|session| (session.name.clone(), self.session_detail(session)))
            .unwrap_or_else(|| (t!("system_info").to_string(), t!("live").to_string()));
        let status_color = if interactive {
            cx.theme().success
        } else if active_is_ssh {
            cx.theme().danger
        } else {
            muted_fg
        };

        v_flex()
            .gap_3()
            .w_full()
            .p_2()
            .child(
                h_flex()
                    .w_full()
                    .min_w(px(0.))
                    .gap_2()
                    .pb_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .items_center()
                    .child(div().size(px(7.)).rounded_full().bg(status_color))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(px(0.))
                            .gap_0p5()
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(ui_rems(0.8))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(monitor_title),
                            )
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_size(ui_rems(0.7))
                                    .text_color(muted_fg)
                                    .child(monitor_detail),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui_rems(0.7))
                            .text_color(muted_fg)
                            .child(t!("system_info")),
                    ),
            )
            .child(
                v_flex()
                    .id("sidebar-cpu-module")
                    .gap_1()
                    .when(interactive, |this| {
                        this.rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().secondary))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.show_remote_processes_dialog(
                                        ServerMonitorView::Cpu,
                                        window,
                                        cx,
                                    )
                                }),
                            )
                    })
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(ui_rems(0.85))
                                    .text_color(cpu_color)
                                    .child(t!("cpu").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(ui_rems(0.85))
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
                    .id("sidebar-memory-module")
                    .gap_1()
                    .when(interactive, |this| {
                        this.rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().secondary))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.show_remote_processes_dialog(
                                        ServerMonitorView::Memory,
                                        window,
                                        cx,
                                    )
                                }),
                            )
                    })
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(ui_rems(0.85))
                                    .text_color(mem_color)
                                    .child(t!("mem").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(ui_rems(0.85))
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
                                    .text_size(ui_rems(0.85))
                                    .text_color(swap_color)
                                    .child(t!("swap").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(ui_rems(0.85))
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
                                    .text_size(ui_rems(0.85))
                                    .text_color(disk_color)
                                    .child(t!("disk").to_string()),
                            )
                            .children(if self.system.disks.len() > 3 {
                                Some(
                                    div()
                                        .text_size(ui_rems(0.65))
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
                                                            .text_size(ui_rems(0.75))
                                                            .text_color(muted_fg)
                                                            .child(mount_short),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(ui_rems(0.75))
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
                    .id("sidebar-network-module")
                    .gap_1()
                    .when(interactive, |this| {
                        this.rounded_md()
                            .cursor_pointer()
                            .hover(|style| style.bg(cx.theme().secondary))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.show_remote_ports_dialog(window, cx)
                                }),
                            )
                    })
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(ui_rems(0.85))
                                    .text_color(net_color)
                                    .child(t!("net").to_string()),
                            )
                            .child(
                                div()
                                    .text_size(ui_rems(0.85))
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
                                            .text_size(ui_rems(0.75))
                                            .text_color(net_color)
                                            .child("↓"),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui_rems(0.75))
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
                                            .text_size(ui_rems(0.75))
                                            .text_color(cx.theme().chart_5)
                                            .child("↑"),
                                    )
                                    .child(
                                        div()
                                            .text_size(ui_rems(0.75))
                                            .child(self.system.net_tx.clone()),
                                    ),
                            ),
                    ),
            )
    }

    pub(crate) fn render_remote_process_list(
        &self,
        view_mode: ServerMonitorView,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let process_filter = self
            .remote_process_filter_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let has_process_data = !self.remote_processes.is_empty();
        let processes = self
            .remote_processes
            .iter()
            .filter(|process| process_matches_filter(process, &process_filter))
            .cloned()
            .collect::<Vec<_>>();
        let terminating = self.terminating_processes.clone();
        let monitored_tab_id = self.system_tab_id.clone().unwrap_or_default();
        let status = self.remote_process_status.clone();
        let processes_loading = self.remote_processes_in_flight;
        let expanded_pid = self.expanded_process_pid;
        let empty_message = if has_process_data {
            t!("no_matching_processes").to_string()
        } else {
            t!("no_processes").to_string()
        };
        // Use one fixed grid for the header and every process row so the
        // selected metric stays aligned with its values below.
        let metric_column_width = px(84.);
        let action_column_width = px(28.);
        let metric_label = if view_mode == ServerMonitorView::Cpu {
            t!("cpu_percent").to_string()
        } else {
            t!("memory_usage_short").to_string()
        };
        let theme = cx.theme().colors;

        v_flex()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .h(px(28.))
                    .flex_none()
                    .items_center()
                    .px_2()
                    .pr(px(24.))
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(ui_rems(0.75))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("process").to_string()),
                    )
                    .child(
                        div()
                            .w(metric_column_width)
                            .flex_none()
                            .text_size(ui_rems(0.75))
                            .text_right()
                            .text_color(cx.theme().muted_foreground)
                            .child(metric_label),
                    )
                    .child(
                        div().w(action_column_width).flex_none(),
                    )
                    .child(
                        div()
                            .w(action_column_width)
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                pointer_button("refresh-processes")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Redo)
                                    .tooltip(t!("refresh_processes").to_string())
                                    .disabled(self.remote_processes_in_flight)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.request_active_process_snapshot();
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_none()
                    .items_center()
                    .px_2()
                    .child(
                        Input::new(&self.remote_process_filter_input)
                            .flex_1()
                            .min_w(px(0.)),
                    ),
            )
            .children(status.map(|status| {
                div()
                    .w_full()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .truncate()
                    .text_size(ui_rems(0.75))
                    .text_color(cx.theme().muted_foreground)
                    .child(status)
            }))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .when(processes.is_empty() && !processes_loading, |this| {
                        this.flex().items_center().justify_center().child(
                            div()
                                .text_size(ui_rems(0.833))
                                .text_color(theme.muted_foreground)
                                .child(empty_message.clone()),
                        )
                    })
                    .when(!processes.is_empty(), |this| {
                        this.child(
                            div()
                                .relative()
                                .size_full()
                                .child(
                                    v_flex()
                                        .id("remote-process-scroll")
                                        .track_scroll(&self.process_scroll_handle)
                                        .overflow_y_scroll()
                                        .size_full()
                                        .pr(px(TERMINAL_SCROLLBAR_GUTTER))
                                        .children(
                                            processes.iter().enumerate().map(|(index, process)| {
                                                let pid = process.pid;
                                                let expanded = expanded_pid == Some(pid);
                                                let is_terminating = terminating.contains(&pid);
                                                let metric = if view_mode == ServerMonitorView::Cpu {
                                                    format!("{:.1}%", process.cpu_percent)
                                                } else {
                                                    format_bytes(process.memory_bytes)
                                                };
                                                let command = process.command.clone();
                                                let copy_payload = format!(
                                                    "{}: {pid}\n{}: {}\n{}: {:.2}%\n{}: {}\n{}: {}",
                                                    t!("process_pid"),
                                                    t!("user"),
                                                    process.user,
                                                    t!("cpu"),
                                                    process.cpu_percent,
                                                    t!("memory"),
                                                    format_bytes(process.memory_bytes),
                                                    t!("process_command"),
                                                    process.command
                                                );
                                                let process_for_dialog = process.clone();
                                                let tab_id = monitored_tab_id.clone();
                                                let row_theme = theme;
                                                v_flex()
                                                    .w_full()
                                                    .min_w(px(0.))
                                                    .max_w_full()
                                                    .px_2()
                                                    .py_1()
                                                    .gap_1()
                                                    .border_b_1()
                                                    .border_color(row_theme.border.opacity(0.5))
                                                    .when(index % 2 == 1, |this| {
                                                        this.bg(row_theme.muted.opacity(0.35))
                                                    })
                                                    .when(expanded, |this| {
                                                        this.bg(row_theme.secondary.opacity(0.45))
                                                    })
                                                    .id(("remote-process-row", pid as usize))
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.toggle_process_expanded(pid, cx);
                                                    }))
                                                    .child(
                                                        h_flex()
                                                            .w_full()
                                                            .items_start()
                                                            .gap_2()
                                                            .child(
                                                                v_flex()
                                                                    .flex_1()
                                                                    .min_w(px(0.))
                                                                    .gap_0p5()
                                                                    .when(!expanded, |this| {
                                                                        this.child(
                                                                            div()
                                                                                .w_full()
                                                                                .truncate()
                                                                                .text_size(ui_rems(0.833))
                                                                                .child(command.clone()),
                                                                        )
                                                                        .child(
                                                                            div()
                                                                                .w_full()
                                                                                .truncate()
                                                                                .text_size(ui_rems(0.667))
                                                                                .text_color(
                                                                                    row_theme
                                                                                        .muted_foreground,
                                                                                )
                                                                                .child(
                                                                                    t!(
                                                                                        "process_summary",
                                                                                        name = process.user.as_str(),
                                                                                        pid = pid
                                                                                    )
                                                                                    .to_string(),
                                                                                ),
                                                                        )
                                                                    })
                                                                    .when(expanded, |this| {
                                                                        this.child(
                                                                            div()
                                                                                .text_size(ui_rems(0.75))
                                                                                .text_color(
                                                                                    row_theme
                                                                                        .muted_foreground,
                                                                                )
                                                                                .child(
                                                                                    t!("process_details")
                                                                                        .to_string(),
                                                                                ),
                                                                        )
                                                                    }),
                                                            )
                                                            .child(
                                                                div()
                                                                    .w(metric_column_width)
                                                                    .flex_none()
                                                                    .text_size(ui_rems(0.75))
                                                                    .font_weight(FontWeight::SEMIBOLD)
                                                                    .text_right()
                                                                    .text_color(
                                                                        if view_mode
                                                                            == ServerMonitorView::Cpu
                                                                        {
                                                                            row_theme.chart_1
                                                                        } else {
                                                                            row_theme.chart_2
                                                                        },
                                                                    )
                                                                    .child(metric),
                                                            )
                                                            .child(
                                                                div()
                                                                    .w(action_column_width)
                                                                    .flex_none()
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .on_mouse_down(
                                                                        MouseButton::Left,
                                                                        |_, _, cx| {
                                                                            cx.stop_propagation();
                                                                        },
                                                                    )
                                                                    .child(
                                                                        PointerClipboard::new((
                                                                            "copy-process",
                                                                            pid as usize,
                                                                        ))
                                                                        .value(copy_payload)
                                                                        .tooltip(
                                                                            t!("copy_process")
                                                                                .to_string(),
                                                                        ),
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .w(action_column_width)
                                                                    .flex_none()
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .on_mouse_down(
                                                                        MouseButton::Left,
                                                                        |_, _, cx| {
                                                                            cx.stop_propagation();
                                                                        },
                                                                    )
                                                                    .child(
                                                                        pointer_button((
                                                                            "terminate-process",
                                                                            pid as usize,
                                                                        ))
                                                                        .danger()
                                                                        .outline()
                                                                        .small()
                                                                        .icon(IconName::Delete)
                                                                        .tooltip(
                                                                            t!("terminate_process")
                                                                                .to_string(),
                                                                        )
                                                                        .disabled(
                                                                            pid <= 1 || is_terminating,
                                                                        )
                                                                        .on_click(cx.listener(
                                                                            move |this, _, window, cx| {
                                                                                cx.stop_propagation();
                                                                                this.show_terminate_process_dialog(
                                                                                    tab_id.clone(),
                                                                                    process_for_dialog.clone(),
                                                                                    window,
                                                                                    cx,
                                                                                )
                                                                            },
                                                                        )),
                                                                    ),
                                                            ),
                                                    )
                                                    .when(expanded, |this| {
                                                        this.child(
                                                            v_flex()
                                                                .w_full()
                                                                .min_w(px(0.))
                                                                .max_w_full()
                                                                .overflow_hidden()
                                                                .gap_1()
                                                                .p_2()
                                                                .rounded_md()
                                                                .bg(row_theme.background.opacity(0.6))
                                                                .child(
                                                                    div()
                                                                        .w_full()
                                                                        .min_w(px(0.))
                                                                        .whitespace_normal()
                                                                        .text_size(ui_rems(0.833))
                                                                        .child(format!(
                                                                            "{}: {}",
                                                                            t!("process_command"),
                                                                            command
                                                                        )),
                                                                )
                                                                .child(
                                                                    h_flex()
                                                                        .gap_3()
                                                                        .text_size(ui_rems(0.75))
                                                                        .text_color(row_theme.muted_foreground)
                                                                        .child(format!(
                                                                            "{}: {}",
                                                                            t!("user"),
                                                                            process.user
                                                                        ))
                                                                        .child(format!(
                                                                            "{}: {}",
                                                                            t!("process_pid"),
                                                                            pid
                                                                        ))
                                                                        .child(format!(
                                                                            "{}: {:.2}%",
                                                                            t!("cpu"),
                                                                            process.cpu_percent
                                                                        ))
                                                                        .child(format!(
                                                                            "{}: {}",
                                                                            t!("memory"),
                                                                            format_bytes(process.memory_bytes)
                                                                        )),
                                                                ),
                                                        )
                                                    })
                                                    .into_any_element()
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .bottom_0()
                                        .w(px(TERMINAL_SCROLLBAR_GUTTER))
                                        .child(
                                            Scrollbar::vertical(&self.process_scroll_handle)
                                                .scrollbar_show(ScrollbarShow::Scrolling),
                                        ),
                                ),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(crate) fn render_remote_port_list(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let port_filter = self
            .remote_port_filter_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let has_port_data = !self.remote_ports.is_empty();
        let ports = self
            .remote_ports
            .iter()
            .filter(|port| port_matches_filter(port, &port_filter))
            .cloned()
            .collect::<Vec<_>>();
        let status = self.remote_ports_status.clone();
        let loading = self.remote_ports_in_flight;
        let empty_message = if has_port_data {
            t!("no_matching_ports").to_string()
        } else {
            t!("no_ports").to_string()
        };
        let theme = cx.theme().colors;

        v_flex()
            .flex_1()
            .min_h(px(0.))
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .h(px(28.))
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .text_size(ui_rems(0.75))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(t!("network_ports").to_string()),
                    )
                    .child(
                        pointer_button("refresh-ports")
                            .ghost()
                            .small()
                            .icon(IconName::Redo)
                            .tooltip(t!("refresh_ports").to_string())
                            .disabled(loading)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_active_port_snapshot();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex().h(px(34.)).flex_none().items_center().px_2().child(
                    Input::new(&self.remote_port_filter_input)
                        .flex_1()
                        .min_w(px(0.)),
                ),
            )
            .child(
                h_flex()
                    .h(px(24.))
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .text_size(ui_rems(0.667))
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .w(px(64.))
                            .flex_none()
                            .child(t!("protocol").to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(t!("address").to_string()),
                    )
                    .child(div().w(px(60.)).flex_none().child(t!("port").to_string()))
                    .child(div().w(px(82.)).flex_none().child(t!("state").to_string()))
                    .child(
                        div()
                            .w(px(150.))
                            .flex_none()
                            .child(t!("process").to_string()),
                    ),
            )
            .children(status.map(|status| {
                div()
                    .w_full()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .truncate()
                    .text_size(ui_rems(0.75))
                    .text_color(theme.muted_foreground)
                    .child(status)
            }))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .when(ports.is_empty() && !loading, |this| {
                        this.flex().items_center().justify_center().child(
                            div()
                                .text_size(ui_rems(0.833))
                                .text_color(theme.muted_foreground)
                                .child(empty_message.clone()),
                        )
                    })
                    .when(!ports.is_empty(), |this| {
                        this.child(
                            div()
                                .relative()
                                .size_full()
                                .child(
                                    uniform_list(
                                        "remote-port-list",
                                        ports.len(),
                                        move |range, _window, _cx| {
                                            range
                                                .filter_map(|index| {
                                                    let port = ports.get(index)?;
                                                    Some(
                                                        h_flex()
                                                            .h(px(40.))
                                                            .w_full()
                                                            .items_center()
                                                            .gap_2()
                                                            .px_2()
                                                            .border_b_1()
                                                            .border_color(theme.border.opacity(0.5))
                                                            .when(index % 2 == 1, |this| {
                                                                this.bg(theme.muted.opacity(0.35))
                                                            })
                                                            .child(
                                                                div()
                                                                    .w(px(64.))
                                                                    .flex_none()
                                                                    .text_size(ui_rems(0.75))
                                                                    .child(port.protocol.clone()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .flex_1()
                                                                    .min_w(px(0.))
                                                                    .truncate()
                                                                    .text_size(ui_rems(0.75))
                                                                    .child(port.address.clone()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .w(px(60.))
                                                                    .flex_none()
                                                                    .text_size(ui_rems(0.75))
                                                                    .child(port.port.to_string()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .w(px(82.))
                                                                    .flex_none()
                                                                    .truncate()
                                                                    .text_size(ui_rems(0.75))
                                                                    .text_color(
                                                                        theme.muted_foreground,
                                                                    )
                                                                    .child(port.state.clone()),
                                                            )
                                                            .child(
                                                                div()
                                                                    .w(px(150.))
                                                                    .flex_none()
                                                                    .truncate()
                                                                    .text_size(ui_rems(0.75))
                                                                    .text_color(
                                                                        theme.muted_foreground,
                                                                    )
                                                                    .child(match port.pid {
                                                                        Some(pid) => t!(
                                                                            "process_summary",
                                                                            name = port
                                                                                .process
                                                                                .as_str(),
                                                                            pid = pid
                                                                        )
                                                                        .to_string(),
                                                                        None => {
                                                                            port.process.clone()
                                                                        }
                                                                    }),
                                                            )
                                                            .into_any_element(),
                                                    )
                                                })
                                                .collect::<Vec<_>>()
                                        },
                                    )
                                    .size_full()
                                    .track_scroll(&self.port_scroll_handle),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .bottom_0()
                                        .w(px(8.))
                                        .child(
                                            Scrollbar::vertical(&self.port_scroll_handle)
                                                .scrollbar_show(ScrollbarShow::Scrolling),
                                        ),
                                ),
                        )
                    }),
            )
            .into_any_element()
    }

    fn connection_group_sections(&self, filter: &str) -> Vec<ConnectionGroupSection> {
        let ungrouped_matches_filter =
            !filter.is_empty() && t!("ungrouped").to_string().to_lowercase().contains(filter);
        let mut sections = self
            .config
            .connection_groups()
            .into_iter()
            .map(|name| ConnectionGroupSection {
                name,
                sessions: Vec::new(),
            })
            .collect::<Vec<_>>();
        let mut ungrouped = Vec::new();

        for session in self
            .config
            .sessions()
            .iter()
            .filter(|session| {
                connection_matches_filter(session, filter)
                    || (session.group.trim().is_empty() && ungrouped_matches_filter)
            })
            .cloned()
        {
            if session.group.trim().is_empty() {
                ungrouped.push(session);
                continue;
            }
            if let Some(section) = sections
                .iter_mut()
                .find(|section| section.name.eq_ignore_ascii_case(&session.group))
            {
                section.sessions.push(session);
            } else {
                sections.push(ConnectionGroupSection {
                    name: session.group.clone(),
                    sessions: vec![session],
                });
            }
        }

        if !filter.is_empty() {
            sections.retain(|section| {
                !section.sessions.is_empty() || section.name.to_lowercase().contains(filter)
            });
        }
        if !ungrouped.is_empty() {
            sections.push(ConnectionGroupSection {
                name: String::new(),
                sessions: ungrouped,
            });
        }
        sections
    }

    fn render_connection_row(
        &self,
        session: crate::session::config::Session,
        active_session_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let edit_id = session.id.clone();
        let delete_id = session.id.clone();
        let management_mode = self.connection_management_mode;
        let is_active = active_session_id == Some(session.id.as_str());
        let is_selected = management_mode && self.selected_connection_ids.contains(&session.id);
        let name = session.name.clone();
        let detail = self.session_detail(&session);
        let selection_id = session.id.clone();
        let row_selection_id = selection_id.clone();
        let row_id = ElementId::Name(format!("saved-connect-{}", session.id).into());

        div()
            .id(row_id)
            .w_full()
            .min_w(px(0.))
            .pl(px(3.))
            .pr_1()
            .py_1()
            .rounded_sm()
            .border_l_2()
            .border_color(if is_active {
                cx.theme().primary
            } else {
                cx.theme().transparent
            })
            .bg(if is_active {
                cx.theme().tab_active
            } else {
                cx.theme().transparent
            })
            .hover(|this| this.bg(cx.theme().secondary))
            .cursor_pointer()
            .when(management_mode, |this| {
                this.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        let selected = !this.selected_connection_ids.contains(&row_selection_id);
                        this.toggle_connection_selection(row_selection_id.clone(), selected, cx);
                    }),
                )
            })
            .when(!management_mode, |this| {
                let connect_id = session.id.clone();
                this.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.connect_saved_session(connect_id.clone(), window, cx);
                    }),
                )
            })
            .context_menu({
                let view = cx.entity();
                move |menu, window, _| {
                    let edit_value = edit_id.clone();
                    let clone_value = edit_id.clone();
                    let delete_value = delete_id.clone();
                    menu.item(PopupMenuItem::new(t!("clone").to_string()).on_click(
                        window.listener_for(&view, move |this, _, window, cx| {
                            this.clone_saved_session(clone_value.clone(), window, cx)
                        }),
                    ))
                    .item(
                        PopupMenuItem::new(t!("edit").to_string()).on_click(window.listener_for(
                            &view,
                            move |this, _, window, cx| {
                                this.edit_saved_session(edit_value.clone(), window, cx)
                            },
                        )),
                    )
                    .item(
                        PopupMenuItem::new(t!("delete").to_string()).on_click(window.listener_for(
                            &view,
                            move |this, _, _, cx| {
                                this.remove_saved_session(delete_value.clone(), cx)
                            },
                        )),
                    )
                }
            })
            .child(
                h_flex()
                    .w_full()
                    .min_w(px(0.))
                    .items_center()
                    .gap_0()
                    .when(management_mode, |this| {
                        this.child(
                            h_flex()
                                .w(px(16.))
                                .mr_1()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_mouse_down(MouseButton::Right, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(
                                    pointer_checkbox(ElementId::Name(
                                        format!("connection-check-{selection_id}").into(),
                                    ))
                                    .checked(is_selected)
                                    .tab_stop(false)
                                    .on_click(cx.listener({
                                        let selection_id = selection_id.clone();
                                        move |this, checked, _, cx| {
                                            this.toggle_connection_selection(
                                                selection_id.clone(),
                                                *checked,
                                                cx,
                                            );
                                        }
                                    })),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .flex_basis(relative(0.5))
                            .min_w(px(0.))
                            .truncate()
                            .text_size(ui_rems(0.833))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex_basis(relative(0.5))
                            .min_w(px(0.))
                            .truncate()
                            .text_right()
                            .text_size(ui_rems(0.75))
                            .text_color(cx.theme().muted_foreground)
                            .child(detail),
                    ),
            )
            .into_any_element()
    }

    fn render_connection_group_section(
        &self,
        section: ConnectionGroupSection,
        force_expanded: bool,
        active_session_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let group = section.name.clone();
        let display_name = if group.is_empty() {
            t!("ungrouped").to_string()
        } else {
            group.clone()
        };
        let management_mode = self.connection_management_mode;
        let count = section.sessions.len();
        let group_session_ids = section
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        let selected_count = if management_mode {
            group_session_ids
                .iter()
                .filter(|session_id| self.selected_connection_ids.contains(*session_id))
                .count()
        } else {
            0
        };
        let selection_state = SelectionState::from_counts(selected_count, count);
        let has_sessions = !group_session_ids.is_empty();
        let select_group_session_ids = group_session_ids.clone();
        let collapsed = !force_expanded && self.config.is_connection_group_collapsed(&group);
        let toggle_group = group.clone();
        let group_id = if group.is_empty() {
            "section-ungrouped".to_string()
        } else {
            format!("section-group-{group}")
        };

        v_flex()
            .w_full()
            .min_w(px(0.))
            .gap_1()
            .child(
                h_flex()
                    .id(ElementId::Name(
                        format!("connection-group-header-{group_id}").into(),
                    ))
                    .w_full()
                    .min_w(px(0.))
                    .h(px(28.))
                    .pl(px(5.))
                    .pr_1()
                    .items_center()
                    .gap_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|this| this.bg(cx.theme().secondary))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.toggle_connection_group(toggle_group.clone(), cx);
                        }),
                    )
                    .when(management_mode, |this| {
                        this.child(
                            h_flex()
                                .w(px(16.))
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .child(
                                    PointerSelectionCheckbox::new(ElementId::Name(
                                        format!("connection-group-check-{group_id}").into(),
                                    ))
                                    .state(selection_state)
                                    .disabled(!has_sessions)
                                    .on_click(cx.listener(
                                        move |this, checked, _, cx| {
                                            this.set_connection_selection(
                                                select_group_session_ids.clone(),
                                                *checked,
                                                cx,
                                            );
                                        },
                                    )),
                                ),
                        )
                    })
                    .child(
                        Icon::new(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .with_size(Size::XSmall)
                        .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        Icon::new(if collapsed {
                            IconName::FolderClosed
                        } else {
                            IconName::FolderOpen
                        })
                        .with_size(Size::Small)
                        .text_color(cx.theme().primary),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(ui_rems(0.8))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(display_name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui_rems(0.7))
                            .text_color(cx.theme().muted_foreground)
                            .child(count.to_string()),
                    )
                    .when(!group.is_empty(), |this| {
                        let rename_group = group.clone();
                        let delete_group = group.clone();
                        this.child(
                            div()
                                .flex_none()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(
                                    pointer_button(ElementId::Name(
                                        format!("connection-group-menu-{group_id}").into(),
                                    ))
                                    .ghost()
                                    .small()
                                    .icon(IconName::Ellipsis)
                                    .tooltip(t!("more").to_string())
                                    .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                        let view = cx.entity();
                                        move |menu, window, _| {
                                            let rename_group = rename_group.clone();
                                            let delete_group = delete_group.clone();
                                            menu.min_w(0.)
                                                .item(
                                                    PopupMenuItem::new(
                                                        t!("rename_connection_group").to_string(),
                                                    )
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        move |this, _, window, cx| {
                                                            this.show_connection_group_dialog(
                                                                Some(rename_group.clone()),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                                )
                                                .item(
                                                    PopupMenuItem::new(
                                                        t!("delete_connection_group").to_string(),
                                                    )
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        move |this, _, _, cx| {
                                                            this.delete_connection_group(
                                                                delete_group.clone(),
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                                )
                                        }
                                    }),
                                ),
                        )
                    }),
            )
            .when(!collapsed, |this| {
                this.child(
                    v_flex().w_full().min_w(px(0.)).gap_1().children(
                        section.sessions.into_iter().map(|session| {
                            self.render_connection_row(session, active_session_id, cx)
                        }),
                    ),
                )
            })
            .into_any_element()
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let connection_filter = self
            .connection_filter_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let group_sections = self.connection_group_sections(&connection_filter);
        let empty_connections_message = if self.config.sessions().is_empty() {
            t!("no_connections").to_string()
        } else {
            t!("no_matching_connections").to_string()
        };
        let management_mode = self.connection_management_mode;
        let no_saved_connections = self.config.sessions().is_empty();
        let has_group_sections = !group_sections.is_empty();
        let visible_session_ids = group_sections
            .iter()
            .flat_map(|section| section.sessions.iter())
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        let has_connections = !visible_session_ids.is_empty();
        let all_connections_selected = management_mode
            && has_connections
            && visible_session_ids
                .iter()
                .all(|id| self.selected_connection_ids.contains(id));
        let total_connections = self.config.sessions().len();
        let selected_connections = self
            .config
            .sessions()
            .iter()
            .filter(|session| management_mode && self.selected_connection_ids.contains(&session.id))
            .count();
        let has_selected_connections = management_mode && selected_connections > 0;
        let connection_groups = self.config.connection_groups();
        let saved_sessions_overflowing = self.saved_sessions_overflowing;
        let saved_sessions_scroll_handle = self.saved_scroll_handle.clone();
        let sidebar_view = cx.entity();
        let active_session_id = self.active_session_id().map(ToOwned::to_owned);
        let is_active_ssh_connected = self
            .active_tab
            .as_ref()
            .and_then(|active_id| self.tabs.iter().find(|tab| tab.id == *active_id))
            .is_some_and(|tab| tab.kind == TabKind::Ssh && tab.connected);

        v_flex()
            .gap_3()
            .w_full()
            .h_full()
            .min_w(px(0.))
            .p_3()
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().sidebar)
            .overflow_hidden()
            .when(self.config.monitoring_position() == "Sidebar", |this| {
                this.child(self.render_sidebar_monitoring_panel(is_active_ssh_connected, cx))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .gap_2()
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_size(ui_rems(0.833))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .truncate()
                                    .child(t!("connection_management")),
                            )
                            .child(
                                pointer_button("toggle-connection-management-mode")
                                    .ghost()
                                    .small()
                                    .when(management_mode, |this| this.icon(IconName::Check))
                                    .label(if management_mode {
                                        t!("done").to_string()
                                    } else {
                                        t!("edit").to_string()
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let current = this.connection_management_mode;
                                        this.set_connection_management_mode(!current, cx);
                                    })),
                            )
                            .child(
                                pointer_button("open-ssh-panel")
                                    .primary()
                                    .small()
                                    .icon(IconName::Plus)
                                    .label(t!("new_connection_short").to_string())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_new_ssh_dialog(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        Input::new(&self.connection_filter_input)
                            .w_full()
                            .min_w(px(0.)),
                    )
                    .when(management_mode, |this| {
                        this.child(
                            pointer_button("new-connection-group-fullwidth")
                                .secondary()
                                .w_full()
                                .icon(IconName::Plus)
                                .label(t!("new_connection_group").to_string())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show_connection_group_dialog(None, window, cx);
                                })),
                        )
                    })
                    .when(management_mode, |this| {
                        this.child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap_1()
                                .pl(px(5.))
                                .py(px(4.))
                                .child(
                                    h_flex()
                                        .w(px(16.))
                                        .flex_none()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            pointer_checkbox("connections-select-all")
                                                .checked(all_connections_selected)
                                                .disabled(!has_connections)
                                                .tab_stop(false)
                                                .on_click(cx.listener({
                                                    let visible_session_ids =
                                                        visible_session_ids.clone();
                                                    move |this, checked, _, cx| {
                                                        this.set_connection_selection(
                                                            visible_session_ids.clone(),
                                                            *checked,
                                                            cx,
                                                        );
                                                    }
                                                })),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(ui_rems(0.75))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{selected_connections}/{total_connections}"
                                        )),
                                )
                                .child(div().flex_1())
                                .child({
                                    let groups = connection_groups.clone();
                                    pointer_button("move-selected-connections")
                                        .ghost()
                                        .icon(IconName::FolderClosed)
                                        .label(t!("move_to_group").to_string())
                                        .tooltip(t!("move_to_group").to_string())
                                        .disabled(!has_selected_connections)
                                        .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                            let view = cx.entity();
                                            move |menu, window, _| {
                                                let menu = menu.min_w(0.).item(
                                                PopupMenuItem::new(t!("ungrouped").to_string())
                                                    .on_click(window.listener_for(
                                                        &view,
                                                        |this, _, _, cx| {
                                                            this.move_selected_connections_to_group(
                                                                String::new(),
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            );
                                                groups.iter().fold(menu, |menu, group| {
                                                    let group = group.clone();
                                                    let label = group.clone();
                                                    menu.item(PopupMenuItem::new(label).on_click(
                                                    window.listener_for(
                                                        &view,
                                                        move |this, _, _, cx| {
                                                            this.move_selected_connections_to_group(
                                                                group.clone(),
                                                                cx,
                                                            );
                                                        },
                                                    ),
                                                ))
                                                })
                                            }
                                        })
                                })
                                .child(
                                    pointer_button("delete-selected-connections")
                                        .danger()
                                        .icon(IconName::Delete)
                                        .label(t!("delete_selected_connections").to_string())
                                        .disabled(!has_selected_connections)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.remove_selected_sessions(cx);
                                        })),
                                ),
                        )
                    })
                    .child(
                        h_flex()
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .w_full()
                            .h_full()
                            .when(!has_group_sections, |this| {
                                this.child(
                                    v_flex()
                                        .absolute()
                                        .top_0()
                                        .bottom_0()
                                        .left_0()
                                        .right_0()
                                        .items_center()
                                        .justify_center()
                                        .gap_2()
                                        .child(
                                            Icon::new(IconName::SquareTerminal)
                                                .with_size(Size::Medium)
                                                .text_color(cx.theme().muted_foreground),
                                        )
                                        .child(
                                            div()
                                                .text_size(ui_rems(0.833))
                                                .text_color(cx.theme().muted_foreground)
                                                .child(empty_connections_message.clone()),
                                        )
                                        .when(no_saved_connections, |this| {
                                            this.child(
                                                pointer_button("empty-new-connection")
                                                    .secondary()
                                                    .icon(IconName::Plus)
                                                    .label(t!("new_connection").to_string())
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.open_new_ssh_dialog(window, cx)
                                                        },
                                                    )),
                                            )
                                        }),
                                )
                            })
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .h_full()
                                    .id("saved-sessions-scroll")
                                    .track_scroll(&self.saved_scroll_handle)
                                    .overflow_y_scroll()
                                    .on_prepaint(move |_, _, cx| {
                                        let overflowing =
                                            saved_sessions_scroll_handle.max_offset().y > px(0.);
                                        sidebar_view.update(cx, |this, cx| {
                                            if this.saved_sessions_overflowing != overflowing {
                                                this.saved_sessions_overflowing = overflowing;
                                                cx.notify();
                                            }
                                        });
                                    })
                                    .gap_2()
                                    .children(group_sections.into_iter().map(|section| {
                                        self.render_connection_group_section(
                                            section,
                                            !connection_filter.is_empty(),
                                            active_session_id.as_deref(),
                                            cx,
                                        )
                                    })),
                            )
                            .when(saved_sessions_overflowing, |this| {
                                this.child(
                                    div().relative().w(px(16.)).h_full().flex_none().child(
                                        gpui_component::scroll::Scrollbar::new(
                                            &self.saved_scroll_handle,
                                        )
                                        .id("saved-scrollbar")
                                        .axis(gpui_component::scroll::ScrollbarAxis::Vertical)
                                        .scrollbar_show(
                                            gpui_component::scroll::ScrollbarShow::Always,
                                        ),
                                    ),
                                )
                            }),
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
        let active_group_index = self
            .active_group
            .as_ref()
            .and_then(|gid| self.tab_groups.iter().position(|g| g.id == *gid));
        let selected = active_group_index
            .or_else(|| {
                self.active_tab.as_ref().and_then(|active_id| {
                    self.tab_groups
                        .iter()
                        .position(|group| group.pane_root.contains(active_id))
                })
            })
            .unwrap_or(0);
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
                (g.id.clone(), self.tab_group_display_name(g), pane_ids)
            })
            .collect();
        let tabbar_menu = {
            let view = view.clone();
            let tab_entries = groups_data.clone();
            let active_group = self.active_group.clone();
            let active_tab = self.active_tab.clone();
            h_flex().flex_none().child(
                pointer_button("tabbar-menu")
                    .ghost()
                    .icon(IconName::ChevronDown)
                    .tooltip(t!("settings_tab_list").to_string())
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, menu_cx| {
                        let popup_menu = menu_cx.entity();
                        tab_entries.iter().enumerate().fold(
                            menu.scrollable(true),
                            |menu, (ix, (group_id, label, pane_ids))| {
                                let group_id = group_id.clone();
                                let drag_group_id = group_id.clone();
                                let target_group_id = group_id.clone();
                                let target_group_for_style = group_id.clone();
                                let close_tab_id = if active_group.as_ref() == Some(&group_id) {
                                    active_tab.clone().or_else(|| pane_ids.first().cloned())
                                } else {
                                    pane_ids.first().cloned()
                                };
                                let item_view = view.clone();
                                let item_menu = popup_menu.clone();
                                let label = label.clone();
                                menu.item(
                                    PopupMenuItem::element(move |_, _| {
                                        let drag_group_id = drag_group_id.clone();
                                        let target_group_for_style = target_group_for_style.clone();
                                        let drop_view = item_view.clone();
                                        let drop_menu = item_menu.clone();
                                        let drop_target = target_group_id.clone();
                                        let close_view = item_view.clone();
                                        let close_menu = item_menu.clone();
                                        let close_tab_id = close_tab_id.clone();
                                        h_flex()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .items_center()
                                            .id(("tab-group-drag", ix))
                                            .cursor_grab()
                                            .drag_over::<TabGroupDrag>(move |this, drag, _, cx| {
                                                if drag.group_id == target_group_for_style {
                                                    this
                                                } else {
                                                    this.border_t_2()
                                                        .border_color(cx.theme().drag_border)
                                                        .bg(cx.theme().drop_target)
                                                }
                                            })
                                            .on_drag(
                                                TabGroupDrag {
                                                    group_id: drag_group_id,
                                                },
                                                |drag, _, _, cx| {
                                                    cx.stop_propagation();
                                                    cx.new(|_| {
                                                        let _ = drag;
                                                        Empty
                                                    })
                                                },
                                            )
                                            .on_drop::<TabGroupDrag>(move |drag, _, cx| {
                                                let dragged_group_id = drag.group_id.clone();
                                                drop_view.update(cx, |this, cx| {
                                                    this.reorder_tab_groups(
                                                        &dragged_group_id,
                                                        &drop_target,
                                                        cx,
                                                    );
                                                });
                                                drop_menu.update(cx, |_, cx| {
                                                    cx.emit(DismissEvent);
                                                });
                                            })
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .truncate()
                                                    .child(label.clone()),
                                            )
                                            .child(
                                                pointer_button(("tab-group-close", ix))
                                                    .ghost()
                                                    .icon(IconName::Delete)
                                                    .tooltip(t!("delete").to_string())
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        |_, window, cx| {
                                                            window.prevent_default();
                                                            cx.stop_propagation();
                                                        },
                                                    )
                                                    .on_click(move |_, window, cx| {
                                                        window.prevent_default();
                                                        cx.stop_propagation();
                                                        if let Some(tab_id) = close_tab_id.clone() {
                                                            close_view.update(cx, |this, cx| {
                                                                this.close_tab(tab_id, cx);
                                                            });
                                                        }
                                                        close_menu.update(cx, |_, cx| {
                                                            cx.emit(DismissEvent);
                                                        });
                                                    }),
                                            )
                                    })
                                    .checked(ix == selected)
                                    .on_click(
                                        window.listener_for(&view, move |this, _, window, cx| {
                                            this.activate_group(group_id.clone(), window, cx);
                                            this.ensure_tab_visible(ix, window, cx);
                                        }),
                                    ),
                                )
                            },
                        )
                    }),
            )
        };
        h_flex()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .pl(px(8.))
            .items_center()
            .gap_1()
            .child(
                pointer_button("sidebar-toggle")
                    .ghost()
                    .icon(if self.sidebar_collapsed {
                        IconName::PanelLeftOpen
                    } else {
                        IconName::PanelLeftClose
                    })
                    .tooltip(t!("settings_toggle_sidebar").to_string())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_collapsed = !this.sidebar_collapsed;
                        this.is_layout_reset = false;
                        this.config.set_sidebar_collapsed(this.sidebar_collapsed);
                        this.save_preferences_background();
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .overflow_x_hidden()
                            .child({
                                h_flex()
                                    .id("ashell-tab-bar")
                                    .relative()
                                    .min_w(px(0.))
                                    .w_full()
                                    .h_full()
                                    .items_center()
                                    .gap_2()
                                    .overflow_x_scroll()
                                    .track_scroll(&self.tabs_scroll_handle)
                                    .on_scroll_wheel(cx.listener(|this, _, _, _| {
                                        this.tab_scroll_animation_id =
                                            this.tab_scroll_animation_id.wrapping_add(1);
                                    }))
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .children(groups_data.iter().enumerate().map(
                                        |(ix, (group_id, title, pane_ids))| {
                                            let gid = group_id.clone();
                                            let label = title.clone();
                                            let click_gid = gid.clone();
                                            let close_id = if self.active_group.as_ref()
                                                == Some(&gid)
                                            {
                                                self.active_tab.clone().unwrap_or_else(|| {
                                                    pane_ids.first().cloned().unwrap_or_default()
                                                })
                                            } else {
                                                pane_ids.first().cloned().unwrap_or_default()
                                            };

                                            let dot_color = pane_ids
                                                .first()
                                                .and_then(|id| {
                                                    self.tabs.iter().find(|t| t.id == *id)
                                                })
                                                .map(|tab| {
                                                    if tab.connected {
                                                        cx.theme().success
                                                    } else {
                                                        cx.theme().danger
                                                    }
                                                })
                                                .unwrap_or(cx.theme().success);
                                            let has_unread_notification =
                                                pane_ids.iter().any(|id| {
                                                    self.unread_terminal_notifications.contains(id)
                                                });
                                            let output_active = ix != selected
                                                && pane_ids.iter().any(|id| {
                                                    self.tabs
                                                        .iter()
                                                        .find(|tab| tab.id == *id)
                                                        .is_some_and(TerminalTab::has_recent_output)
                                                });
                                            h_flex()
                                                .id(("ashell-tab", ix))
                                                .relative()
                                                .flex_none()
                                                .h(px(34.))
                                                .min_w(px(112.))
                                                .max_w(px(220.))
                                                .border_b_2()
                                                .border_color(if ix == selected {
                                                    cx.theme().primary
                                                } else {
                                                    cx.theme().transparent
                                                })
                                                .text_size(ui_rems(0.875))
                                                .hover(|this| {
                                                    this.text_color(
                                                        cx.theme().tab_active_foreground,
                                                    )
                                                })
                                                .child(
                                                    h_flex()
                                                        .w_full()
                                                        .h_full()
                                                        .min_w(px(0.))
                                                        .px_2()
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            h_flex()
                                                                .flex_none()
                                                                .size(px(12.))
                                                                .items_center()
                                                                .justify_center()
                                                                .when(
                                                                    output_active
                                                                        && !has_unread_notification,
                                                                    |this| {
                                                                        this.child(
                                                                            Spinner::new()
                                                                                .small()
                                                                                .color(
                                                                                    cx.theme()
                                                                                        .primary,
                                                                                ),
                                                                        )
                                                                    },
                                                                )
                                                                .when(
                                                                    !output_active
                                                                        && !has_unread_notification,
                                                                    |this| {
                                                                        this.child(
                                                                            div()
                                                                                .size(px(6.))
                                                                                .rounded_full()
                                                                                .bg(dot_color),
                                                                        )
                                                                    },
                                                                )
                                                                .when(
                                                                    has_unread_notification,
                                                                    |this| {
                                                                        this.child(
                                                                            flashing_terminal_notification_icon(
                                                                                ix,
                                                                                cx.theme().danger,
                                                                            ),
                                                                        )
                                                                    },
                                                                ),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w(px(0.))
                                                                .truncate()
                                                                .when(ix == selected, |this| {
                                                                    this.font_weight(
                                                                        FontWeight::SEMIBOLD,
                                                                    )
                                                                    .text_color(
                                                                        cx.theme().foreground,
                                                                    )
                                                                })
                                                                .when(ix != selected, |this| {
                                                                    this.text_color(
                                                                        cx.theme().muted_foreground,
                                                                    )
                                                                })
                                                                .child(label),
                                                        )
                                                        .child(
                                                            pointer_button(("tab-close", ix))
                                                                .ghost()
                                                                .small()
                                                                .icon(IconName::Close)
                                                                .opacity(if ix == selected {
                                                                    0.8
                                                                } else {
                                                                    0.45
                                                                })
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
                                                                            this.close_tab(
                                                                                close_id.clone(),
                                                                                cx,
                                                                            )
                                                                        }
                                                                    },
                                                                )),
                                                        ),
                                                )
                                                .cursor_pointer()
                                                .on_mouse_down(MouseButton::Left, |_, window, _| {
                                                    window.prevent_default();
                                                })
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.activate_group(
                                                            click_gid.clone(),
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                        },
                                    ))
                            })
                            .on_prepaint({
                                let view = view.clone();
                                move |bounds, _, cx| {
                                    view.update(cx, |this, _| {
                                        this.tabs_viewport_bounds = Some(bounds);
                                    });
                                }
                            }),
                    )
                    .child(tabbar_menu),
            )
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .pr(px(6.))
                    .child(
                        pointer_button("open-selector")
                            .ghost()
                            .icon(IconName::Plus)
                            .tooltip(t!("settings_open_session").to_string())
                            .dropdown_menu_with_anchor(Anchor::BottomRight, {
                                let view = cx.entity();
                                move |menu, window, _cx| {
                                    menu.min_w(0.)
                                        .item(
                                            PopupMenuItem::new(t!("local_terminal").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, _, cx| {
                                                        this.open_local(cx);
                                                    },
                                                )),
                                        )
                                        .item(
                                            PopupMenuItem::new(t!("open_connection").to_string())
                                                .on_click(window.listener_for(
                                                    &view,
                                                    |this, _, window, cx| {
                                                        this.show_selector_dialog(window, cx);
                                                    },
                                                )),
                                        )
                                }
                            }),
                    )
                    .child(
                        pointer_button("split-horizontal")
                            .ghost()
                            .icon(IconName::PanelBottom)
                            .tooltip(t!("settings_split_pane_down").to_string())
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                this.split_current_pane("down", cx);
                            })),
                    )
                    .child(
                        pointer_button("split-vertical")
                            .ghost()
                            .icon(IconName::PanelRight)
                            .tooltip(t!("settings_split_pane_right").to_string())
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                this.split_current_pane("right", cx);
                            })),
                    )
                    .child(self.render_search_button(cx))
                    .child(
                        pointer_button("tabbar-settings")
                            .ghost()
                            .icon(IconName::Settings)
                            .tooltip(t!("settings_open_settings").to_string())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_settings_dialog(window, cx)
                            })),
                    ),
            )
    }

    fn render_command_history_popover_content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(active_session_id) = self.active_tab.as_ref().and_then(|active_id| {
            self.tabs
                .iter()
                .find(|tab| &tab.id == active_id && tab.kind == TabKind::Ssh)
                .and_then(|tab| tab.session.as_ref())
                .map(|session| session.id.clone())
        }) else {
            return div().into_any_element();
        };

        let mut history_entries = self.config.all_command_history();
        let (mut active_history, other_history): (Vec<_>, Vec<_>) = history_entries
            .drain(..)
            .partition(|(session_id, _, _)| session_id == &active_session_id);
        active_history.extend(other_history);
        let mut seen_commands = HashSet::new();
        let history_entries = active_history
            .into_iter()
            .filter(|(_, _, command)| seen_commands.insert(command.clone()))
            .collect::<Vec<_>>();
        let total_history = history_entries.len();
        let selected_history = history_entries
            .iter()
            .filter(|(session_id, index, _)| {
                self.selected_command_history
                    .contains(&(session_id.clone(), *index))
            })
            .count();
        let history_filter = self
            .command_history_filter_input
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let filtered_history = history_entries
            .iter()
            .filter(|(_, _, command)| {
                history_filter.is_empty() || command.to_lowercase().contains(&history_filter)
            })
            .cloned()
            .collect::<Vec<_>>();
        let visible_history_keys = filtered_history
            .iter()
            .map(|(session_id, index, _)| (session_id.clone(), *index))
            .collect::<Vec<_>>();
        let all_history_selected = !visible_history_keys.is_empty()
            && visible_history_keys
                .iter()
                .all(|key| self.selected_command_history.contains(key));
        let has_selected_history = selected_history > 0;
        let has_visible_history = !filtered_history.is_empty();
        let history_has_overflow = filtered_history.len() > 10;
        let visible_history_rows = filtered_history.len().min(10);
        let history_list_height = if visible_history_rows == 0 {
            px(72.)
        } else {
            px(visible_history_rows as f32 * 32.
                + visible_history_rows.saturating_sub(1) as f32 * 8.)
        };
        let theme = cx.theme().clone();

        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(t!("command_history")),
                    )
                    .child(
                        pointer_button("close-command-history")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .tooltip(t!("close_command_history").to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_command_history(cx);
                            })),
                    ),
            )
            .child(
                Input::new(&self.command_history_filter_input)
                    .w_full()
                    .min_w(px(0.)),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_1()
                    .pl(px(5.))
                    .py(px(4.))
                    .child(
                        h_flex()
                            .w(px(16.))
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .child(
                                pointer_checkbox("command-history-select-all")
                                    .checked(all_history_selected)
                                    .disabled(!has_visible_history)
                                    .tab_stop(false)
                                    .on_click(cx.listener({
                                        let visible_history_keys = visible_history_keys.clone();
                                        move |this, checked, _, cx| {
                                            this.set_command_history_selection(
                                                visible_history_keys.clone(),
                                                *checked,
                                                cx,
                                            );
                                        }
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(0.75))
                            .text_color(theme.muted_foreground)
                            .child(format!("{selected_history}/{total_history}")),
                    )
                    .child(div().flex_1())
                    .child(
                        pointer_button("delete-selected-command-history")
                            .danger()
                            .icon(IconName::Delete)
                            .label(t!("delete_selected_connections").to_string())
                            .tooltip(t!("delete_selected_commands").to_string())
                            .disabled(!has_selected_history)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.remove_selected_command_history(cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .h(history_list_height)
                    .flex_none()
                    .min_h(px(0.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .id("ssh-command-history-scroll")
                            .track_scroll(&self.command_history_scroll_handle)
                            .overflow_y_scroll()
                            .child(if history_entries.is_empty() {
                        div()
                            .w_full()
                            .py_4()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(ui_rems(0.833))
                            .text_color(theme.muted_foreground)
                            .child(t!("command_history_empty"))
                            .into_any_element()
                    } else if filtered_history.is_empty() {
                        div()
                            .w_full()
                            .py_4()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(ui_rems(0.833))
                            .text_color(theme.muted_foreground)
                            .child(t!("no_matching_commands"))
                            .into_any_element()
                    } else {
                        v_flex()
                            .w_full()
                            .gap_2()
                            .children(filtered_history.into_iter().map(
                                |(history_session_id, history_index, command)| {
                                    let row_theme = theme.clone();
                                    let copy_value = command.clone();
                                    let execute_value = command.clone();
                                    let selection_key =
                                        (history_session_id.clone(), history_index);
                                    let is_selected =
                                        self.selected_command_history.contains(&selection_key);
                                    let row_selection_key = selection_key.clone();
                                    let row_id = format!(
                                        "ssh-command-history-row-{history_session_id}-{history_index}"
                                    );
                                    let copy_id = format!(
                                        "copy-command-{history_session_id}-{history_index}"
                                    );
                                    let execute_id = format!(
                                        "execute-command-{history_session_id}-{history_index}"
                                    );
                                    let selection_id = format!(
                                        "command-history-check-{history_session_id}-{history_index}"
                                    );
                                    h_flex()
                                            .w_full()
                                            .h(px(32.))
                                            .flex_none()
                                            .min_w(px(0.))
                                            .items_center()
                                            .gap_0()
                                            .px_1()
                                            .rounded_md()
                                            .border_1()
                                            .border_color(row_theme.border.opacity(0.6))
                                            .bg(row_theme.muted.opacity(0.35))
                                            .hover(|this| {
                                                this.bg(row_theme.secondary.opacity(0.55))
                                            })
                                            .id(ElementId::Name(row_id.into()))
                                            .cursor_pointer()
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    let selected = !this
                                                        .selected_command_history
                                                        .contains(&row_selection_key);
                                                    let (session_id, index) =
                                                        row_selection_key.clone();
                                                    this.toggle_command_history_selection(
                                                        session_id,
                                                        index,
                                                        selected,
                                                        cx,
                                                    );
                                                }),
                                            )
                                            .child(
                                                h_flex()
                                                    .w(px(16.))
                                                    .mr_1()
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
                                                        pointer_checkbox(ElementId::Name(
                                                            selection_id.into(),
                                                        ))
                                                        .checked(is_selected)
                                                        .tab_stop(false)
                                                        .on_click(cx.listener({
                                                            let (session_id, index) =
                                                                selection_key.clone();
                                                            move |this, checked, _, cx| {
                                                                this.toggle_command_history_selection(
                                                                    session_id.clone(),
                                                                    index,
                                                                    *checked,
                                                                    cx,
                                                                );
                                                            }
                                                        })),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .truncate()
                                                    .text_size(ui_rems(0.833))
                                                    .text_color(row_theme.foreground)
                                                    .child(command),
                                            )
                                            .child(
                                                h_flex()
                                                    .flex_none()
                                                    .items_center()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                |_, _, cx| cx.stop_propagation(),
                                                            )
                                                            .on_mouse_down(
                                                                MouseButton::Right,
                                                                |_, _, cx| cx.stop_propagation(),
                                                            )
                                                            .child(
                                                                pointer_button(ElementId::Name(
                                                                    execute_id.into(),
                                                                ))
                                                                .ghost()
                                                                .small()
                                                                .icon(IconName::Play)
                                                                .tooltip(
                                                                    t!("execute_command")
                                                                        .to_string(),
                                                                )
                                                                .on_click(cx.listener(
                                                                    move |this, _, window, cx| {
                                                                        this.execute_ssh_history_command(
                                                                            execute_value.clone(),
                                                                            window,
                                                                            cx,
                                                                        );
                                                                    },
                                                                )),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                |_, _, cx| cx.stop_propagation(),
                                                            )
                                                            .child(
                                                                PointerClipboard::new(copy_id)
                                                                .value(copy_value)
                                                                .tooltip(
                                                                    t!("copy_command").to_string(),
                                                                ),
                                                            ),
                                                    )
                                            )
                                            .into_any_element()
                                },
                            ))
                            .into_any_element()
                            }),
                    )
                    .when(history_has_overflow, |this| {
                        this.child(
                            div()
                                .relative()
                                .w(px(16.))
                                .h_full()
                                .flex_none()
                                .child(
                                    Scrollbar::vertical(&self.command_history_scroll_handle)
                                        .scrollbar_show(ScrollbarShow::Always),
                                ),
                        )
                    }),
            )
            .into_any_element()
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
                        view.update(cx, |this, cx| {
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
                let font_size = px(this.terminal_font_size());
                let line_height = px(this.terminal_line_height());
                let cell_width = px(this.terminal_cell_width());
                let is_url_hovered = this
                    .hovered_url
                    .as_ref()
                    .is_some_and(|hu| hu.tab_id == *tab_id);
                let mut el = div()
                    .size_full()
                    .pl(px(8.))
                    .pr(px(TERMINAL_SCROLLBAR_GUTTER))
                    .overflow_hidden()
                    .when(is_url_hovered, |d| d.cursor_pointer())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            let active_tab_changed =
                                this.active_tab.as_deref() != Some(tab_id_clone2.as_str());
                            this.focus_pane_with_id(tab_id_clone2.clone());
                            if active_tab_changed {
                                this.prompt_active_ssh_reconnect_if_needed(window, cx);
                            }
                            cx.notify();
                        }),
                    )
                    .child(terminal::element::TerminalElement::new(
                        terminal::element::TerminalElementConfig {
                            view: cx.entity(),
                            focus_handle,
                            pane_focused: is_focused,
                            snapshot,
                            marked_text,
                            font_family,
                            font_size,
                            line_height,
                            cell_width,
                            tab_id: tab_id.to_string(),
                            search_highlights: this.search_highlight_map(
                                tab_id,
                                cx.theme().danger.opacity(0.35),
                                cx.theme().danger.opacity(0.70),
                            ),
                        },
                    ));
                let scrollbar = this.terminal_scrollbars.entry(tab_id.clone()).or_default();
                el = el.vertical_scrollbar(scrollbar);

                // When disconnected, overlay a reconnect bar at the top of the terminal.
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
                        div().absolute().top_0().left_0().right_0().child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_1()
                                .bg(cx.theme().danger.opacity(0.15))
                                .cursor_pointer()
                                .child(
                                    div()
                                        .text_size(ui_rems(0.85))
                                        .text_color(cx.theme().danger)
                                        .child(
                                            t!("session_disconnected", "reason" = reason)
                                                .to_string(),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_size(ui_rems(0.85))
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
                let has_multiple_panes = this.pane_root.total_panes() > 1;

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
                                            this.start_drag_split(splitter_path.clone(), event);
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
                                        this.start_drag_split(splitter_path.clone(), event);
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
        if self
            .active_tab
            .as_ref()
            .is_some_and(|active_id| !self.tabs.iter().any(|tab| &tab.id == active_id))
        {
            self.active_tab = self.tabs.first().map(|tab| tab.id.clone());
        }
        self.sync_sftp_path_input(window, cx);

        if self.show_transfers_dialog {
            self.show_transfers_dialog = false;
            self.show_transfers_dialog(window, cx);
        }
        if let Some(active_id) = self.active_tab.clone() {
            if let Some(scrollbar) = self.terminal_scrollbars.get(&active_id) {
                if let Some(new_display_offset) = scrollbar.future_display_offset.take() {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == active_id) {
                        let current = tab.render_snapshot(false).display_offset;
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

        let has_ssh_session = self.active_ssh_session().is_some();
        let is_monitor_bottom = self.config.monitoring_position() == "Bottom";
        let is_active_ssh_connected = self
            .active_tab
            .as_ref()
            .and_then(|active_id| self.tabs.iter().find(|tab| tab.id == *active_id))
            .is_some_and(|tab| tab.kind == TabKind::Ssh && tab.connected);
        let viewport_width = window.viewport_size().width;

        let body_panel = if has_ssh_session {
            let minimized_height = 24.;
            let min_panel_height = 180.;
            let default_panel_height = 248.;

            let sftp_size = if self.sftp_panel_minimized {
                px(minimized_height)
            } else {
                px(self
                    .config
                    .body_panels()
                    .and_then(|s| s.get(1).copied())
                    .unwrap_or(default_panel_height))
            };

            let view = cx.entity();
            v_flex()
                .size_full()
                .min_w(px(0.))
                .items_stretch()
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.))
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_hidden()
                        .child(
                            v_resizable("ashell-body")
                                .lock(self.config.lock_layout())
                                .with_state(&self.body_panels)
                                .on_resize(move |_, _, cx| {
                                    view.update(cx, |this, _| {
                                        this.is_layout_reset = false;
                                    });
                                })
                                .child(
                                    resizable_panel().child(self.render_terminal_panel(window, cx)),
                                )
                                .child(
                                    resizable_panel()
                                        .size(sftp_size)
                                        .size_range(if self.sftp_panel_minimized {
                                            px(minimized_height)..px(minimized_height)
                                        } else {
                                            px(min_panel_height)..px(1200.)
                                        })
                                        .child(self.render_sftp_panel(window, cx)),
                                ),
                        ),
                )
                .when(is_monitor_bottom, |this| {
                    this.child(self.render_monitoring_panel(
                        viewport_width,
                        is_active_ssh_connected,
                        cx,
                    ))
                })
                .into_any_element()
        } else {
            v_flex()
                .size_full()
                .min_w(px(0.))
                .items_stretch()
                .child(
                    div()
                        .w_full()
                        .min_w(px(0.))
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_hidden()
                        .child(self.render_terminal_panel(window, cx)),
                )
                .when(is_monitor_bottom, |this| {
                    this.child(self.render_monitoring_panel(viewport_width, false, cx))
                })
                .into_any_element()
        };

        let workspace = if self.sidebar_collapsed {
            v_flex()
                .size_full()
                .relative()
                .overflow_hidden()
                .when(
                    self.active_title_bar_style == crate::session::config::TitleBarStyle::Native,
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
                .child(body_panel)
                .into_any_element()
        } else {
            let sidebar_area = resizable_panel()
                .size(px(self
                    .config
                    .workspace_panels()
                    .and_then(|s| s.first().copied())
                    .unwrap_or(SIDEBAR_WIDTH)))
                .size_range(px(200.)..px(520.))
                .flex_none()
                .child(self.sidebar(cx));

            let main_area = resizable_panel().min_w(px(0.)).child(
                v_flex()
                    .size_full()
                    .min_w(px(0.))
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

            let view = cx.entity();
            h_resizable("ashell-workspace")
                .lock(self.config.lock_layout())
                .with_state(&self.workspace_panels)
                .on_resize(move |_, _, cx| {
                    view.update(cx, |this, _| {
                        this.is_layout_reset = false;
                    });
                })
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
            .on_action(cx.listener(|this, _: &crate::AboutAshell, window, cx| {
                this.show_about_dialog(window, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::NewLocalTerminal, _, cx| {
                this.open_local(cx);
            }))
            .on_action(cx.listener(|this, _: &crate::CloseWindow, window, cx| {
                this.save_layout_state(window, cx);
                window.remove_window();
            }))
            .on_action(cx.listener(|_, _: &crate::MinimizeWindow, window, _| {
                window.minimize_window();
            }))
            .on_action(cx.listener(|_, _: &crate::ZoomWindow, window, _| {
                window.zoom_window();
            }))
            .on_action(cx.listener(|_, _: &crate::ToggleFullScreen, window, _| {
                window.toggle_fullscreen();
            }))
            .on_action(cx.listener(|this, _: &crate::OpenSettings, window, cx| this.show_settings_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &crate::OpenSession, window, cx| this.show_selector_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &crate::OpenTransfers, window, cx| this.show_transfers_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &crate::NewSsh, window, cx| this.open_new_ssh_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &crate::OpenSearch, window, cx| this.toggle_search(window, cx)))
            .on_action(cx.listener(|this, _: &crate::ToggleSidebar, _, cx| {
                this.sidebar_collapsed = !this.sidebar_collapsed;
                this.is_layout_reset = false;
                this.config.set_sidebar_collapsed(this.sidebar_collapsed);
                this.save_preferences_background();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &crate::ToggleSftpZoom, window, cx| {
                this.toggle_sftp_minimized(window, cx);
            }))
            .on_action(cx.listener(|this, _: &crate::FocusPaneLeft, window, cx| this.focus_adjacent_pane("left", window, cx)))
            .on_action(cx.listener(|this, _: &crate::FocusPaneRight, window, cx| this.focus_adjacent_pane("right", window, cx)))
            .on_action(cx.listener(|this, _: &crate::FocusPaneUp, window, cx| this.focus_adjacent_pane("up", window, cx)))
            .on_action(cx.listener(|this, _: &crate::FocusPaneDown, window, cx| this.focus_adjacent_pane("down", window, cx)))
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
                    window.dispatch_action(Box::new(gpui_component::input::Copy), cx);
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
                    window.dispatch_action(Box::new(gpui_component::input::Paste), cx);
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
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(self.render_window_controls(window, cx))
                        .child(
                            div()
                                .id("title-bar-content")
                                .flex_1()
                                .min_w(px(0.))
                                .h_full()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, _| {
                                        this.should_move_window = true;
                                    }),
                                )
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, _| {
                                        this.should_move_window = false;
                                    }),
                                )
                                .on_mouse_up_out(
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
                                        // Preserve clicks and enter the native move loop only
                                        // after dragging starts within the integrated top bar.
                                        this.should_move_window = false;
                                        crate::app::window_drag::start_window_drag(window);
                                    }
                                }))
                                .on_double_click(|_, window, _| {
                                    #[cfg(target_os = "macos")]
                                    window.titlebar_double_click();
                                    #[cfg(not(target_os = "macos"))]
                                    window.zoom_window();
                                })
                                .child(self.render_tab_bar(cx)),
                        ),
                )
            })
            .child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(workspace),
            )
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
            .when_some(self.sftp_context_menu.clone(), |this, menu| {
                let download_label = if menu.is_dir {
                    t!("download_folder").to_string()
                } else {
                    t!("download").to_string()
                };
                let edit_label = t!("edit_file").to_string();
                let rename_label = t!("rename").to_string();
                let menu_width = if menu.is_dir {
                    compact_menu_width(&[download_label.as_str(), rename_label.as_str()])
                } else {
                    compact_menu_width(&[
                        download_label.as_str(),
                        edit_label.as_str(),
                        rename_label.as_str(),
                    ])
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
                                .w(menu_width)
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
                                            pointer_button("sftp-context-download")
                                                .ghost()
                                                .w_full()
                                                .justify_start()
                                                .label(download_label)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.trigger_sftp_context_download(window, cx);
                                                })),
                                        )
                                        .when(!menu.is_dir, |this| {
                                            this.child(
                                                pointer_button("sftp-context-edit")
                                                    .ghost()
                                                    .w_full()
                                                    .justify_start()
                                                    .label(edit_label)
                                                    .tooltip(t!("edit_file_tooltip").to_string())
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.trigger_sftp_context_edit(
                                                                window, cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                        })
                                        .child(
                                            pointer_button("sftp-context-rename")
                                                .ghost()
                                                .w_full()
                                                .justify_start()
                                                .label(rename_label)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.trigger_sftp_context_rename(window, cx);
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
                                            pointer_button("ssh-connect-progress")
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
                                                                        .text_size(ui_rems(1.0))
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
                                                        pointer_button("ssh-connect-progress-retry")
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
                                                        pointer_button("ssh-connect-progress-close")
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
                        let size_changed = this
                            .last_window_size
                            .is_none_or(|prev| prev != current_win_size);
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
