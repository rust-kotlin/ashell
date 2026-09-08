use std::collections::HashMap;

use gpui::{
    Context, Focusable as _, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, ElementExt as _, IconName, Sizable as _,
    button::ButtonVariants as _, h_flex, input::Input,
};
use rust_i18n::t;

use crate::{
    Ashell,
    app::controls::{pointer_button, ui_rems},
};

impl Ashell {
    pub(crate) fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_active {
            self.close_search(window, cx);
        } else {
            self.open_search(window, cx);
        }
    }

    pub(crate) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_active = true;
        // Focus the search input on the next frame so it happens after the
        // current render cycle completes, avoiding focus being stolen back
        // by the terminal panel's track_focus.
        let search_input = self.search_input.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            search_input.update(cx, |state, cx| {
                state.focus_handle(cx).focus(window, cx);
            });
        });
        cx.notify();
    }

    pub(crate) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_active = false;
        self.search_query.clear();
        self.search_matches.clear();
        self.search_current = 0;
        self.search_target_tab = None;
        self.search_bar_bounds = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Move keyboard focus back to the search input so the user can keep typing.
    /// Deferred to the next frame so it happens after the current render cycle,
    /// preventing the terminal panel's track_focus from stealing focus back.
    fn refocus_search_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        let search_input = self.search_input.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            search_input.update(cx, |state, cx| {
                state.focus_handle(cx).focus(window, cx);
            });
        });
    }

    fn search_tab_index(&self) -> Option<usize> {
        self.active_tab
            .as_deref()
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
            .or_else(|| {
                self.active_group
                    .as_ref()
                    .and_then(|group_id| self.tab_groups.iter().find(|group| &group.id == group_id))
                    .and_then(|group| group.pane_root.first_tab_id())
                    .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
            })
            .or_else(|| (!self.tabs.is_empty()).then_some(0))
    }

    pub(crate) fn perform_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).text().to_string();
        if query.is_empty() {
            self.search_query.clear();
            self.search_matches.clear();
            self.search_current = 0;
            self.refocus_search_input(window, cx);
            cx.notify();
            return;
        }

        let Some(tab_index) = self.search_tab_index() else {
            self.status = t!("no_results").into();
            self.refocus_search_input(window, cx);
            cx.notify();
            return;
        };

        // Remember which tab was searched so highlights only appear in that pane.
        self.search_target_tab = Some(self.tabs[tab_index].id.clone());
        let tab = &self.tabs[tab_index];

        let query_lower = lowercase_search_text(&query);

        // Search the ENTIRE terminal buffer (scrollback + visible screen).
        // full_grid_rows returns grid line indices; the first row is at
        // `grid_start` (typically -history_size).
        let (grid_start, all_rows) = tab.full_grid_rows();

        let mut matches: Vec<(i32, i32)> = Vec::new();

        for (row_idx, row) in all_rows.iter().enumerate() {
            if row.is_empty() {
                continue;
            }

            let abs_row = grid_start + row_idx as i32;
            matches.extend(
                search_row_columns(row, &query_lower)
                    .into_iter()
                    .map(|col| (abs_row, col)),
            );
        }

        matches.sort_unstable();
        let match_count = count_match_groups(&matches);

        self.search_query = query;
        self.search_matches = matches;

        if match_count > 0 {
            self.search_current = 0;
            self.jump_to_current_match(cx);
        }

        self.status = format!(
            "{}: {} ({})",
            t!("search"),
            self.search_query,
            if match_count == 0 {
                t!("no_results").to_string()
            } else {
                format!("{}/{}", self.search_current + 1, match_count)
            }
        )
        .into();

        // Keep focus on the search input so the user can continue typing.
        self.refocus_search_input(window, cx);
        cx.notify();
    }

    pub(crate) fn search_goto_next(&mut self, cx: &mut Context<Self>) {
        let match_count = count_match_groups(&self.search_matches);
        if match_count == 0 {
            return;
        }
        self.search_current = (self.search_current + 1) % match_count;
        self.jump_to_current_match(cx);
        cx.notify();
    }

    pub(crate) fn search_goto_prev(&mut self, cx: &mut Context<Self>) {
        let match_count = count_match_groups(&self.search_matches);
        if match_count == 0 {
            return;
        }
        self.search_current = (self.search_current + match_count - 1) % match_count;
        self.jump_to_current_match(cx);
        cx.notify();
    }

    fn jump_to_current_match(&mut self, _cx: &mut Context<Self>) {
        // target_grid_line is the grid line index (negative = history).
        let Some((target_grid_line, _)) =
            find_nth_match_start(&self.search_matches, self.search_current)
        else {
            return;
        };

        // Find the tab ID first (immutable borrow), then look up mutably.
        let tab_id = self.active_tab.clone().or_else(|| {
            self.active_group
                .as_ref()
                .and_then(|gid| self.tab_groups.iter().find(|g| &g.id == gid))
                .and_then(|g| g.pane_root.first_tab_id())
                .map(|s| s.to_string())
        });

        let tab = if let Some(id) = tab_id.as_deref() {
            self.tabs.iter_mut().find(|t| t.id == id)
        } else {
            self.tabs.first_mut()
        };

        if let Some(tab) = tab {
            let snapshot = tab.render_snapshot(false);
            let display_offset = snapshot.display_offset as i32;
            let rows = snapshot.rows as i32;

            // viewport row = grid_line + display_offset
            let vp_row = target_grid_line + display_offset;
            let visible = vp_row >= 0 && vp_row < rows;

            if !visible {
                // Scroll so the target grid line appears near the top.
                // display_offset = -grid_line puts it at viewport row 0.
                let new_offset = (-target_grid_line).max(0) as usize;
                if new_offset > snapshot.display_offset {
                    tab.scroll_up_by(new_offset - snapshot.display_offset);
                } else if new_offset < snapshot.display_offset {
                    tab.scroll_down_by(snapshot.display_offset - new_offset);
                }
            }
        }
    }

    /// Build a highlight map for search matches, converting grid line indices
    /// to the current viewport coordinates. Only returns highlights for the
    /// pane that was actually searched (`search_target_tab`).
    pub(crate) fn search_highlight_map(
        &self,
        tab_id: &str,
        match_color: Hsla,
        current_color: Hsla,
    ) -> Option<HashMap<(i32, i32), Hsla>> {
        if self.search_matches.is_empty()
            || self.search_query.is_empty()
            || self.search_target_tab.as_deref() != Some(tab_id)
        {
            return None;
        }

        let tab = self.tabs.get(self.search_tab_index()?)?;
        let snapshot = tab.render_snapshot(false);
        let display_offset = snapshot.display_offset as i32;
        let rows = snapshot.rows as i32;

        let mut map = HashMap::new();

        let sorted = &self.search_matches;

        let mut group_idx = 0;
        let mut i = 0;
        while i < sorted.len() {
            let is_current = group_idx == self.search_current;
            let color = if is_current {
                current_color
            } else {
                match_color
            };

            // grid_line → viewport row:  vp_row = grid_line + display_offset
            let (grid_line, _) = sorted[i];
            let vp_row = grid_line + display_offset;
            let next_i = next_match_group_index(sorted, i);
            if vp_row >= 0 && vp_row < rows {
                for &(_, col) in &sorted[i..next_i] {
                    map.insert((vp_row, col), color);
                }
            }

            group_idx += 1;
            i = next_i;
        }

        Some(map)
    }

    /// Render the search button (used in the tab bar).
    pub(crate) fn render_search_button(&self, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        // Wrap in a div so .hover() doesn't conflict with Button's internal hover.
        div().child(
            pointer_button("search-btn")
                .ghost()
                .icon(IconName::Search)
                .tooltip(t!("search").to_string())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_search(window, cx);
                })),
        )
    }

    /// Render the expanded search bar overlay (when search is active).
    pub(crate) fn render_search_bar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl gpui::IntoElement {
        let match_count = count_match_groups(&self.search_matches);
        let has_query = !self.search_query.is_empty();
        let has_matches = match_count > 0;
        let current_display = if has_matches {
            format!("{}/{}", self.search_current + 1, match_count)
        } else if has_query {
            "0".to_string()
        } else {
            String::new()
        };

        let view = cx.entity();
        div()
            .absolute()
            .top(px(8.))
            .right(px(24.))
            .on_prepaint(move |bounds, _window, cx| {
                view.update(cx, |this, _| {
                    this.search_bar_bounds = Some(bounds);
                });
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.refocus_search_input(window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .p_1()
                    .rounded(px(6.))
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .w(px(200.))
                            .on_key_down(cx.listener(
                                |this, event: &gpui::KeyDownEvent, window, cx| {
                                    if event.keystroke.key.as_str() == "escape" {
                                        this.close_search(window, cx);
                                        window.prevent_default();
                                        cx.stop_propagation();
                                    }
                                },
                            ))
                            .child(Input::new(&self.search_input)),
                    )
                    .when(!current_display.is_empty(), |this| {
                        this.child(
                            div()
                                .text_size(ui_rems(0.75))
                                .text_color(cx.theme().muted_foreground)
                                .min_w(px(36.))
                                .text_center()
                                .child(current_display),
                        )
                    })
                    .child(
                        pointer_button("search-prev")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronUp)
                            .disabled(!has_matches)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.search_goto_prev(cx);
                            })),
                    )
                    .child(
                        pointer_button("search-next")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronDown)
                            .disabled(!has_matches)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.search_goto_next(cx);
                            })),
                    )
                    .child(
                        pointer_button("search-close")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_search(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

/// Normalize query and row characters in the same way, including expansions.
fn lowercase_search_text(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Map normalized UTF-8 bytes back to the original terminal columns.
fn search_row_columns(row: &[(i32, char)], query_lower: &str) -> Vec<i32> {
    if query_lower.is_empty() {
        return Vec::new();
    }
    let mut text = String::new();
    let mut byte_to_col = Vec::new();
    for &(col, character) in row {
        text.extend(character.to_lowercase());
        byte_to_col.resize(text.len(), col);
    }
    let mut columns = Vec::new();
    for (start, matched) in text.match_indices(query_lower) {
        let end = start + matched.len() - 1;
        columns.extend(byte_to_col[start]..=byte_to_col[end]);
    }
    columns
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Count distinct match groups in a sorted list of (row, col) positions.
/// A group is a run of consecutive columns in the same row.
fn count_match_groups(matches: &[(i32, i32)]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < matches.len() {
        count += 1;
        i = next_match_group_index(matches, i);
    }
    count
}

/// Find the (row, col) start of the Nth distinct match group.
fn find_nth_match_start(matches: &[(i32, i32)], n: usize) -> Option<(i32, i32)> {
    let mut group_idx = 0;
    let mut i = 0;
    while i < matches.len() {
        if group_idx == n {
            return Some(matches[i]);
        }
        group_idx += 1;
        i = next_match_group_index(matches, i);
    }
    None
}

fn next_match_group_index(matches: &[(i32, i32)], start: usize) -> usize {
    let row = matches[start].0;
    let mut end = start + 1;
    while end < matches.len() && matches[end].0 == row && matches[end].1 == matches[end - 1].1 + 1 {
        end += 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::{
        count_match_groups, find_nth_match_start, lowercase_search_text, search_row_columns,
    };

    #[test]
    fn unicode_search_preserves_columns_when_case_conversion_changes_length() {
        let row = [(0, 'K'), (1, 'İ'), (2, 'x'), (3, '中'), (5, 'X')];
        assert_eq!(
            search_row_columns(&row, &lowercase_search_text("K")),
            vec![0]
        );
        assert_eq!(
            search_row_columns(&row, &lowercase_search_text("İ")),
            vec![1]
        );
        assert_eq!(search_row_columns(&row, "x"), vec![2, 5]);
        assert_eq!(search_row_columns(&row, "中x"), vec![3, 4, 5]);
        assert!(search_row_columns(&row, "").is_empty());
    }

    #[test]
    fn searches_word_spaces_without_matching_text_across_them() {
        let row = "foo bar"
            .chars()
            .enumerate()
            .map(|(col, character)| (col as i32, character))
            .collect::<Vec<_>>();
        assert_eq!(
            search_row_columns(&row, "foo bar"),
            (0..7).collect::<Vec<_>>()
        );
        assert_eq!(search_row_columns(&row, "bar"), vec![4, 5, 6]);
        assert!(search_row_columns(&row, "foobar").is_empty());
    }

    #[test]
    fn groups_consecutive_search_matches_by_row() {
        let matches = [(0, 1), (0, 2), (0, 5), (1, 0), (1, 1)];

        assert_eq!(count_match_groups(&matches), 3);
        assert_eq!(find_nth_match_start(&matches, 0), Some((0, 1)));
        assert_eq!(find_nth_match_start(&matches, 1), Some((0, 5)));
        assert_eq!(find_nth_match_start(&matches, 2), Some((1, 0)));
        assert_eq!(find_nth_match_start(&matches, 3), None);
    }
}
