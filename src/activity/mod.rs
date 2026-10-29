//! The Activity tab: what the machine is doing, like Activity Monitor or btop. Live graphs of CPU,
//! GPU, memory, network and disk traffic, then the processes and listening ports.

pub mod monitor;
mod registry;

use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, Theme,
    button::{Button, ButtonVariants as _},
    chart::AreaChart,
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{PopupMenu, PopupMenuItem},
    notification::Notification,
    tab::{Tab, TabBar},
    table::{Column, ColumnSort, DataTable, TableDelegate, TableState},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    fs::format_size,
    icons,
    theme::{self, Toasts as _},
    workspace::TabEvent,
};
use monitor::{Monitor, Port, Pressure, Process, Sample, Thermal, Watch};

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Processes,
    Ports,
}

pub struct Activity {
    monitor: Entity<Monitor>,
    section: Section,
    filter: Entity<InputState>,
    processes: Entity<TableState<Processes>>,
    ports: Entity<TableState<Ports>>,
    focus_handle: FocusHandle,
    /// The room under the header as of the last frame, which decides how the graphs are laid out.
    room: Size<Pixels>,
    /// Keeps the details sampled while the tab is shown.
    watch: Option<Watch>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TabEvent> for Activity {}

impl Activity {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let monitor = Monitor::global(cx);
        let this = cx.weak_entity();
        let processes = cx.new(|cx| {
            TableState::new(Processes::new(this), window, cx)
                .row_selectable(false)
                .col_selectable(false)
                .col_movable(false)
        });
        let ports = cx.new(|cx| TableState::new(Ports::new(), window, cx).row_selectable(false).col_selectable(false));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let subscriptions = vec![
            cx.observe(&monitor, |this, _, cx| {
                if this.watch.is_some() {
                    this.refresh(cx)
                }
            }),
            cx.subscribe(&filter, |this, _, event, cx| {
                if let InputEvent::Change = event {
                    this.refresh(cx)
                }
            }),
        ];
        let mut this = Self {
            monitor,
            section: Section::Processes,
            filter,
            processes,
            ports,
            focus_handle: cx.focus_handle(),
            room: Size::default(),
            watch: None,
            _subscriptions: subscriptions,
        };
        this.refresh(cx);
        this
    }

    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if shown != self.watch.is_some() {
            self.watch = shown.then(|| self.monitor.read(cx).watch());
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let query = self.filter.read(cx).value().to_lowercase();
        let monitor = self.monitor.read(cx);
        let processes = monitor.details.processes.iter().filter(|p| p.matches(&query)).cloned().collect();
        let ports = monitor.details.ports.iter().filter(|p| p.matches(&query)).cloned().collect();
        self.processes.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            delegate.rows = processes;
            // Quit mustn't go to a process that's gone, whose pid may be another's by now, or to one
            // the filter hides.
            delegate.selected.take_if(|pid| !delegate.rows.iter().any(|process| process.pid == *pid));
            delegate.sort_rows();
            cx.notify();
        });
        self.ports.update(cx, |table, cx| {
            table.delegate_mut().rows = ports;
            cx.notify();
        });
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let monitor = self.monitor.read(cx);
        let details = &monitor.details;
        let [one, five, fifteen] = monitor.now.load;
        let muted = theme.muted_foreground;
        let mut facts = vec![
            (monitor.os.to_string(), muted),
            (monitor.cpu_brand.to_string(), muted),
            (format!("up {}", format_uptime(monitor.now.uptime)), muted),
            (format!("load {one:.2} {five:.2} {fifteen:.2}"), muted),
        ];
        if let Some(battery) = details.battery {
            let charging = if battery.charging { ", charging" } else { "" };
            let color = if battery.percent < 20 && !battery.charging { theme.red } else { muted };
            facts.push((format!("Battery {}%{charging}", battery.percent), color));
        }
        match details.pressure {
            Pressure::Normal => {}
            Pressure::Warning => facts.push(("Memory pressure high".into(), theme.yellow)),
            Pressure::Critical => facts.push(("Memory pressure critical".into(), theme.red)),
        }
        match details.thermal {
            Thermal::Nominal => {}
            Thermal::Fair => facts.push(("Running warm".into(), theme.yellow)),
            Thermal::Serious => facts.push(("Running hot, slowed down".into(), theme.red)),
            Thermal::Critical => facts.push(("Overheating".into(), theme.red)),
        }
        h_flex()
            .flex_none()
            .flex_wrap()
            .gap_x_3()
            .gap_y_1()
            .child(
                h_flex()
                    .min_w_0()
                    .gap_3()
                    .child(Icon::new(IconName::Server).size(px(22.)).flex_none().text_color(theme.accent_foreground))
                    .child(div().min_w_0().truncate().text_xl().font_semibold().child(monitor.host.clone())),
            )
            .children(
                facts
                    .into_iter()
                    .filter(|(fact, _)| !fact.is_empty())
                    .map(|(fact, color)| div().flex_none().text_sm().text_color(color).child(fact)),
            )
    }

    /// The graphs, as cards in `rows`, or with no room for those, as a strip of small meters.
    fn render_graphs(&self, rows: Option<&[usize]>, cx: &mut Context<Self>) -> Div {
        let (theme, monitor) = (cx.theme(), self.monitor.read(cx));
        let Some(rows) = rows else {
            return h_flex().flex_none().flex_wrap().gap_2().children(GRAPHS.map(|graph| graph.meter(monitor, theme)));
        };
        let mut cards = GRAPHS.map(|graph| graph.card(monitor, theme)).into_iter();
        // Full height, or shorter for the lists' sake, but never short of the least.
        v_flex().h(graphs_height(rows, CARD_HEIGHT)).min_h(graphs_height(rows, CARD_MIN_HEIGHT)).gap(CARD_GAP).children(
            rows.iter().map(|&count| {
                h_flex().flex_1().min_h_0().items_stretch().gap(CARD_GAP).children(cards.by_ref().take(count))
            }),
        )
    }

    fn render_section_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let monitor = self.monitor.read(cx);
        let counts = [monitor.details.processes.len(), monitor.details.ports.len()];
        let selected = self.processes.read(cx).delegate().selected;
        h_flex()
            .flex_none()
            .flex_wrap()
            .gap_2()
            .child(
                TabBar::new("activity-sections")
                    .underline()
                    .small()
                    .selected_index(self.section as usize)
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.section = [Section::Processes, Section::Ports][*ix];
                        cx.notify();
                    }))
                    .children(["Processes", "Ports"].into_iter().zip(counts).map(|(label, count)| {
                        Tab::new()
                            .label(label)
                            .suffix(div().text_xs().text_color(cx.theme().muted_foreground).child(count.to_string()))
                    })),
            )
            .child(
                h_flex()
                    .ml_auto()
                    .gap_2()
                    .when(self.section == Section::Processes, |el| {
                        el.when_some(selected, |el, pid| {
                            el.children(QUITS.map(|(label, signal)| {
                                Button::new(label)
                                    .ghost()
                                    .small()
                                    .label(label)
                                    .on_click(move |_, window, cx| send_signal(pid, signal, window, cx))
                            }))
                        })
                    })
                    .child(
                        div().w(px(220.)).min_w(px(120.)).child(
                            Input::new(&self.filter)
                                .small()
                                .prefix(Icon::new(IconName::Search).small().text_color(cx.theme().muted_foreground)),
                        ),
                    ),
            )
    }
}

impl Focusable for Activity {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Activity {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.section {
            Section::Processes => {
                DataTable::new(&self.processes).bordered(false).stripe(true).small().into_any_element()
            }
            Section::Ports => DataTable::new(&self.ports).bordered(false).stripe(true).small().into_any_element(),
        };
        // As many cards to a row as fit. Short of room for all five, the percentages and the traffic
        // take rows of their own, as even as they can be, not four and a lonely one.
        let rows: &[usize] = match ((self.room.width + CARD_GAP) / (CARD_WIDTH + CARD_GAP)) as usize {
            _ if self.room.width == Pixels::ZERO => &[5],
            5.. => &[5],
            3 | 4 => &[3, 2],
            2 => &[2, 1, 2],
            _ => &[1; 5],
        };
        // Cards while they leave the lists enough room, even at their least, and small meters if not.
        let cards = self.room.height == Pixels::ZERO
            || self.room.height >= graphs_height(rows, CARD_MIN_HEIGHT) + LISTS_MIN_HEIGHT;
        let activity = cx.entity();
        v_flex()
            .id("activity")
            .track_focus(&self.focus_handle)
            .size_full()
            .p_4()
            .gap_4()
            .bg(cx.theme().background)
            .child(self.render_header(cx))
            .child(
                v_flex()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .gap_4()
                    .child(self.render_graphs(cards.then_some(rows), cx))
                    .child(self.render_section_bar(cx))
                    .child(div().flex_1().when(cards, |el| el.min_h(LIST_MIN_HEIGHT)).min_h_0().child(content))
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                activity.update(cx, |this, cx| {
                                    if this.room != bounds.size {
                                        this.room = bounds.size;
                                        cx.notify();
                                    }
                                })
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
    }
}

/// How small a graph's card can get, and the room between them.
const CARD_WIDTH: Pixels = px(130.);
const CARD_HEIGHT: Pixels = px(170.);
const CARD_MIN_HEIGHT: Pixels = px(110.);
const CARD_GAP: Pixels = px(12.);
/// The least a list gets beside the cards, and with its tabs above it and the gaps between.
const LIST_MIN_HEIGHT: Pixels = px(160.);
const LISTS_MIN_HEIGHT: Pixels = px(260.);

/// How tall `rows` of cards are, at `card` each.
fn graphs_height(rows: &[usize], card: Pixels) -> Pixels {
    card * rows.len() as f32 + CARD_GAP * (rows.len() - 1) as f32
}

/// One of the graphs: a percent, or traffic both ways in bytes per second.
#[derive(Clone, Copy)]
enum Graph {
    Percent(Percent),
    /// What it's called, what it is in a sample, and what each way is called.
    Traffic(&'static str, fn(&Sample) -> (u64, u64), [&'static str; 2]),
}

/// The graphs, in order.
const GRAPHS: [Graph; 5] = [
    Graph::Percent(Percent::Cpu),
    Graph::Percent(Percent::Gpu),
    Graph::Percent(Percent::Memory),
    Graph::Traffic("Network", |sample| (sample.received, sample.transmitted), ["In", "Out"]),
    Graph::Traffic("Disk", |sample| (sample.read, sample.written), ["Read", "Write"]),
];

impl Graph {
    fn title(self) -> &'static str {
        match self {
            Graph::Percent(percent) => percent.title(),
            Graph::Traffic(title, ..) => title,
        }
    }

    /// Its chart of the samples, filled in fading toward the bottom.
    fn chart(self, monitor: &Monitor, theme: &Theme) -> Chart {
        let samples = monitor.history.iter().copied();
        match self {
            Graph::Percent(percent) => percent.chart(samples, theme).fill(fade(percent.color(theme))),
            Graph::Traffic(title, value, _) => {
                let (incoming, outgoing) = traffic_colors(theme);
                // Both ways on one scale, topped by the busiest second in view.
                let busiest = monitor.history.iter().map(|sample| {
                    let (incoming, outgoing) = value(sample);
                    incoming.max(outgoing)
                });
                let peak = busiest.max().unwrap_or(0).max(64 * 1024);
                chart(samples, title, incoming)
                    .fill(fade(incoming))
                    .y(move |(_, sample)| value(sample).0 as f64)
                    .stroke(outgoing)
                    .fill(fade(outgoing))
                    .linear()
                    .y(move |(_, sample)| value(sample).1 as f64)
                    .y_domain(0., peak as f64 * 1.1)
            }
        }
    }

    /// The graph as a small meter: its title, its chart and what it reads now, in a row.
    fn meter(self, monitor: &Monitor, theme: &Theme) -> Div {
        let latest = monitor.latest();
        let reading = match self {
            Graph::Percent(percent) => percent.reading(&latest, theme),
            Graph::Traffic(_, value, _) => {
                let (incoming, outgoing) = rates(value(&latest), theme);
                h_flex().gap_2().child(incoming).child(outgoing)
            }
        };
        theme::card(theme)
            .h_flex()
            .flex_1()
            .h(px(32.))
            .px_3()
            .gap_2()
            .text_xs()
            .whitespace_nowrap()
            .child(div().text_color(theme.muted_foreground).child(self.title()))
            .child(div().flex_1().min_w(px(24.)).h(px(14.)).child(self.chart(monitor, theme).y_padding(1., 0.)))
            .child(reading)
    }

    /// The graph as a card: its title above its chart, beside what it reads now (or for traffic,
    /// which color is which way), and more below the chart.
    fn card(self, monitor: &Monitor, theme: &Theme) -> Div {
        let latest = monitor.latest();
        let (beside, below) = match self {
            Graph::Percent(percent) => (percent.reading(&latest, theme), percent.details(monitor, theme)),
            Graph::Traffic(_, value, [incoming, outgoing]) => {
                let colors = traffic_colors(theme);
                let legends = caption(theme).child(legend(incoming, colors.0)).child(legend(outgoing, colors.1));
                let (incoming, outgoing) = rates(value(&latest), theme);
                (legends, caption(theme).justify_between().child(incoming).child(outgoing).into_any_element())
            }
        };
        theme::card(theme)
            .v_flex()
            .flex_1()
            .min_w_0()
            .p_3()
            .gap_2()
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_between()
                    .gap_x_3()
                    .child(div().text_sm().text_color(theme.muted_foreground).child(self.title()))
                    .child(beside),
            )
            .child(div().flex_1().min_h_0().child(self.chart(monitor, theme)))
            .child(below)
    }
}

/// The graphs of a percent, which the status bar's meters show too.
#[derive(Clone, Copy)]
pub enum Percent {
    Cpu,
    Gpu,
    Memory,
}

impl Percent {
    pub const ALL: [Percent; 3] = [Percent::Cpu, Percent::Gpu, Percent::Memory];

    fn title(self) -> &'static str {
        match self {
            Percent::Cpu => "CPU",
            Percent::Gpu => "GPU",
            Percent::Memory => "Memory",
        }
    }

    pub fn color(self, theme: &Theme) -> Hsla {
        match self {
            Percent::Cpu => theme.green,
            Percent::Gpu => theme.yellow,
            Percent::Memory => theme.magenta,
        }
    }

    fn of(self, sample: &Sample) -> f32 {
        match self {
            Percent::Cpu => sample.cpu,
            Percent::Gpu => sample.gpu,
            Percent::Memory => sample.memory,
        }
    }

    /// Its chart of `samples`, from 0 to 100%, for the caller to fill.
    pub fn chart(self, samples: impl IntoIterator<Item = Sample>, theme: &Theme) -> Chart {
        chart(samples, self.title(), self.color(theme)).y(move |(_, sample)| self.of(sample) as f64).y_domain(0., 100.)
    }

    /// What it reads in `sample`, like 12%.
    fn reading(self, sample: &Sample, theme: &Theme) -> Div {
        div().text_sm().font_semibold().text_color(self.color(theme)).child(format!("{:.0}%", self.of(sample)))
    }

    /// What its card shows below the chart: each core's load for the CPU, and the memory in use for
    /// the others.
    fn details(self, monitor: &Monitor, theme: &Theme) -> AnyElement {
        match self {
            Percent::Cpu => core_bars(&monitor.now.cores, theme).into_any_element(),
            Percent::Gpu => caption(theme)
                .justify_between()
                .when_some(monitor.gpu_cores, |el, cores| el.child(format!("{cores} cores")))
                .child(format!("{} in use", format_size(monitor.now.gpu_memory)))
                .into_any_element(),
            Percent::Memory => {
                let ((used, total), (swap_used, swap_total)) = (monitor.now.memory, monitor.now.swap);
                caption(theme)
                    .justify_between()
                    .child(format!("{} of {}", format_size(used), format_size(total)))
                    .when(swap_total > 0, |el| el.child(format!("Swap {}", format_size(swap_used))))
                    .into_any_element()
            }
        }
    }
}

type Chart = AreaChart<(usize, Sample), SharedString, f64>;

/// A chart of `samples`, oldest first, that only shows the trend: no axes, grid or tooltip. Add a `y`
/// for each series.
fn chart(samples: impl IntoIterator<Item = Sample>, id: &'static str, color: Hsla) -> Chart {
    AreaChart::new(samples.into_iter().enumerate())
        .id(id)
        .x(|(ix, _)| SharedString::from(ix.to_string()))
        .x_axis(false)
        .y_axis(false)
        .grid(false)
        .appear(false)
        .interactive(false)
        .stroke(color)
        .linear()
}

/// A chart's fill in `color`, fading toward the bottom.
fn fade(color: Hsla) -> Background {
    linear_gradient(0., linear_color_stop(color.opacity(0.35), 1.), linear_color_stop(color.opacity(0.02), 0.))
}

/// The colors of traffic coming in and going out.
fn traffic_colors(theme: &Theme) -> (Hsla, Hsla) {
    (theme.cyan, theme.red)
}

/// Bytes per second coming in and going out, each in its color.
fn rates((incoming, outgoing): (u64, u64), theme: &Theme) -> (Div, Div) {
    let colors = traffic_colors(theme);
    let rate = |bytes: u64, color: Hsla| div().text_color(color).child(format!("{}/s", format_size(bytes)));
    (rate(incoming, colors.0), rate(outgoing, colors.1))
}

fn caption(theme: &Theme) -> Div {
    h_flex().flex_wrap().gap_x_3().text_xs().text_color(theme.muted_foreground)
}

fn legend(label: &'static str, color: Hsla) -> impl IntoElement {
    h_flex().gap_1().child(div().size(px(8.)).rounded_full().bg(color)).child(label)
}

/// One small bar per core, like btop's per-core meters.
fn core_bars(cores: &[f32], theme: &Theme) -> impl IntoElement {
    h_flex().h(px(18.)).gap_0p5().items_end().children(cores.iter().map(|usage| {
        let color = load_color(*usage, theme);
        div()
            .flex_1()
            .h_full()
            .rounded_sm()
            .bg(theme.border)
            .flex()
            .flex_col()
            .justify_end()
            .child(div().w_full().h(relative(usage.clamp(2., 100.) / 100.)).rounded_sm().bg(color))
    }))
}

fn load_color(percent: f32, theme: &Theme) -> Hsla {
    match percent {
        p if p >= 80. => theme.red,
        p if p >= 50. => theme.yellow,
        _ => theme.green,
    }
}

fn format_uptime(seconds: u64) -> String {
    let (days, hours, minutes) = (seconds / 86_400, seconds / 3600 % 24, seconds / 60 % 60);
    match days {
        0 => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// Quit, which asks a process to quit, and Force Quit, which stops it: what each is called and the
/// signal it sends.
const QUITS: [(&str, i32); 2] = [("Quit", libc::SIGTERM), ("Force Quit", libc::SIGKILL)];

/// Adds Quit and Force Quit for `pid` to `menu`, with `suffix` after each, like " Process".
fn quit_items(menu: PopupMenu, pid: u32, suffix: &str) -> PopupMenu {
    QUITS.into_iter().fold(menu, |menu, (label, signal)| {
        menu.item(
            PopupMenuItem::new(format!("{label}{suffix}"))
                .on_click(move |_, window, cx| send_signal(pid, signal, window, cx)),
        )
    })
}

fn send_signal(pid: u32, signal: i32, window: &mut Window, cx: &mut App) {
    if let Err(error) = monitor::signal(pid, signal) {
        window.toast(Notification::error(format!("Couldn't stop process {pid}: {error}")), cx);
    }
}

impl Process {
    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.name.to_lowercase().contains(query)
            || self.command.to_lowercase().contains(query)
            || self.user.to_lowercase().contains(query)
            || self.pid.to_string() == query
    }
}

/// The app a program belongs to: the outermost `.app` on its path, so a helper inside an app's
/// bundle gets that app's icon.
fn app_bundle(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().filter(|path| path.extension().is_some_and(|ext| ext == "app")).last().map(Path::to_path_buf)
}

impl Port {
    fn matches(&self, query: &str) -> bool {
        query.is_empty() || self.process.to_lowercase().contains(query) || self.port.to_string().contains(query)
    }
}

struct Processes {
    rows: Vec<Process>,
    columns: Vec<Column>,
    sort: (usize, ColumnSort),
    /// Selection follows the process, not the row, as the list re-sorts.
    selected: Option<u32>,
    activity: WeakEntity<Activity>,
}

impl Processes {
    fn new(activity: WeakEntity<Activity>) -> Self {
        let columns = vec![
            Column::new("name", "Name").width(220.).sortable(),
            Column::new("pid", "PID").width(80.).sortable().text_right(),
            Column::new("user", "User").width(110.).sortable(),
            Column::new("cpu", "CPU").width(80.).sortable().text_right().sort(ColumnSort::Descending),
            Column::new("gpu", "GPU").width(80.).sortable().text_right(),
            Column::new("memory", "Memory").width(100.).sortable().text_right(),
            Column::new("disk", "Disk").width(100.).sortable().text_right(),
            Column::new("command", "Command").width(900.),
        ];
        // Sorted at first the way the one column that says so is.
        let sort = columns.iter().enumerate().find_map(|(ix, column)| match column.sort? {
            ColumnSort::Default => None,
            sort => Some((ix, sort)),
        });
        Self {
            rows: Vec::new(),
            columns,
            sort: sort.expect("a column says how to sort at first"),
            selected: None,
            activity,
        }
    }

    fn sort_rows(&mut self) {
        let (column, order) = self.sort;
        self.rows.sort_by(|a, b| {
            let ordering = match column {
                0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                1 => a.pid.cmp(&b.pid),
                2 => a.user.cmp(&b.user),
                3 => a.cpu.partial_cmp(&b.cpu).unwrap_or(Ordering::Equal),
                4 => a.gpu.partial_cmp(&b.gpu).unwrap_or(Ordering::Equal),
                5 => a.memory.cmp(&b.memory),
                _ => a.disk.cmp(&b.disk),
            };
            if order == ColumnSort::Descending { ordering.reverse() } else { ordering }
        });
    }
}

impl TableDelegate for Processes {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, ix: usize, _: &App) -> Column {
        self.columns[ix].clone()
    }

    fn perform_sort(&mut self, ix: usize, sort: ColumnSort, _: &mut Window, _: &mut Context<TableState<Self>>) {
        self.sort = (ix, if sort == ColumnSort::Default { ColumnSort::Descending } else { sort });
        self.sort_rows();
    }

    fn render_tr(&mut self, ix: usize, _: &mut Window, cx: &mut Context<TableState<Self>>) -> Stateful<Div> {
        let pid = self.rows.get(ix).map(|p| p.pid);
        div()
            .id(("process", ix))
            .when(pid.is_some() && pid == self.selected, |row| row.bg(cx.theme().table_active))
            .on_click(cx.listener(move |table, _, _, cx| {
                table.delegate_mut().selected = pid;
                if let Some(activity) = table.delegate().activity.upgrade() {
                    activity.update(cx, |_, cx| cx.notify());
                }
                cx.notify();
            }))
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let Some(process) = self.rows.get(row) else { return div() };
        let cell = div().w_full().truncate();
        match column {
            0 => {
                let icon = process.exe.as_deref().and_then(app_bundle).map(|app| icons::item(&app, true, px(16.)));
                h_flex()
                    .w_full()
                    .gap_1p5()
                    .child(div().size(px(16.)).flex_none().children(icon))
                    .child(div().min_w_0().truncate().child(process.name.clone()))
            }
            1 => cell.text_right().text_color(theme.muted_foreground).child(process.pid.to_string()),
            2 => cell.text_color(theme.muted_foreground).child(process.user.clone()),
            3 => cell.text_right().text_color(load_color(process.cpu, theme)).child(format!("{:.1}%", process.cpu)),
            4 => cell.text_right().text_color(load_color(process.gpu, theme)).child(format!("{:.1}%", process.gpu)),
            5 if process.memory == 0 => cell.text_right().text_color(theme.muted_foreground).child("—"),
            5 => cell.text_right().child(format_size(process.memory)),
            6 if process.disk == 0 => cell,
            6 => cell.text_right().child(format!("{}/s", format_size(process.disk))),
            _ => cell.text_color(theme.muted_foreground).child(process.command.clone()),
        }
    }

    fn context_menu(
        &mut self,
        row: usize,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(process) = self.rows.get(row).cloned() else { return menu };
        let pid = process.pid;
        let activity = self.activity.clone();
        quit_items(menu, pid, "")
            .separator()
            .when_some(process.exe.clone(), |menu, exe| {
                menu.item(PopupMenuItem::new("Show in Files").on_click(move |_, _, cx| {
                    activity.update(cx, |_, cx| cx.emit(TabEvent::Reveal(vec![exe.clone()]))).ok();
                }))
            })
            .item(
                PopupMenuItem::new("Copy PID")
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(pid.to_string()))),
            )
            .item(PopupMenuItem::new("Copy Command").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(process.command.to_string()))
            }))
    }
}

struct Ports {
    rows: Vec<Port>,
    columns: Vec<Column>,
}

impl Ports {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            columns: vec![
                Column::new("port", "Port").width(90.).text_right(),
                Column::new("address", "Address").width(180.),
                Column::new("process", "Process").width(220.),
                Column::new("pid", "PID").width(80.).text_right(),
            ],
        }
    }
}

impl TableDelegate for Ports {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, ix: usize, _: &App) -> Column {
        self.columns[ix].clone()
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let Some(port) = self.rows.get(row) else { return div() };
        let cell = div().w_full().truncate();
        match column {
            0 => cell.text_right().font_semibold().text_color(theme.cyan).child(port.port.to_string()),
            1 => cell.text_color(theme.muted_foreground).child(port.address.clone()),
            2 => cell.child(port.process.clone()),
            _ => cell.text_right().text_color(theme.muted_foreground).child(port.pid.to_string()),
        }
    }

    fn context_menu(
        &mut self,
        row: usize,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(port) = self.rows.get(row).cloned() else { return menu };
        let url = format!("http://localhost:{}", port.port);
        let copy_url = url.clone();
        let pid = port.pid;
        let menu = menu
            .item(PopupMenuItem::new("Open in Browser").on_click(move |_, _, cx| cx.open_url(&url)))
            .item(
                PopupMenuItem::new("Copy URL")
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()))),
            )
            .separator();
        quit_items(menu, pid, " Process")
    }
}
