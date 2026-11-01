//! This Mac's overview, like About This Mac: what the Mac is, what it runs, and how long it's been
//! up.

use std::ffi::CStr;

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, StyledExt as _, h_flex, v_flex};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    activity::{
        format_uptime,
        monitor::{self, Battery, Monitor},
        registry,
    },
    fs,
};

pub struct Overview {
    /// Its name in the store, like "Mac mini (2024)".
    model: SharedString,
    /// What it's made of and runs, as rows of names and values.
    facts: Vec<(&'static str, String)>,
    battery: Option<Battery>,
    monitor: Entity<Monitor>,
    focus_handle: FocusHandle,
    /// For how long it's been up.
    _monitor: Subscription,
}

impl Overview {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let monitor = Monitor::global(cx);
        let (host, chip, gpu_cores) = {
            let monitor = monitor.read(cx);
            (monitor.host.to_string(), monitor.cpu_brand.to_string(), monitor.gpu_cores)
        };
        let model = registry::at(c"IODeviceTree:/product")
            .and_then(|product| product.get("product-name")?.text())
            .or_else(|| sysctl_text(c"hw.model"))
            .unwrap_or_else(|| "Mac".into());
        let version = sysctl_text(c"kern.osproductversion").unwrap_or_default();
        let build = sysctl_text(c"kern.osversion").map(|build| format!(" ({build})")).unwrap_or_default();
        let mut facts = vec![("Name", host), ("Chip", chip)];
        facts.extend(cores().map(|cores| ("CPU", cores)));
        facts.extend(gpu_cores.map(|cores| ("GPU", fs::count(cores as usize, "core", "cores"))));
        // In gigabytes of 2³⁰ bytes, as Apple counts memory.
        facts.extend(sysctl_number(c"hw.memsize").map(|bytes| ("Memory", format!("{} GB", bytes >> 30))));
        facts.push(("Software", format!("macOS {version}{build}")));
        Self {
            model: model.into(),
            facts,
            battery: monitor::battery(),
            focus_handle: cx.focus_handle(),
            _monitor: cx.observe(&monitor, |_, _, cx| cx.notify()),
            monitor,
        }
    }

    fn render_battery(&self, battery: Battery, cx: &App) -> impl IntoElement + use<> {
        let charging = if battery.charging { ", charging" } else { "" };
        let mut rows = vec![("Charge", format!("{}%{charging}", battery.percent))];
        rows.extend(battery.health.map(|health| ("Maximum capacity", format!("{health}%"))));
        rows.extend(battery.cycles.map(|cycles| ("Cycle count", cycles.to_string())));
        group("Battery", rows, cx)
    }
}

/// A heading over a card of what it's about.
fn group(heading: &'static str, rows: Vec<(&'static str, String)>, cx: &App) -> impl IntoElement + use<> {
    v_flex().gap_2().child(div().font_semibold().child(heading)).child(card(rows, cx))
}

/// A card of facts, a name and its value to a row.
fn card(rows: Vec<(&'static str, String)>, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex().rounded(theme.radius_lg).border_1().border_color(theme.border).children(rows.into_iter().enumerate().map(
        |(ix, (name, value))| {
            h_flex()
                .gap_4()
                .px_4()
                .py_2()
                .text_sm()
                .when(ix > 0, |row| row.border_t_1().border_color(theme.border))
                .child(div().flex_none().text_color(theme.muted_foreground).child(name))
                .child(div().flex_1().min_w_0().text_right().truncate().child(value))
        },
    ))
}

impl Focusable for Overview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Overview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut mac = self.facts.clone();
        mac.push(("Up for", format_uptime(self.monitor.read(cx).now.uptime)));
        div().id("overview").size_full().overflow_y_scroll().track_focus(&self.focus_handle).bg(theme.background).child(
            v_flex()
                .max_w(px(720.))
                .mx_auto()
                .p_6()
                .gap_6()
                .child(
                    h_flex()
                        .gap_3()
                        .child(Icon::new(IconName::Laptop).size(px(22.)).text_color(theme.muted_foreground))
                        .child(div().text_xl().font_semibold().child(self.model.clone())),
                )
                .child(card(mac, cx))
                .children(self.battery.map(|battery| self.render_battery(battery, cx))),
        )
    }
}

/// The CPU's cores, as many of each kind as it has: "10 cores, 4 performance and 6 efficiency".
fn cores() -> Option<String> {
    let total = sysctl_number(c"hw.physicalcpu")?;
    let levels = sysctl_number(c"hw.nperflevels").unwrap_or(1);
    let kinds: Vec<_> = (0..levels)
        .filter_map(|level| {
            let count = sysctl_number(&std::ffi::CString::new(format!("hw.perflevel{level}.physicalcpu")).ok()?)?;
            let name = sysctl_text(&std::ffi::CString::new(format!("hw.perflevel{level}.name")).ok()?)?;
            Some(format!("{count} {}", name.to_lowercase()))
        })
        .collect();
    let total = fs::count(total as usize, "core", "cores");
    Some(match kinds.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{total}, {} and {last}", rest.join(", ")),
        _ => total,
    })
}

fn sysctl_number(name: &CStr) -> Option<u64> {
    // Some are 32 bits and some 64, and this is little-endian.
    let (mut value, mut size) = (0u64, size_of::<u64>());
    let done =
        unsafe { libc::sysctlbyname(name.as_ptr(), (&raw mut value).cast(), &mut size, std::ptr::null_mut(), 0) };
    (done == 0).then_some(value)
}

fn sysctl_text(name: &CStr) -> Option<String> {
    let mut size = 0;
    unsafe { libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) };
    let mut bytes = vec![0u8; size];
    let done =
        unsafe { libc::sysctlbyname(name.as_ptr(), bytes.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
    (done == 0).then(|| String::from_utf8_lossy(&bytes[..size]).trim_end_matches('\0').to_owned())
}
