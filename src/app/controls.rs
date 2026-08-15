use std::time::Duration;

use gpui::{
    App, ClipboardItem, ElementId, IntoElement, RenderOnce, SharedString, Styled as _, Window,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    switch::Switch,
};

pub(crate) fn pointer_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer()
}

pub(crate) fn pointer_checkbox(id: impl Into<ElementId>) -> Checkbox {
    Checkbox::new(id).cursor_pointer()
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
            .xsmall()
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
