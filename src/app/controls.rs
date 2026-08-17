use std::{rc::Rc, time::Duration};

use gpui::{
    App, ClipboardItem, ElementId, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Rems, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, px, rems,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Size,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    switch::Switch,
};

/// Shared application button size, matching the SSH file panel header controls.
pub(crate) const APP_BUTTON_SIZE: Size = Size::Small;

pub(crate) fn pointer_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).with_size(APP_BUTTON_SIZE).cursor_pointer()
}

/// Keep intentionally compact text readable when a view uses relative sizes.
pub(crate) fn ui_rems(size: f32) -> Rems {
    rems(size.max(0.85))
}

pub(crate) fn pointer_checkbox(id: impl Into<ElementId>) -> Checkbox {
    Checkbox::new(id).cursor_pointer()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SelectionState {
    Unchecked,
    Indeterminate,
    Checked,
}

impl SelectionState {
    pub(crate) fn from_counts(selected: usize, total: usize) -> Self {
        if total == 0 || selected == 0 {
            Self::Unchecked
        } else if selected >= total {
            Self::Checked
        } else {
            Self::Indeterminate
        }
    }

    fn next_checked(self) -> bool {
        self != Self::Checked
    }
}

type SelectionClickHandler = Rc<dyn Fn(&bool, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub(crate) struct PointerSelectionCheckbox {
    id: ElementId,
    state: SelectionState,
    disabled: bool,
    on_click: Option<SelectionClickHandler>,
}

impl PointerSelectionCheckbox {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            state: SelectionState::Unchecked,
            disabled: false,
            on_click: None,
        }
    }

    pub(crate) fn state(mut self, state: SelectionState) -> Self {
        self.state = state;
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for PointerSelectionCheckbox {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let selected = self.state != SelectionState::Unchecked;
        let border_color = if selected {
            cx.theme().primary
        } else {
            cx.theme().input
        };
        let color = if self.disabled {
            border_color.opacity(0.5)
        } else {
            border_color
        };
        let icon_color = if self.disabled {
            cx.theme().primary_foreground.opacity(0.5)
        } else {
            cx.theme().primary_foreground
        };
        let radius = cx.theme().radius.min(px(4.));
        let next_checked = self.state.next_checked();
        let on_click = self.on_click.clone();

        div()
            .id(self.id)
            .size(px(16.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(color)
            .rounded(radius)
            .bg(if selected {
                color
            } else {
                cx.theme().input_background()
            })
            .when(cx.theme().shadow && !self.disabled, |this| this.shadow_xs())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .when(!self.disabled, |this| {
                this.cursor_pointer().on_click(move |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                    if let Some(on_click) = &on_click {
                        on_click(&next_checked, window, cx);
                    }
                })
            })
            .when_some(
                match self.state {
                    SelectionState::Unchecked => None,
                    SelectionState::Indeterminate => Some(IconName::Minus),
                    SelectionState::Checked => Some(IconName::Check),
                },
                |this, icon| {
                    this.child(
                        Icon::new(icon)
                            .with_size(Size::Small)
                            .text_color(icon_color),
                    )
                },
            )
    }
}

pub(crate) fn pointer_switch(id: impl Into<ElementId>) -> Switch {
    Switch::new(id).cursor_pointer()
}

#[derive(IntoElement)]
pub(crate) struct PointerClipboard {
    id: ElementId,
    value: SharedString,
    tooltip: Option<SharedString>,
}

impl PointerClipboard {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            value: SharedString::default(),
            tooltip: None,
        }
    }

    pub(crate) fn value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = value.into();
        self
    }

    pub(crate) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

impl RenderOnce for PointerClipboard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| ClipboardState::default());
        let copied = state.read(cx).copied;

        pointer_button(self.id)
            .icon(if copied {
                IconName::Check
            } else {
                IconName::Copy
            })
            .ghost()
            .small()
            .when_some(self.tooltip, |this, tooltip| this.tooltip(tooltip))
            .when(!copied, |this| {
                let state = state.clone();
                let value = self.value.clone();
                this.on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                    state.update(cx, |state, cx| {
                        state.copied = true;
                        cx.notify();
                    });

                    let state = state.clone();
                    cx.spawn(async move |cx| {
                        cx.background_executor().timer(Duration::from_secs(2)).await;
                        state.update(cx, |state, cx| {
                            state.copied = false;
                            cx.notify();
                        });
                    })
                    .detach();
                })
            })
    }
}

#[derive(Default)]
struct ClipboardState {
    copied: bool,
}
