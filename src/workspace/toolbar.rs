//! The toolbar over a folder: back, forward and up, the path bar, grid or list, the Trash and the
//! filter.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::{
    Workspace,
    actions::{ClearFilter, FocusFilter, GoBack, GoForward, GoUp, ViewAsGrid, ViewAsList},
    parts::action_button,
    sidebar::empty_trash,
};
use crate::{
    browser::{self, Browser},
    places::Places,
    settings::Settings,
};

impl Workspace {
    pub(super) fn render_toolbar(
        &self,
        browser: &Entity<Browser>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let browser = browser.read(cx);
        let (can_back, can_forward, can_up) =
            (browser.can_go_back(), browser.can_go_forward(), browser.path().parent().is_some());
        let path = browser.path().to_path_buf();
        let in_trash = browser.in_trash();
        let selected = browser.status().1 > 0;
        // Short of room, what's after the path bar goes under it.
        h_flex()
            .min_h(px(36.))
            .flex_none()
            .flex_wrap()
            .justify_end()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tab_active)
            .child(action_button("back", IconName::ArrowLeft, "Back", GoBack).small().disabled(!can_back))
            .child(action_button("forward", IconName::ArrowRight, "Forward", GoForward).small().disabled(!can_forward))
            .child(action_button("up", IconName::ArrowUp, "Enclosing Folder", GoUp).small().disabled(!can_up))
            .child({
                let workspace = cx.entity();
                div().relative().flex_1().min_w(px(120.)).ml_1().child(self.render_path_bar(&path, window, cx)).child(
                    canvas(
                        move |bounds, _, cx| {
                            workspace.update(cx, |this, cx| {
                                if this.path_bar_width != bounds.size.width {
                                    this.path_bar_width = bounds.size.width;
                                    cx.notify();
                                }
                            })
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .child({
                let grid = Settings::get(cx).grid;
                h_flex()
                    .child(action_button("view-grid", IconName::LayoutGrid, "Grid", ViewAsGrid).small().selected(grid))
                    .child(action_button("view-list", IconName::List, "List", ViewAsList).small().selected(!grid))
            })
            // In the Trash, Empty Trash takes its place.
            .when(!in_trash, |bar| {
                bar.child(
                    Button::new("trash")
                        .ghost()
                        .small()
                        .icon(IconName::Trash)
                        .tooltip_with_action("Move to Trash", &browser::Trash, Some("FileList"))
                        .disabled(!selected)
                        .on_click(cx.listener(|this, _, window, cx| this.with_folder(cx, |b, cx| b.trash(window, cx)))),
                )
            })
            .when(in_trash, |bar| {
                bar.child(
                    Button::new("empty-trash")
                        .small()
                        .label("Empty Trash")
                        .disabled(cx.global::<Places>().trash == 0)
                        .on_click(|_, window, cx| empty_trash(window, cx)),
                )
            })
            .child(self.render_filter(window, cx))
    }

    /// The filter, a button until it's wanted. It stays open while it's typed in or filters.
    fn render_filter(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let filter = self.filter.read(cx);
        if filter.value().is_empty() && !filter.focus_handle(cx).is_focused(window) {
            return action_button("filter", IconName::Search, "Filter", FocusFilter).small().into_any_element();
        }
        // Splits the room with the path bar, up to its full width.
        div()
            .flex_1()
            .min_w(px(100.))
            .max_w(px(220.))
            .key_context("Filter")
            .on_action(cx.listener(|this, _: &ClearFilter, window, cx| {
                this.reset_filter(window, cx);
                this.focus_active(window, cx);
            }))
            .child(
                Input::new(&self.filter)
                    .small()
                    .prefix(Icon::new(IconName::Search).small().text_color(cx.theme().muted_foreground)),
            )
            .into_any_element()
    }

    pub(super) fn reset_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.filter.read(cx).value().is_empty() {
            // `set_value` focuses the input, so hand focus back afterwards.
            let focused = window.focused(cx);
            self.filter.update(cx, |filter, cx| filter.set_value("", window, cx));
            if let Some(focused) = focused {
                focused.focus(window, cx);
            }
        }
        if let Some(browser) = self.active_folder() {
            browser.update(cx, |browser, cx| browser.set_filter("".into(), cx));
        }
    }
}
