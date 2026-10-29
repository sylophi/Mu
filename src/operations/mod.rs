//! Running file operations: off the main thread with their progress in the status bar, once
//! anything that needs asking about has been asked, and into the undo history once they're done.

mod clash;

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::*;

use crate::{
    fs::{self, Done, Progress},
    history::{self, Op},
    theme::Toasts as _,
};

pub fn init(cx: &mut App) {
    cx.set_global(Transfers::default());
}

/// File operations underway, for the status bar to show how far their copying has got.
#[derive(Default)]
pub struct Transfers(Vec<Transfer>);

impl Global for Transfers {}

pub struct Transfer {
    /// What it's doing, like "Copying".
    pub verb: &'static str,
    pub progress: Arc<Progress>,
    pub started: Instant,
}

pub fn transfers(cx: &App) -> &[Transfer] {
    &cx.global::<Transfers>().0
}

/// Runs `work` off the main thread, keeping the status bar up to date on its progress.
pub fn track<R: Send + 'static>(
    verb: &'static str,
    work: impl FnOnce(&Progress) -> R + Send + 'static,
    cx: &mut App,
) -> Task<R> {
    let progress = Arc::new(Progress::default());
    let transfer = Transfer { verb, progress: progress.clone(), started: Instant::now() };
    cx.update_global::<Transfers, _>(|transfers, _| transfers.0.push(transfer));
    let task = cx.background_spawn({
        let progress = progress.clone();
        async move { fs::admin::session(|| work(&progress)) }
    });
    cx.spawn(async move |cx| {
        // An update that changes nothing, four times a second, has the status bar, which observes
        // Transfers, draw how far the copying has got.
        let ticks = cx.spawn(async |cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                cx.update(|cx| cx.update_global::<Transfers, _>(|_, _| {}));
            }
        });
        let result = task.await;
        drop(ticks);
        cx.update(|cx| {
            cx.update_global::<Transfers, _>(|transfers, _| {
                transfers.0.retain(|transfer| !Arc::ptr_eq(&transfer.progress, &progress))
            })
        });
        result
    })
}

/// Runs `op` off the main thread and remembers how to take it back, saying so if any of it
/// failed. Resolves to what it did.
pub fn run(
    op: Op,
    work: impl FnOnce(&Progress) -> Done + Send + 'static,
    window: &mut Window,
    cx: &mut App,
) -> Task<Done> {
    let task = track(op.verb(), work, cx);
    let window = window.window_handle();
    cx.spawn(async move |cx| {
        let mut done = task.await;
        window
            .update(cx, |_, window, cx| {
                history::record(op, done.undo.take(), window, cx);
                if let Some(error) = &done.error {
                    window.toast_error(error, cx);
                }
            })
            .ok();
        done
    })
}

/// Copies or moves `paths` into folder `dir` (see `run`), once it's asked about any names taken
/// there, or moves them to the Trash. Resolves to what it did, or None when it's cancelled.
pub fn transfer(op: Op, paths: Vec<PathBuf>, dir: PathBuf, window: &mut Window, cx: &mut App) -> Task<Option<Done>> {
    if matches!(op, Op::Trash) {
        return trash(paths, window, cx);
    }
    let choose = clash::choose(op, paths.clone(), dir.clone(), window, cx);
    window.spawn(cx, async move |cx| {
        let choices = choose.await?;
        let work = move |progress: &Progress| match op {
            Op::Copy => fs::copy_into(&paths, &dir, &choices, progress),
            _ => fs::move_into(&paths, &dir, &choices, progress),
        };
        let task = cx.update(|window, cx| run(op, work, window, cx)).ok()?;
        Some(task.await)
    })
}

/// Moves `paths` to the Trash (see `run`), once it's asked about any that are locked, like Finder.
/// Resolves to what it did, or None when it's cancelled.
pub fn trash(paths: Vec<PathBuf>, window: &mut Window, cx: &mut App) -> Task<Option<Done>> {
    let locked: Vec<_> = paths.iter().filter(|path| fs::is_locked(path)).collect();
    let question = match locked.as_slice() {
        [] => None,
        [path] => Some(format!("“{}” is locked. Move it to the Trash anyway?", fs::display_name(path))),
        locked => {
            Some(format!("{} are locked. Move them to the Trash anyway?", fs::count(locked.len(), "item", "items")))
        }
    };
    let answer =
        question.map(|question| window.prompt(PromptLevel::Warning, &question, None, &["Move to Trash", "Cancel"], cx));
    window.spawn(cx, async move |cx| {
        if let Some(answer) = answer
            && answer.await != Ok(0)
        {
            return None;
        }
        let task = cx.update(|window, cx| run(Op::Trash, move |_| fs::trash(&paths), window, cx)).ok()?;
        Some(task.await)
    })
}
