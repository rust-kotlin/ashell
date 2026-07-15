use gpui::{Pixels, Point};

use super::DropZone;

#[derive(Clone)]
pub(crate) struct DragTarget<I, T> {
    pub(crate) window_id: I,
    pub(crate) payload: T,
    pub(crate) zone: DropZone,
}

impl<I: PartialEq, T> DragTarget<I, T> {
    fn same_destination(&self, other: &Self) -> bool {
        self.window_id == other.window_id && self.zone == other.zone
    }
}

pub(crate) enum TargetUpdate<T> {
    Unchanged,
    Changed { previous: Option<T> },
}

pub(crate) enum DropIntent<T> {
    None,
    Cancelled,
    Split {
        group_id: String,
        zone: DropZone,
    },
    Merge {
        group_id: String,
        target: T,
        zone: DropZone,
    },
    Detach {
        group_id: String,
    },
}

#[cfg(test)]
pub(crate) fn validate_reconnect_group(tab_count: usize) -> Result<(), &'static str> {
    match tab_count {
        1 => Ok(()),
        0 => Err("cannot move an empty group"),
        _ => Err("cannot move a group with multiple panes yet"),
    }
}

#[cfg(test)]
pub(crate) fn commit_after_prepare<T, E>(
    prepare: impl FnOnce() -> Result<T, E>,
    commit: impl FnOnce(T),
) -> Result<(), E> {
    let prepared = prepare()?;
    commit(prepared);
    Ok(())
}

pub(crate) struct TabDragState<I, T> {
    pending_group: Option<String>,
    start: Option<Point<Pixels>>,
    dragging_group: Option<String>,
    split_zone: Option<DropZone>,
    outside: bool,
    merge_target: Option<DragTarget<I, T>>,
}

impl<I, T> Default for TabDragState<I, T> {
    fn default() -> Self {
        Self {
            pending_group: None,
            start: None,
            dragging_group: None,
            split_zone: None,
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

    pub(crate) fn is_pending(&self) -> bool {
        self.pending_group.is_some()
    }

    pub(crate) fn split_zone(&self) -> Option<DropZone> {
        self.split_zone
    }

    pub(crate) fn set_split_zone(&mut self, zone: Option<DropZone>) -> bool {
        if self.split_zone == zone {
            return false;
        }
        self.split_zone = zone;
        true
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
        let split_zone = self.split_zone.take();
        let outside = self.outside;
        self.reset_without_target();

        if let Some(target) = target {
            return DropIntent::Merge {
                group_id,
                target: target.payload,
                zone: target.zone,
            };
        }
        if let Some(zone) = split_zone {
            return DropIntent::Split { group_id, zone };
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
        self.split_zone = None;
        self.outside = false;
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{point, px};

    use super::{
        DragTarget, DropIntent, TabDragState, TargetUpdate, commit_after_prepare,
        validate_reconnect_group,
    };
    use crate::DropZone;

    #[test]
    fn drag_starts_only_after_threshold() {
        let mut state = TabDragState::<u8, &'static str>::default();
        state.begin("group-a".into(), point(px(10.), px(10.)));

        assert!(!state.promote_if_needed(point(px(13.), px(14.)), 5.0));
        assert!(state.promote_if_needed(point(px(16.), px(10.)), 5.0));
        assert!(state.is_dragging());
    }

    #[test]
    fn same_zone_in_different_window_changes_target() {
        let mut state = TabDragState::<u8, &'static str>::default();
        let first = DragTarget {
            window_id: 1,
            payload: "window-b",
            zone: DropZone::Left,
        };
        let second = DragTarget {
            window_id: 2,
            payload: "window-c",
            zone: DropZone::Left,
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
    fn merge_has_priority_and_finish_resets_state() {
        let mut state = TabDragState::<u8, &'static str>::default();
        state.begin("group-a".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);
        state.set_split_zone(Some(DropZone::Right));
        state.set_merge_target(Some(DragTarget {
            window_id: 2,
            payload: "window-c",
            zone: DropZone::Down,
        }));

        assert!(matches!(
            state.finish(),
            DropIntent::Merge {
                group_id,
                target: "window-c",
                zone: DropZone::Down,
            } if group_id == "group-a"
        ));
        assert!(!state.is_dragging());
        assert!(state.merge_target().is_none());
        assert!(!state.outside());
    }

    #[test]
    fn failed_prepare_keeps_source() {
        let commits = Rc::new(Cell::new(0));
        let commit_counter = commits.clone();

        let result = commit_after_prepare(
            || Err::<(), _>("target failed"),
            move |_| commit_counter.set(commit_counter.get() + 1),
        );

        assert_eq!(result, Err("target failed"));
        assert_eq!(commits.get(), 0);
    }

    #[test]
    fn successful_prepare_commits_source_once() {
        let commits = Rc::new(Cell::new(0));
        let commit_counter = commits.clone();

        let result = commit_after_prepare(
            || Ok::<_, &'static str>("prepared target"),
            move |prepared| {
                assert_eq!(prepared, "prepared target");
                commit_counter.set(commit_counter.get() + 1);
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(commits.get(), 1);
    }

    #[test]
    fn compound_group_is_rejected_before_source_commit() {
        let commits = Rc::new(Cell::new(0));
        let commit_counter = commits.clone();

        let result = commit_after_prepare(
            || validate_reconnect_group(2),
            move |_| commit_counter.set(commit_counter.get() + 1),
        );

        assert_eq!(result, Err("cannot move a group with multiple panes yet"));
        assert_eq!(commits.get(), 0);
    }

    #[test]
    fn invalid_release_cancels_without_detaching() {
        let mut state = TabDragState::<u8, ()>::default();
        state.begin("group-a".into(), point(px(0.), px(0.)));
        state.promote_if_needed(point(px(10.), px(0.)), 5.0);

        assert!(matches!(state.finish(), DropIntent::Cancelled));
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
