use gpui::{Bounds, Pixels, Point, Size, px};

#[derive(Clone)]
pub(crate) struct DragTarget<I, T> {
    pub(crate) window_id: I,
    pub(crate) payload: T,
}

impl<I: PartialEq, T> DragTarget<I, T> {
    fn same_destination(&self, other: &Self) -> bool {
        self.window_id == other.window_id
    }
}

pub(crate) enum TargetUpdate<T> {
    Unchanged,
    Changed { previous: Option<T> },
}

pub(crate) enum DropIntent<T> {
    None,
    Cancelled,
    Reorder { group_id: String, index: usize },
    Merge { group_id: String, target: T },
    Detach { group_id: String },
}

pub(crate) struct TabDragState<I, T> {
    pending_group: Option<String>,
    start: Option<Point<Pixels>>,
    dragging_group: Option<String>,
    reorder_index: Option<usize>,
    outside: bool,
    merge_target: Option<DragTarget<I, T>>,
}

impl<I, T> Default for TabDragState<I, T> {
    fn default() -> Self {
        Self {
            pending_group: None,
            start: None,
            dragging_group: None,
            reorder_index: None,
            outside: false,
            merge_target: None,
        }
    }
}

impl<I: PartialEq, T> TabDragState<I, T> {
    pub(crate) fn begin(&mut self, group_id: String, position: Point<Pixels>) {
        self.cancel();
        self.pending_group = Some(group_id);
        self.start = Some(position);
    }

    pub(crate) fn promote_if_needed(&mut self, position: Point<Pixels>, threshold: f32) -> bool {
        if self.dragging_group.is_some() {
            return false;
        }
        let (Some(start), Some(group_id)) = (self.start, self.pending_group.as_ref()) else {
            return false;
        };
        let dx: f32 = (position.x - start.x).into();
        let dy: f32 = (position.y - start.y).into();
        if (dx * dx + dy * dy).sqrt() <= threshold {
            return false;
        }
        self.dragging_group = Some(group_id.clone());
        self.pending_group = None;
        true
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging_group.is_some()
    }

    pub(crate) fn dragging_group(&self) -> Option<&str> {
        self.dragging_group.as_deref()
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending_group.is_some()
    }

    pub(crate) fn merge_target(&self) -> Option<&DragTarget<I, T>> {
        self.merge_target.as_ref()
    }

    pub(crate) fn set_merge_target(&mut self, target: Option<DragTarget<I, T>>) -> TargetUpdate<T> {
        let unchanged = match (&self.merge_target, &target) {
            (None, None) => true,
            (Some(current), Some(next)) => current.same_destination(next),
            _ => false,
        };
        if unchanged {
            return TargetUpdate::Unchanged;
        }
        let previous = self.merge_target.take().map(|target| target.payload);
        self.merge_target = target;
        TargetUpdate::Changed { previous }
    }

    pub(crate) fn set_reorder_index(&mut self, index: Option<usize>) -> bool {
        if self.reorder_index == index {
            return false;
        }
        self.reorder_index = index;
        true
    }

    pub(crate) fn outside(&self) -> bool {
        self.outside
    }

    pub(crate) fn set_outside(&mut self, outside: bool) -> bool {
        if self.outside == outside {
            return false;
        }
        self.outside = outside;
        true
    }

    pub(crate) fn finish(&mut self) -> DropIntent<T> {
        let Some(group_id) = self.dragging_group.take() else {
            self.reset_without_target();
            self.merge_target = None;
            return DropIntent::None;
        };
        let target = self.merge_target.take();
        let reorder_index = self.reorder_index;
        let outside = self.outside;
        self.reset_without_target();

        if let Some(target) = target {
            return DropIntent::Merge {
                group_id,
                target: target.payload,
            };
        }
        if let Some(index) = reorder_index {
            return DropIntent::Reorder { group_id, index };
        }
        if outside {
            return DropIntent::Detach { group_id };
        }
        DropIntent::Cancelled
    }

    pub(crate) fn cancel(&mut self) -> Option<T> {
        let previous = self.merge_target.take().map(|target| target.payload);
        self.reset_without_target();
        previous
    }

    pub(crate) fn clear_target_if(&mut self, window_id: &I) -> Option<T> {
        if self
            .merge_target
            .as_ref()
            .is_some_and(|target| &target.window_id == window_id)
        {
            return self.merge_target.take().map(|target| target.payload);
        }
        None
    }

    fn reset_without_target(&mut self) {
        self.pending_group = None;
        self.start = None;
        self.dragging_group = None;
        self.reorder_index = None;
        self.outside = false;
    }
}

pub(crate) fn should_offer_detach(
    group_count: usize,
    cursor: Point<Pixels>,
    viewport_size: Size<Pixels>,
    tab_bar_bounds: Option<Bounds<Pixels>>,
    has_merge_target: bool,
) -> bool {
    let inside_source_window = cursor.x >= px(0.)
        && cursor.y >= px(0.)
        && cursor.x < viewport_size.width
        && cursor.y < viewport_size.height;
    let inside_tab_bar = tab_bar_bounds.is_some_and(|bounds| bounds.contains(&cursor));

    group_count > 1 && inside_source_window && !inside_tab_bar && !has_merge_target
}

pub(crate) fn reorder_index_at_x(
    dragged_group_id: &str,
    cursor_x: Pixels,
    ordered_bounds: &[(String, Bounds<Pixels>)],
) -> Option<usize> {
    if !ordered_bounds
        .iter()
        .any(|(group_id, _)| group_id == dragged_group_id)
    {
        return None;
    }

    let remaining = ordered_bounds
        .iter()
        .filter(|(group_id, _)| group_id != dragged_group_id)
        .collect::<Vec<_>>();
    Some(
        remaining
            .iter()
            .position(|(_, bounds)| cursor_x < bounds.origin.x + bounds.size.width / 2.0)
            .unwrap_or(remaining.len()),
    )
}

#[cfg(test)]
mod tests {
    use gpui::{point, px, size, Bounds};

    use super::{
        DragTarget, DropIntent, TabDragState, TargetUpdate, reorder_index_at_x,
        should_offer_detach,
    };

    #[test]
    fn drag_starts_only_after_threshold() {
        let mut state = TabDragState::<u8, &'static str>::default();
        state.begin("group-a".into(), point(px(10.), px(10.)));

        assert!(!state.promote_if_needed(point(px(13.), px(14.)), 5.0));
        assert!(state.promote_if_needed(point(px(16.), px(10.)), 5.0));
        assert!(state.is_dragging());
    }

    #[test]
    fn different_window_changes_target() {
        let mut state = TabDragState::<u8, &'static str>::default();
        let first = DragTarget {
            window_id: 1,
            payload: "window-b",
        };
        let second = DragTarget {
            window_id: 2,
            payload: "window-c",
        };

        assert!(matches!(
            state.set_merge_target(Some(first)),
            TargetUpdate::Changed { previous: None }
        ));
        assert!(matches!(
            state.set_merge_target(Some(second)),
            TargetUpdate::Changed {
                previous: Some("window-b")
            }
        ));
        assert_eq!(state.merge_target().unwrap().payload, "window-c");
    }

    #[test]
    fn merge_finish_resets_state() {
        let mut state = TabDragState::<u8, &'static str>::default();
        state.begin("group-a".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);
        state.set_merge_target(Some(DragTarget {
            window_id: 2,
            payload: "window-c",
        }));

        assert!(matches!(
            state.finish(),
            DropIntent::Merge {
                group_id,
                target: "window-c",
            } if group_id == "group-a"
        ));
        assert!(!state.is_dragging());
        assert!(state.merge_target().is_none());
        assert!(!state.outside());
    }

    #[test]
    fn invalid_release_cancels_without_detaching() {
        let mut state = TabDragState::<u8, ()>::default();
        state.begin("group-a".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);

        assert!(matches!(state.finish(), DropIntent::Cancelled));
    }

    #[test]
    fn tab_bar_release_reorders_group() {
        let mut state = TabDragState::<u8, ()>::default();
        state.begin("group-c".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);
        state.set_reorder_index(Some(0));
        state.set_outside(true);

        assert!(matches!(
            state.finish(),
            DropIntent::Reorder { group_id, index: 0 } if group_id == "group-c"
        ));
    }

    #[test]
    fn merge_takes_priority_over_reorder_and_detach() {
        let mut state = TabDragState::<u8, &'static str>::default();
        state.begin("group-a".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);
        state.set_reorder_index(Some(1));
        state.set_outside(true);
        state.set_merge_target(Some(DragTarget {
            window_id: 2,
            payload: "window-b",
        }));

        assert!(matches!(
            state.finish(),
            DropIntent::Merge {
                group_id,
                target: "window-b",
            } if group_id == "group-a"
        ));
    }

    #[test]
    fn cursor_position_computes_left_and_right_reorder_indices() {
        let bounds = vec![
            (
                "group-a".to_string(),
                Bounds::new(point(px(0.), px(0.)), size(px(100.), px(32.))),
            ),
            (
                "group-b".to_string(),
                Bounds::new(point(px(100.), px(0.)), size(px(100.), px(32.))),
            ),
            (
                "group-c".to_string(),
                Bounds::new(point(px(200.), px(0.)), size(px(100.), px(32.))),
            ),
        ];

        assert_eq!(reorder_index_at_x("group-c", px(20.), &bounds), Some(0));
        assert_eq!(reorder_index_at_x("group-a", px(280.), &bounds), Some(2));
        assert_eq!(reorder_index_at_x("missing", px(20.), &bounds), None);
    }

    #[test]
    fn completed_drag_cannot_commit_twice() {
        let mut state = TabDragState::<u8, ()>::default();
        state.begin("group-a".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);
        state.set_outside(true);

        assert!(matches!(state.finish(), DropIntent::Detach { .. }));
        assert!(matches!(state.finish(), DropIntent::None));
    }

    #[test]
    fn cancelling_reorder_keeps_finish_inert() {
        let mut state = TabDragState::<u8, ()>::default();
        state.begin("group-b".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);
        state.set_reorder_index(Some(0));

        state.cancel();
        assert!(matches!(state.finish(), DropIntent::None));
    }

    #[test]
    fn detach_is_offered_only_inside_source_window_with_multiple_groups() {
        let viewport = size(px(800.), px(600.));
        let tab_bar = Bounds::new(point(px(0.), px(0.)), size(px(800.), px(40.)));

        assert!(should_offer_detach(
            2,
            point(px(300.), px(300.)),
            viewport,
            Some(tab_bar),
            false,
        ));
        assert!(!should_offer_detach(
            1,
            point(px(300.), px(300.)),
            viewport,
            Some(tab_bar),
            false,
        ));
        assert!(!should_offer_detach(
            2,
            point(px(900.), px(300.)),
            viewport,
            Some(tab_bar),
            false,
        ));
        assert!(!should_offer_detach(
            2,
            point(px(300.), px(20.)),
            viewport,
            Some(tab_bar),
            false,
        ));
        assert!(!should_offer_detach(
            2,
            point(px(300.), px(300.)),
            viewport,
            Some(tab_bar),
            true,
        ));
    }

    #[test]
    fn single_group_can_still_merge() {
        let mut state = TabDragState::<u8, &'static str>::default();
        state.begin("group-a".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);
        state.set_outside(false);
        state.set_merge_target(Some(DragTarget {
            window_id: 2,
            payload: "window-b",
        }));

        assert!(matches!(
            state.finish(),
            DropIntent::Merge {
                group_id,
                target: "window-b",
            } if group_id == "group-a"
        ));
    }

    #[test]
    fn detach_hint_state_commits_detach_on_release() {
        let mut state = TabDragState::<u8, ()>::default();
        state.begin("group-a".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);
        state.set_outside(true);

        assert!(matches!(
            state.finish(),
            DropIntent::Detach { group_id } if group_id == "group-a"
        ));
    }
}
