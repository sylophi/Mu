//! The status bar: what's selected, how far copying has got, and the system meters.

use std::{rc::Rc, time::Duration};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, Theme,
    button::{Button, ButtonVariants as _},
    h_flex,
    popover::Popover,
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
    fs::{
        self, shown_path,
        underway::{Direction, Transfer},
    },
    icons, operations,
    settings::Settings,
    underway::{self, Underway},
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

/// Mu's operations that have taken long enough to wonder how far they've got.
fn operations(cx: &App) -> Vec<&operations::Transfer> {
    operations::transfers(cx)
        .iter()
        .filter(|transfer| transfer.started.elapsed() > Duration::from_millis(500) && transfer.progress.bytes().1 > 0)
        .collect()
}

/// What other apps have underway where Mu's watching. Mu's own copies show as its operations.
fn others(cx: &App) -> Vec<Rc<Transfer>> {
    Underway::global(cx).read(cx).all().into_iter().filter(|transfer| !transfer.is_mu()).collect()
}

/// Everything underway at a glance: how many go up, down or are being copied, how much is left,
/// and how far it's all got. It opens the list of all of it.
fn render_transfers(cx: &App) -> Option<impl IntoElement + use<>> {
    let (operations, others) = (operations(cx), others(cx));
    if operations.is_empty() && others.is_empty() {
        return None;
    }
    let count = |direction| others.iter().filter(|transfer| transfer.direction == direction).count();
    let copies = count(Direction::Copy) + operations.len();
    let (done, total) = operations
        .iter()
        .map(|operation| operation.progress.bytes())
        .chain(others.iter().filter_map(|transfer| transfer.bytes()))
        .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
    let theme = cx.theme();
    let counts = [
        (IconName::ArrowUp, count(Direction::Upload)),
        (IconName::ArrowDown, count(Direction::Download)),
        (IconName::Copy, copies),
    ];
    let pill = Button::new("transfers").ghost().xsmall().child(
        h_flex()
            .gap_2()
            .text_xs()
            .children(
                counts
                    .into_iter()
                    .filter(|(_, count)| *count > 0)
                    .map(|(icon, count)| h_flex().gap_0p5().child(Icon::new(icon).xsmall()).child(count.to_string())),
            )
            .when(total > 0, |el| {
                el.child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} left", fs::format_size(total.saturating_sub(done)))),
                )
                .child(strokes(done as f32 / total as f32, theme))
            }),
    );
    Some(
        Popover::new("transfers-list")
            .anchor(Anchor::BottomRight)
            .trigger(pill)
            .content(|_, _, cx| render_transfer_list(cx)),
    )
}

/// All that's underway, by who's doing it, each with how far it's got and a way to stop it.
fn render_transfer_list(cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let group = |title: SharedString, summary: Option<String>| {
        h_flex()
            .mt_2()
            .mb_0p5()
            .px_2()
            .justify_between()
            .text_xs()
            .font_medium()
            .text_color(theme.muted_foreground)
            .child(title)
            .children(summary)
    };
    let mut list = v_flex().w(px(320.)).max_h(px(420.)).text_sm();
    let operations = operations(cx);
    if !operations.is_empty() {
        list =
            list.child(group("Mu".into(), None)).children(operations.into_iter().enumerate().map(|(ix, operation)| {
                let progress = operation.progress.clone();
                let (done, total) = progress.bytes();
                job(
                    SharedString::from(format!("operation-{ix}")),
                    Icon::new(IconName::Copy).size(JOB_ICON).text_color(theme.muted_foreground).into_any_element(),
                    operation.verb.into(),
                    Some(done as f64 / total as f64),
                    underway::of(done, total),
                    Some(move || progress.cancel()),
                    theme,
                )
            }));
    }
    let mut others = others(cx);
    // What can't be told who's doing it goes last.
    others.sort_by_key(|transfer| (transfer.by.is_none(), transfer.by));
    for transfers in others.chunk_by(|a, b| a.by == b.by) {
        let by = transfers[0].by.unwrap_or("Other Apps");
        let count = |direction| transfers.iter().filter(|transfer| transfer.direction == direction).count();
        let summary = [("↑", count(Direction::Upload)), ("↓", count(Direction::Download))]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(arrow, count)| format!("{arrow} {count}"))
            .collect::<Vec<_>>()
            .join(" ");
        list = list.child(group(by.into(), (!summary.is_empty()).then_some(summary))).children(transfers.iter().map(
            |transfer| {
                let path = transfer.path.clone();
                let picture = {
                    let path = path.clone();
                    move || icons::item(&path, path.is_dir(), JOB_ICON).into_any_element()
                };
                let stop = transfer.can_stop().then(|| {
                    let transfer = transfer.clone();
                    move || transfer.stop()
                });
                job(
                    SharedString::from(path.to_string_lossy().into_owned()),
                    underway::filling(picture, JOB_ICON, transfer.fraction()).into_any_element(),
                    fs::display_name(&path).into(),
                    transfer.fraction(),
                    underway::amount(transfer).unwrap_or_else(|| underway::status(transfer)),
                    stop,
                    theme,
                )
            },
        ));
    }
    list
}

/// The size of the icons in the list of what's underway.
const JOB_ICON: Pixels = px(22.);

/// A row in the list of what's underway.
fn job(
    id: SharedString,
    icon: AnyElement,
    title: SharedString,
    done: Option<f64>,
    amount: String,
    stop: Option<impl Fn() + 'static>,
    theme: &Theme,
) -> Stateful<Div> {
    let bar = underway::bar("bar", done).flex_1();
    h_flex()
        .id(id)
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded(theme.radius)
        .child(icon)
        .child(
            v_flex().flex_1().min_w_0().gap_0p5().child(truncated(title)).child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(bar)
                    .child(div().flex_none().child(amount)),
            ),
        )
        .children(stop.map(|stop| {
            Button::new("stop").ghost().xsmall().icon(IconName::CircleX).tooltip("Stop").on_click(move |_, _, _| stop())
        }))
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
