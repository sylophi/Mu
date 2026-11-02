//! This Mac's overview, like About This Mac: what the Mac is, what it runs, how long it's been up,
//! and what's connected to it.

use std::ffi::CStr;

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;

use crate::{
    activity::{
        format_uptime,
        monitor::{self, Battery, Monitor},
        registry,
    },
    browser::Refresh,
    fs, theme,
};

use super::{
    devices::{self, Device, Kind},
    group, list, loading, note,
};

pub struct Overview {
    /// Its name in the store, like "Mac mini (2024)".
    model: SharedString,
    /// What it's made of and runs, as rows of names and values.
    facts: Vec<(&'static str, String)>,
    battery: Option<Battery>,
    /// Till they've been looked for.
    devices: Option<Vec<Device>>,
    monitor: Entity<Monitor>,
    focus_handle: FocusHandle,
    /// For how long it's been up.
    _monitor: Subscription,
    _load: Task<()>,
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
        let mut this = Self {
            model: model.into(),
            facts,
            battery: None,
            devices: None,
            focus_handle: cx.focus_handle(),
            _monitor: cx.observe(&monitor, |_, _, cx| cx.notify()),
            monitor,
            _load: Task::ready(()),
        };
        this.load(cx);
        this
    }

    /// Reads what changes: the battery and what's connected.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.devices = None;
        let devices = cx.background_spawn(async { (monitor::battery(), devices::connected()) });
        self._load = cx.spawn(async move |this, cx| {
            let (battery, devices) = devices.await;
            this.update(cx, |this, cx| {
                this.battery = battery;
                this.devices = Some(devices);
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn render_battery(&self, battery: Battery, cx: &App) -> impl IntoElement + use<> {
        let charging = if battery.charging { ", charging" } else { "" };
        let mut rows = vec![("Charge", format!("{}%{charging}", battery.percent))];
        rows.extend(battery.health.map(|health| ("Maximum capacity", format!("{health}%"))));
        rows.extend(battery.cycles.map(|cycles| ("Cycle count", cycles.to_string())));
        group("Battery", None, facts(rows, cx), cx)
    }

    fn render_devices(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let list = match &self.devices {
            None => loading(),
            Some(devices) if devices.is_empty() => note("Nothing's connected", cx),
            Some(devices) => list(
                devices.iter().map(|device| {
                    let icon = match device.kind {
                        Kind::Usb => IconName::Usb,
                        Kind::Thunderbolt => IconName::Zap,
                        Kind::Bluetooth => IconName::Bluetooth,
                        Kind::Display => IconName::Monitor,
                    };
                    h_flex()
                        .gap_3()
                        .child(Icon::new(icon).size(px(16.)).flex_none().text_color(theme.muted_foreground))
                        .child(div().flex_1().min_w_0().truncate().child(device.name.clone()))
                        .child(div().flex_none().text_color(theme.muted_foreground).child(device.detail.clone()))
                }),
                cx,
            ),
        };
        group("Devices", None, list, cx)
    }
}

/// Facts, a name and its value to a row.
fn facts(rows: Vec<(&'static str, String)>, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    list(
        rows.into_iter().map(|(name, value)| {
            h_flex()
                .gap_4()
                .child(div().flex_none().text_color(muted).child(name))
                .child(div().flex_1().min_w_0().text_right().truncate().child(value))
        }),
        cx,
    )
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
        div()
            .id("overview")
            .size_full()
            .overflow_y_scroll()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.load(cx)))
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
                            .child(Icon::new(IconName::Laptop).size(px(22.)).text_color(theme.muted_foreground))
                            .child(div().text_xl().font_semibold().child(self.model.clone())),
                    )
                    .child(theme::card(theme).child(facts(mac, cx)))
                    .children(self.battery.map(|battery| self.render_battery(battery, cx)))
                    .child(self.render_devices(cx)),
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
