//! The status bar: what's selected, how far copying has got, and the system meters.

use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _, Theme,
    button::{Button, ButtonVariants as _},
    h_flex,
    spinner::Spinner,
    status_bar::StatusBar,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::{
    Item, STATUS_BAR_HEIGHT, Workspace,
    actions::{ShowActivity, ToggleHidden, TogglePreview, ToggleSidebar},
    parts::{action_button, truncated},
};
use crate::{
    activity::{
        Percent,
        monitor::{HISTORY, Monitor},
    },
    fs::{self, shown_path},
    icons, operations,
    settings::Settings,
};

/// CPU, GPU and memory at a glance, in the status bar.
pub(super) struct Meters {
    monitor: Entity<Monitor>,
    _observe: Subscription,
}

impl Meters {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        let monitor = Monitor::global(cx);
        let observe = cx.observe(&monitor, |_, _, cx| cx.notify());
        Self { monitor, _observe: observe }
    }
}

impl Render for Meters {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        const POINTS: usize = 24;
        let theme = cx.theme();
        let recent = self.monitor.read(cx).history.iter().skip(HISTORY - POINTS).copied();
        h_flex()
            .id("meters")
            .h(px(20.))
            .px_1p5()
            .mr_1()
            .gap_1p5()
            .rounded(theme.radius)
            .cursor_pointer()
            .hover(|s| s.bg(theme.secondary_hover))
            .children(Percent::ALL.map(|percent| {
                div().w(px(24.)).h(px(12.)).child(
                    percent
                        .chart(recent.clone(), theme)
                        // The default headroom would be most of a chart this small.
                        .y_padding(1., 0.)
                        .fill(percent.color(theme).opacity(0.25)),
                )
            }))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowActivity), cx))
    }
}

/// How far copying has got, once it's taken long enough to wonder, with a way to stop it.
fn render_transfers(cx: &App) -> Option<impl IntoElement + use<>> {
    let transfers: Vec<_> = operations::transfers(cx)
        .iter()
        .filter(|transfer| transfer.started.elapsed() > Duration::from_millis(500) && transfer.progress.bytes().1 > 0)
        .collect();
    let verb = transfers.first()?.verb;
    let (copied, total) =
        transfers.iter().map(|transfer| transfer.progress.bytes()).fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
    let progresses: Vec<_> = transfers.iter().map(|transfer| transfer.progress.clone()).collect();
    let theme = cx.theme();
    Some(
        h_flex()
            .gap_2()
            .pr_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(format!("{verb} {} of {}", fs::format_size(copied), fs::format_size(total)))
            .child(strokes(copied as f32 / total as f32, theme))
            .child(Button::new("stop-transfer").ghost().xsmall().icon(IconName::X).tooltip("Stop").on_click(
                move |_, _, _| {
                    for progress in &progresses {
                        progress.cancel();
                    }
                },
            )),
    )
}

/// How far along something is, drawn as the four strokes of Mu's logo filling in together.
fn strokes(done: f32, theme: &Theme) -> Div {
    v_flex().w(px(80.)).gap(px(1.)).children(icons::logo_colors(theme).map(|color| {
        div()
            .h(px(1.5))
            .w_full()
            .rounded_full()
            .bg(theme.secondary_active)
            .child(div().h_full().w(relative(done.clamp(0., 1.))).rounded_full().bg(color))
    }))
}

impl Workspace {
    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (show_sidebar, show_preview, show_hidden) =
            (self.docks.sidebar, self.docks.preview, Settings::get(cx).show_hidden);
        let theme = cx.theme();
        let info = h_flex().flex_1().min_w_0().gap_2().text_xs().text_color(theme.muted_foreground);
        let info = match self.active_item() {
            Some(Item::Folder(browser)) => {
                let browser = browser.read(cx);
                let (count, selected, size) = browser.status();
                let mut summary = fs::count(count, "item", "items");
                if selected > 0 {
                    summary = format!("{selected} of {summary} selected");
                    if size > 0 {
                        summary.push_str(&format!(", {}", fs::format_size(size)));
                    }
                }
                info.when(!operations::transfers(cx).is_empty(), |el| el.child(Spinner::new().xsmall()))
                    .child(truncated(summary))
            }
            Some(Item::Terminal(terminal)) => info.child(truncated(shown_path(&terminal.read(cx).cwd()))),
            Some(Item::Editor(editor)) => info.child(editor.read(cx).cursor_label(cx)),
            _ => info,
        };
        // Short of room, the summary gives way, not the buttons.
        StatusBar::new().h(STATUS_BAR_HEIGHT).child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(
                    action_button("toggle-sidebar", IconName::PanelLeft, "Sidebar", ToggleSidebar)
                        .xsmall()
                        .selected(show_sidebar),
                )
                .child(info)
                .child(h_flex().flex_none().gap_1().children(render_transfers(cx)).child(self.meters.clone()).when(
                    self.active_folder().is_some(),
                    |el| {
                        let hidden = if show_hidden { IconName::Eye } else { IconName::EyeOff };
                        el.child(
                            action_button("toggle-hidden", hidden, "Hidden Files", ToggleHidden)
                                .xsmall()
                                .selected(show_hidden),
                        )
                        .child(
                            action_button("toggle-preview", IconName::PanelRight, "Preview", TogglePreview)
                                .xsmall()
                                .selected(show_preview),
                        )
                    },
                )),
        )
    }
}
