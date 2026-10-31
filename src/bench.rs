//! Built with the `bench` feature, Mu times how soon what it's asked for is on screen, for
//! `bench/run.py`. It opens a home folder made up for it, presses keys and clicks in its window as
//! macOS hands them over, writes how long each took to the file MU_BENCH names, then quits.

use std::{
    cell::Cell,
    path::Path,
    time::{Duration, Instant},
};

use gpui_kit::{
    App, AsyncApp, Keystroke,
    profiler::{self, FrameEvent, FrameTimingCollector},
};
use image::{Rgb, RgbImage};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSWindow, NSWindowOcclusionState};
use objc2_core_foundation::CGPoint;
use objc2_foundation::{NSProcessInfo, NSString};
use serde::Serialize;

use crate::settings::Settings;

/// What's timed, with how long it may take: its median, in milliseconds, on the slower of the Macs
/// CI runs on, with room to spare. Smaller slowdowns show against the base build instead.
const TARGETS: &[(&str, f64)] = &[
    // Within a frame at 60 Hz.
    ("Arrow keys in a list", 16.),
    ("Type in the terminal", 16.),
    ("Open the palette", 25.),
    ("Double-click a 10,000-item folder", 25.),
    ("Open a 10,000-item folder with ⌘↓", 25.),
    // Quick Look draws the pictures.
    ("Open a folder of photos in the grid", 50.),
    // From Enter until the shell is back at its prompt, with a screenful of colors drawn.
    ("Fill the terminal with colored text", 50.),
];

/// The made-up home folder: a folder of 10,000 files, four of photos, and colored text to print in
/// the terminal.
pub fn prepare() {
    let home = std::env::temp_dir().join("mu-bench");
    // Nothing remembered from a run before, like the window or the folders gone to.
    let support = home.join("Library/Application Support/Mu");
    if support.exists() {
        std::fs::remove_dir_all(&support).unwrap();
    }
    std::fs::create_dir_all(&support).unwrap();
    fill(&home.join("10k"), 10_000, |path, _| std::fs::write(path.with_extension("txt"), "").unwrap());
    for folder in 1..=4 {
        fill(&home.join(format!("photos {folder}")), 30, |path, ix| {
            // Different enough that Quick Look draws each one.
            let tint = ((folder * 30 + ix) * 37 % 256) as u8;
            let photo = RgbImage::from_fn(1200, 800, |x, y| Rgb([tint, (x / 5) as u8, (y / 4) as u8]));
            photo.save(path.with_extension("jpg")).unwrap();
        });
    }
    // A row of a different color every four characters, like a TUI's.
    let colors: String = (0..60)
        .map(|row| {
            (0..25).map(|run| format!("\x1b[38;5;{}mmu{run:02}", (row * 25 + run) % 216 + 16)).collect::<String>()
                + "\x1b[0m\n"
        })
        .collect();
    std::fs::write(home.join(".colors"), colors).unwrap();
    // Without one, zsh asks a new user to set it up.
    std::fs::write(home.join(".zshrc"), "").unwrap();
    // SAFETY: nothing else runs yet that could read the environment.
    unsafe {
        std::env::set_var("HOME", &home);
        // The terminal's shell starts through login, which sets HOME back to the real one, so zsh
        // is told where its settings are, rather than taking the real ones and keeping its history.
        std::env::set_var("SHELL", "/bin/zsh");
        std::env::set_var("ZDOTDIR", &home);
    }
}

/// Makes `count` files in `dir` with `make`, unless a run before did.
fn fill(dir: &Path, count: usize, make: impl Fn(&Path, usize)) {
    if std::fs::read_dir(dir).is_ok_and(|entries| entries.count() == count) {
        return;
    }
    _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    for ix in 0..count {
        make(&dir.join(format!("{} {:05}", dir.file_name().unwrap().display(), ix + 1)), ix);
    }
}

thread_local! {
    /// When something that comes in after the input asking for it, like a listing or a picture,
    /// last came in.
    static LANDED: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Notes that something a timing waits to see has come in, for the next frame to show.
pub fn landed() {
    LANDED.set(Some(Instant::now()));
}

#[derive(Serialize)]
struct Report {
    /// The display's refresh rate, in Hz.
    refresh_rate: f64,
    timings: Vec<Timing>,
}

#[derive(Serialize)]
struct Timing {
    name: &'static str,
    target: f64,
    /// Each in milliseconds, from the input to the frame showing what it asked for being presented.
    samples: Vec<f64>,
}

pub fn start(cx: &mut App) {
    let out = std::env::var_os("MU_BENCH").unwrap_or_else(|| fail("MU_BENCH names the file for the timings"));
    profiler::set_trace_enabled(true);
    mu_terminal::OUTPUT_LANDED.set(landed).ok();
    cx.spawn(async move |cx| {
        let mut bench = Bench { cx, frames: FrameTimingCollector::new(), refresh: None, timings: Vec::new() };
        bench.until(|| LANDED.get().is_some(), "Mu's window showed nothing").await;
        bench.settle("Mu's window", 120).await;
        let window = window();
        // In front of everything, as an app covered up draws nothing.
        window.setLevel(3);
        window.makeKeyAndOrderFront(None);
        let visible = || window.occlusionState().contains(NSWindowOcclusionState::Visible);
        bench.until(visible, "Mu's window isn't on screen. Is the display asleep?").await;
        bench.settle("Mu's window", 120).await;
        run(&mut bench).await;
        let refresh_rate = 1. / bench.refresh.unwrap_or_else(|| fail("Nothing was drawn")).as_secs_f64();
        let report = Report { refresh_rate: refresh_rate.round(), timings: bench.timings };
        std::fs::write(out, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        bench.cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn run(bench: &mut Bench<'_>) {
    // The window opens on the home folder, its items in a list, with the 10,000 files first.
    const FIRST_ROW: (f64, f64) = (320., 139.);
    for _ in 0..8 {
        bench.click(FIRST_ROW).await;
        bench
            .time_landed("Double-click a 10,000-item folder", |down| {
                mouse(if down { NSEventType::LeftMouseDown } else { NSEventType::LeftMouseUp }, FIRST_ROW, 2)
            })
            .await;
        bench.press("cmd-up").await;
        bench.press("home").await;
        bench.time_landed("Open a 10,000-item folder with ⌘↓", |down| key("cmd-down", down)).await;
        bench.press("cmd-up").await;
    }
    bench.press("cmd-down").await;
    for ix in 0..24 {
        let arrow = if ix % 4 < 2 { "down" } else { "up" };
        bench.time("Arrow keys in a list", |down| key(arrow, down)).await;
    }
    for _ in 0..6 {
        bench.time("Open the palette", |down| key("cmd-shift-p", down)).await;
        bench.press("escape").await;
    }
    // Each a first visit, from the last folder of photos to the first.
    bench.press("cmd-up").await;
    bench.cx.update(|cx| Settings::update(cx, |settings| settings.grid = true));
    bench.settle("The grid", 120).await;
    bench.press("end").await;
    for folder in 0..4 {
        if folder > 0 {
            bench.press("cmd-up").await;
            bench.press("left").await;
        }
        bench.time_landed("Open a folder of photos in the grid", |down| key("cmd-down", down)).await;
    }
    // From the home folder, where the colored text is.
    bench.press("cmd-up").await;
    LANDED.set(None);
    bench.press("ctrl-`").await;
    bench.until(|| LANDED.get().is_some(), "The shell printed nothing").await;
    // For the rest of its prompt.
    bench.settle("The shell", 120).await;
    for _ in 0..4 {
        for letter in "cat .colors".chars() {
            let letter = letter.to_string();
            bench.time_landed("Type in the terminal", |down| key(&letter, down)).await;
        }
        bench.time_landed("Fill the terminal with colored text", |down| key("enter", down)).await;
    }
}

struct Bench<'a> {
    cx: &'a mut AsyncApp,
    frames: FrameTimingCollector,
    /// The display's refresh interval, as of the last frame.
    refresh: Option<Duration>,
    timings: Vec<Timing>,
}

impl Bench<'_> {
    async fn wait(&self, ms: u64) {
        self.cx.background_executor().timer(Duration::from_millis(ms)).await;
    }

    /// Waits until `ready`, failing with `failure` if that takes over 10 seconds.
    async fn until(&self, ready: impl Fn() -> bool, failure: &str) {
        let start = Instant::now();
        while !ready() {
            if start.elapsed() > Duration::from_secs(10) {
                fail(failure);
            }
            self.wait(10).await;
        }
    }

    /// Times `input`, which asks for something to come in, like a folder's items, failing if nothing does.
    async fn time_landed(&mut self, name: &'static str, input: impl Fn(bool)) {
        if self.time(name, input).await.is_none() {
            fail(&format!("{name} showed nothing"));
        }
    }

    /// Times pressing `input`, which it's handed `true` for and then `false` to release: from the
    /// press until the frame showing what it asked for is presented, which is the first frame drawn
    /// after the last thing it waited for came in. Returns when that came in.
    async fn time(&mut self, name: &'static str, input: impl Fn(bool)) -> Option<Instant> {
        self.frames.collect_unseen();
        LANDED.set(None);
        let start = Instant::now();
        input(true);
        // Long enough for pictures that come in one after another.
        let (frames, landed) = self.settle(name, 120).await;
        let shown = landed.map_or(start, |at| at.max(start));
        let mut drawn = false;
        let presented = frames.iter().find_map(|frame| match frame {
            FrameEvent::Draw(draw) if draw.draw_start >= shown => {
                drawn = true;
                self.refresh = draw.refresh_interval.or(self.refresh);
                None
            }
            FrameEvent::Present(present) if drawn => Some(present.present_end),
            _ => None,
        });
        let presented = presented.unwrap_or_else(|| fail(&format!("{name} drew nothing")));
        let ms = presented.duration_since(start).as_secs_f64() * 1000.;
        match self.timings.iter_mut().find(|timing| timing.name == name) {
            Some(timing) => timing.samples.push(ms),
            None => {
                let target = TARGETS
                    .iter()
                    .find(|(target, _)| *target == name)
                    .unwrap_or_else(|| fail(&format!("{name} has no target")))
                    .1;
                self.timings.push(Timing { name, target, samples: vec![ms] });
            }
        }
        input(false);
        // Past a refresh at 30 Hz, so the window's idle by the next input.
        self.settle(name, 50).await;
        landed
    }

    /// Waits until nothing more comes in or gets drawn for `quiet` milliseconds, as a person would
    /// leave between one input and the next. Returns the frames drawn meanwhile, and when something
    /// last came in.
    async fn settle(&mut self, name: &str, quiet_ms: u64) -> (Vec<FrameEvent>, Option<Instant>) {
        let start = Instant::now();
        let mut frames = Vec::new();
        let mut quiet = start;
        let mut landed = LANDED.get();
        while quiet.elapsed() < Duration::from_millis(quiet_ms) {
            self.wait(10).await;
            let new = self.frames.collect_unseen();
            if !new.is_empty() || LANDED.get() != landed {
                quiet = Instant::now();
                landed = LANDED.get();
            }
            frames.extend(new);
            if start.elapsed() > Duration::from_secs(10) {
                fail(&format!("{name} never settled"));
            }
        }
        (frames, landed)
    }

    /// Presses `spec` without timing it.
    async fn press(&mut self, spec: &str) {
        key(spec, true);
        self.wait(50).await;
        key(spec, false);
        self.settle(spec, 120).await;
    }

    /// Clicks at `at` once without timing it, as the first click of a double click.
    async fn click(&self, at: (f64, f64)) {
        mouse(NSEventType::MouseMoved, at, 0);
        self.wait(100).await;
        mouse(NSEventType::LeftMouseDown, at, 1);
        self.wait(50).await;
        mouse(NSEventType::LeftMouseUp, at, 1);
        self.wait(80).await;
    }
}

/// Stops with `message`. A panic can't unwind out of AppKit's callbacks, so it would crash, and
/// macOS would show that Mu quit unexpectedly.
fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

fn window() -> objc2::rc::Retained<NSWindow> {
    let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    app.keyWindow().or_else(|| app.windows().firstObject()).unwrap()
}

/// Hands `event` to the window as AppKit hands one to the key window: a press with ⌘ or ⌃ as a
/// key equivalent first, and a move to the view under the pointer.
fn post(event: &NSEvent) {
    let window = window();
    let shortcut = event.modifierFlags().intersects(NSEventModifierFlags::Command | NSEventModifierFlags::Control);
    match event.r#type() {
        NSEventType::KeyDown if shortcut && window.performKeyEquivalent(event) => {}
        NSEventType::MouseMoved => {
            let content = window.contentView().unwrap();
            let view = content.subviews().firstObject().unwrap_or(content);
            let _: () = unsafe { objc2::msg_send![&view, mouseMoved: event] };
        }
        _ => window.sendEvent(event),
    }
}

/// A key press or release, like `down` or `cmd-shift-p`.
fn key(spec: &str, down: bool) {
    let keystroke = Keystroke::parse(spec).unwrap_or_else(|_| fail(&format!("No key {spec}")));
    let mut flags = NSEventModifierFlags::empty();
    for (held, flag) in [
        (keystroke.modifiers.platform, NSEventModifierFlags::Command),
        (keystroke.modifiers.shift, NSEventModifierFlags::Shift),
        (keystroke.modifiers.control, NSEventModifierFlags::Control),
    ] {
        flags.set(flag, held);
    }
    let name = keystroke.key.as_str();
    let (code, chars) = match name {
        "down" => (125, "\u{F701}"),
        "up" => (126, "\u{F700}"),
        "left" => (123, "\u{F702}"),
        "home" => (115, "\u{F729}"),
        "end" => (119, "\u{F72B}"),
        "escape" => (53, "\u{1b}"),
        "enter" => (36, "\r"),
        " " => (49, " "),
        "." => (47, "."),
        "`" => (50, "`"),
        letter if letter.len() == 1 && letter.as_bytes()[0].is_ascii_lowercase() => {
            // Where each letter is on the keyboard.
            let code = "asdfhgzxcv_bqweryt_____________ou_ip_lj_k____nm".find(letter).unwrap();
            (code as u16, letter)
        }
        _ => fail(&format!("No key {name}")),
    };
    if matches!(name, "down" | "up" | "left") {
        flags |= NSEventModifierFlags::Function | NSEventModifierFlags::NumericPad;
    } else if matches!(name, "home" | "end") {
        flags |= NSEventModifierFlags::Function;
    }
    let kind = if down { NSEventType::KeyDown } else { NSEventType::KeyUp };
    let chars = NSString::from_str(chars);
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        kind, CGPoint::ZERO, flags, uptime(), window().windowNumber(), None, &chars, &chars, false, code,
    )
    .unwrap();
    post(&event);
}

/// A mouse event at `at`, from the window's top left.
fn mouse(kind: NSEventType, (x, y): (f64, f64), clicks: isize) {
    let window = window();
    let height = window.frame().size.height;
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind,
        CGPoint::new(x, height - y),
        NSEventModifierFlags::empty(),
        uptime(),
        window.windowNumber(),
        None,
        0,
        clicks,
        if kind == NSEventType::LeftMouseDown { 1. } else { 0. },
    )
    .unwrap();
    post(&event);
}

fn uptime() -> f64 {
    NSProcessInfo::processInfo().systemUptime()
}
