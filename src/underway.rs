//! What's being copied, downloaded or uploaded, for all of Mu: in the folders its tabs show, and
//! in Downloads, Desktop and Documents, where things usually arrive. The listing, the sidebar, the
//! preview and the status bar all show it from here.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    time::{Duration, Instant},
};

use gpui_kit::{
    component::{
        Sizable as _,
        plot::shape::{self, ArcData},
        progress::Progress,
    },
    *,
};

use crate::fs::{
    self, home_dir,
    underway::{Direction, Heard, Subscription, Transfer, subscribe},
};

/// How long what just finished shows as having landed.
pub const LANDED: Duration = Duration::from_millis(1500);

pub fn init(cx: &mut App) {
    let underway = cx.new(Underway::new);
    cx.set_global(GlobalUnderway(underway));
}

struct GlobalUnderway(Entity<Underway>);

impl Global for GlobalUnderway {}

pub struct Underway {
    shared: Rc<Shared>,
    /// What finished a moment ago, and when.
    landed: Vec<(PathBuf, Instant)>,
    _usual: Watch,
    _hear: Task<()>,
}

/// What the subscriptions keep up to date, on the main thread.
struct Shared {
    transfers: RefCell<Vec<Rc<Transfer>>>,
    /// The folders watched, how many watches want each, and what hears from it.
    watched: RefCell<HashMap<PathBuf, (usize, Subscription)>>,
    heard: smol::channel::Sender<Heard>,
}

/// That something stopped for an item, which may look different now, like a download that's done
/// or an upload that's no longer waiting.
pub struct Stopped(pub Rc<Transfer>);

impl EventEmitter<Stopped> for Underway {}

impl Underway {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalUnderway>().0.clone()
    }

    fn new(cx: &mut Context<Self>) -> Self {
        let (tx, rx) = smol::channel::unbounded();
        let shared = Rc::new(Shared { transfers: RefCell::default(), watched: RefCell::default(), heard: tx });
        let home = home_dir();
        let usual = watch(&shared, ["Downloads", "Desktop", "Documents"].map(|dir| home.join(dir)).to_vec());
        Self {
            shared,
            landed: Vec::new(),
            _usual: usual,
            _hear: cx.spawn(async move |this, cx| hear(this, rx, cx).await),
        }
    }

    /// Hears what's underway for the items in `dirs`, until the watch is dropped.
    pub fn watch(&self, dirs: Vec<PathBuf>) -> Watch {
        watch(&self.shared, dirs)
    }

    /// What's underway everywhere watched.
    pub fn all(&self) -> Vec<Rc<Transfer>> {
        self.shared.transfers.borrow().clone()
    }

    /// What's underway for the item at `path`.
    pub fn get(&self, path: &Path) -> Option<Rc<Transfer>> {
        self.shared.transfers.borrow().iter().find(|transfer| transfer.path == path).cloned()
    }

    /// What's underway for items in the folder at `path`, all the way down.
    pub fn inside(&self, path: &Path) -> Vec<Rc<Transfer>> {
        let transfers = self.shared.transfers.borrow();
        transfers.iter().filter(|transfer| transfer.path != path && transfer.path.starts_with(path)).cloned().collect()
    }

    /// Whether what was underway for the item at `path` finished a moment ago.
    pub fn landed(&self, path: &Path) -> bool {
        self.landed.iter().any(|(landed, _)| landed == path)
    }

    /// Whether anything is underway, or just landed, for the items `watch` hears of.
    pub fn any_for(&self, watch: &Watch) -> bool {
        let heard = |path: &Path| path.parent().is_some_and(|dir| watch.dirs.iter().any(|watched| watched == dir));
        self.shared.transfers.borrow().iter().any(|transfer| heard(&transfer.path))
            || self.landed.iter().any(|(landed, _)| heard(landed))
    }
}

/// Keeps up with what the subscriptions hear, and draws again four times a second while anything's
/// underway, as it goes along.
async fn hear(this: WeakEntity<Underway>, rx: smol::channel::Receiver<Heard>, cx: &mut AsyncApp) {
    loop {
        let busy = this.update(cx, |this, cx| {
            this.landed.retain(|(_, at)| at.elapsed() < LANDED);
            cx.notify();
            !this.shared.transfers.borrow().is_empty() || !this.landed.is_empty()
        });
        let Ok(busy) = busy else { return };
        let timer = cx.background_executor().timer(Duration::from_millis(250));
        let tick = async {
            timer.await;
            None
        };
        let heard = match busy {
            true => smol::future::or(async { Some(rx.recv().await) }, tick).await,
            false => Some(rx.recv().await),
        };
        let Some(heard) = heard else { continue };
        let Ok(heard) = heard else { return };
        if let Heard::Stopped(transfer) = heard {
            let updated = this.update(cx, |this, cx| {
                if transfer.finished() {
                    this.landed.push((transfer.path.clone(), Instant::now()));
                }
                cx.emit(Stopped(transfer.clone()));
            });
            if updated.is_err() {
                return;
            }
        }
    }
}

/// Watches `dirs`, sharing what already watches any of them.
fn watch(shared: &Rc<Shared>, dirs: Vec<PathBuf>) -> Watch {
    let mut watched = shared.watched.borrow_mut();
    for dir in &dirs {
        if let Some((count, _)) = watched.get_mut(dir) {
            *count += 1;
            continue;
        }
        let weak = Rc::downgrade(shared);
        let subscription = subscribe(dir, move |heard| {
            let Some(shared) = weak.upgrade() else { return };
            let mut transfers = shared.transfers.borrow_mut();
            match &heard {
                Heard::Started(transfer) => transfers.push(transfer.clone()),
                Heard::Stopped(transfer) => transfers.retain(|other| !Rc::ptr_eq(other, transfer)),
            }
            drop(transfers);
            shared.heard.try_send(heard).ok();
        });
        watched.insert(dir.clone(), (1, subscription));
    }
    Watch { dirs, shared: Rc::downgrade(shared) }
}

/// Folders watched for what's underway in them, until it's dropped.
pub struct Watch {
    dirs: Vec<PathBuf>,
    shared: Weak<Shared>,
}

impl Watch {
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let Some(shared) = self.shared.upgrade() else { return };
        let mut watched = shared.watched.borrow_mut();
        for dir in &self.dirs {
            let Some((count, _)) = watched.get_mut(dir) else { continue };
            *count -= 1;
            if *count == 0 {
                watched.remove(dir);
                // Nobody hears how they end now.
                shared.transfers.borrow_mut().retain(|transfer| transfer.path.parent() != Some(dir));
            }
        }
    }
}

/// What's happening to an item, like "Downloading, 2 min left".
pub fn status(transfer: &Transfer) -> String {
    let verb = verb(transfer.direction);
    match transfer.time_left() {
        Some(left) => format!("{verb}, {} left", duration(left)),
        None => verb.to_string(),
    }
}

pub fn verb(direction: Direction) -> &'static str {
    match direction {
        Direction::Upload => "Uploading",
        Direction::Download => "Downloading",
        Direction::Copy => "Copying",
    }
}

/// How far along it is, short, like "↓ 40%" or "Copying 20%".
pub fn progress(transfer: &Transfer) -> String {
    let direction = transfer.direction;
    let Some(fraction) = transfer.fraction() else { return verb(direction).to_string() };
    let percent = (fraction * 100.) as u32;
    match direction {
        Direction::Upload => format!("↑ {percent}%"),
        Direction::Download => format!("↓ {percent}%"),
        Direction::Copy => format!("Copying {percent}%"),
    }
}

/// What several items in a folder are going through, like "Uploading 3 items".
pub fn summary(transfers: &[Rc<Transfer>]) -> String {
    let first = transfers[0].direction;
    let verb = match transfers.iter().all(|transfer| transfer.direction == first) {
        true => verb(first),
        false => "Syncing",
    };
    format!("{verb} {}", fs::count(transfers.len(), "item", "items"))
}

/// How much of it is done, like "160 of 412 MB".
pub fn amount(transfer: &Transfer) -> Option<String> {
    let (done, total) = transfer.bytes()?;
    Some(of(done, total))
}

/// `done` bytes of `total`, the unit said once when they share it.
pub fn of(done: u64, total: u64) -> String {
    let (done, total) = (fs::format_size(done), fs::format_size(total));
    match (done.rsplit_once(' '), total.rsplit_once(' ')) {
        (Some((number, unit)), Some((_, total_unit))) if unit == total_unit => format!("{number} of {total}"),
        _ => format!("{done} of {total}"),
    }
}

/// A rough length of time, like "2 min" or "15 s".
pub fn duration(time: Duration) -> String {
    match time.as_secs() {
        seconds @ 0..60 => format!("{} s", seconds.max(1)),
        seconds @ 60..5400 => format!("{} min", (seconds + 30) / 60),
        seconds => format!("{} h", (seconds + 1800) / 3600),
    }
}

/// How fast, like "5.1 MB/s".
pub fn speed(bytes_a_second: f64) -> String {
    format!("{}/s", fs::format_size(bytes_a_second as u64))
}

/// A picture of `size` filling in from the bottom as `done` goes from 0 to 1, faint until then.
pub fn filling(picture: impl Fn() -> AnyElement, size: Pixels, done: Option<f64>) -> Div {
    let filled = size * done.unwrap_or(0.) as f32;
    div().relative().flex_none().size(size).child(div().absolute().inset_0().opacity(0.3).child(picture())).child(
        div()
            .absolute()
            .left_0()
            .bottom_0()
            .w(size)
            .h(filled)
            .overflow_hidden()
            .child(div().absolute().left_0().bottom_0().size(size).child(picture())),
    )
}

/// How far along something is, from 0 to 1, as Finder draws it: a ring filling in as a pie,
/// empty while that's not known.
pub fn pie(done: Option<f64>, color: Hsla) -> impl IntoElement {
    let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut Window, _: &mut App| {
        let radius = f32::from(bounds.size.width) / 2.;
        let turn = |part: f64| ArcData::new(&(), 0, 0., 0., part as f32 * std::f32::consts::TAU);
        shape::Arc::new().inner_radius(radius - 1.).outer_radius(radius).paint(&turn(1.), color, &bounds, window);
        if let Some(done) = done {
            shape::Arc::new().outer_radius(radius - 2.).paint(&turn(done), color, &bounds, window);
        }
    };
    canvas(|_, _, _| {}, paint).size(px(12.)).flex_none()
}

/// How far along something is, as a bar, still moving while that's not known.
pub fn bar(id: impl Into<ElementId>, done: Option<f64>) -> Progress {
    Progress::new(id).xsmall().value(done.unwrap_or(0.) as f32 * 100.).loading(done.is_none())
}
