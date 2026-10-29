//! The Undo History tab (⌥⌘Z): every file operation since Mu opened, newest first, each of which
//! can be undone or redone on its own.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    browser::{Redo, Undo},
    fs,
    history::{self, Change, History},
    workspace::TabEvent,
};

pub fn init(cx: &mut App) {
    let context = Some("UndoHistory");
    cx.bind_keys([KeyBinding::new("cmd-z", Undo, context), KeyBinding::new("cmd-shift-z", Redo, context)]);
}

pub struct HistoryPage {
    focus_handle: FocusHandle,
    _observe: Subscription,
}

impl EventEmitter<TabEvent> for HistoryPage {}

impl HistoryPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self { focus_handle: cx.focus_handle(), _observe: cx.observe_global::<History>(|_, cx| cx.notify()) }
    }

    fn render_change(&self, change: &Change, first: bool, busy: bool, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let id = change.id;
        let current = change.current();
        // Short of room, the buttons go under the rest.
        h_flex()
            .flex_wrap()
            .gap_3()
            .px_4()
            .py_3()
            .when(!first, |row| row.border_t_1().border_color(theme.border))
            .when(change.undone, |row| row.text_color(theme.muted_foreground))
            .child(
                h_flex()
                    .flex_1()
                    .min_w(px(160.))
                    .gap_3()
                    .child(Icon::new(change.op.icon()).size(px(18.)).flex_none().text_color(theme.muted_foreground))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap_x_2()
                                    .child(
                                        div()
                                            .font_medium()
                                            .when(change.undone, |title| title.line_through())
                                            .child(change.op.name()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(fs::format_date(change.time)),
                                    ),
                            )
                            .child(div().text_sm().truncate().child(change.detail.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(change.place.clone()),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .ml_auto()
                    .gap_1()
                    .child(
                        Button::new(("show", id))
                            .ghost()
                            .small()
                            .label("Show")
                            .disabled(current.is_empty())
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(TabEvent::Reveal(current.clone())))),
                    )
                    .child(
                        Button::new(("take-back", id))
                            .small()
                            .w(px(64.))
                            .label(if change.undone { "Redo" } else { "Undo" })
                            .loading(change.running())
                            .disabled(busy)
                            .on_click(move |_, window, cx| history::toggle(id, window, cx).detach()),
                    ),
            )
    }
}

impl Focusable for HistoryPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HistoryPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = history::busy(cx);
        let rows: Vec<_> = history::changes(cx)
            .iter()
            .rev()
            .enumerate()
            .map(|(ix, change)| self.render_change(change, ix == 0, busy, cx))
            .collect();
        let theme = cx.theme();
        div()
            .id("undo-history")
            .size_full()
            .overflow_y_scroll()
            .track_focus(&self.focus_handle)
            .key_context("UndoHistory")
            .on_action(|_: &Undo, window, cx| history::step(false, window, cx).detach())
            .on_action(|_: &Redo, window, cx| history::step(true, window, cx).detach())
            .bg(theme.background)
            .child(
                v_flex()
                    .max_w(px(720.))
                    .mx_auto()
                    .p_6()
                    .gap_6()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(Icon::new(IconName::RotateCcwClock).size(px(22.)).text_color(theme.muted_foreground))
                            .child(div().text_xl().font_semibold().child("Undo History")),
                    )
                    .map(|page| {
                        if rows.is_empty() {
                            page.child(div().text_sm().text_color(theme.muted_foreground).child("Nothing to undo"))
                        } else {
                            page.child(
                                v_flex().rounded(theme.radius_lg).border_1().border_color(theme.border).children(rows),
                            )
                        }
                    }),
            )
    }
}
