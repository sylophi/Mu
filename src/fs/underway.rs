//! What's being copied, downloaded or uploaded, item by item, as the app doing it tells: iCloud
//! Drive syncing, Finder copying, a browser downloading, or Mu copying (see `Published`). Apps only
//! tell those watching the folder an item is in.

use std::{
    path::{Path, PathBuf},
    ptr::{NonNull, null_mut},
    rc::Rc,
    time::{Duration, Instant},
};

use block2::RcBlock;
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_foundation::{
    NSNumber, NSProgress, NSProgressFileOperationKindCopying, NSProgressFileOperationKindDecompressingAfterDownloading,
    NSProgressFileOperationKindDownloading, NSProgressFileOperationKindKey, NSProgressFileOperationKindReceiving,
    NSProgressFileOperationKindUploading, NSProgressFileURLKey, NSProgressKindFile, NSProgressUnpublishingHandler,
    NSString, NSURL,
};

use super::{file_url, icloud};

/// Something underway for an item: how far it's got, which way it's going and who's doing it.
pub struct Transfer {
    pub path: PathBuf,
    pub direction: Direction,
    /// Who's doing it, when that can be told.
    pub by: Option<&'static str>,
    progress: Retained<NSProgress>,
    /// When it was first heard of and how many bytes it had done, to tell how fast it goes.
    seen: (Instant, u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Upload,
    Download,
    Copy,
}

impl Transfer {
    fn new(path: PathBuf, progress: Retained<NSProgress>) -> Self {
        let info = |key: &str| progress.userInfo().objectForKey(&NSString::from_str(key));
        let direction = direction(&progress);
        let by = if info(BY_MU).is_some() {
            Some("Mu")
        } else if info("FIOperationUUID").is_some() {
            Some("Finder")
        } else if icloud::contains(&path) && direction != Direction::Copy {
            Some("iCloud Drive")
        } else {
            None
        };
        let mut transfer = Self { path, direction, by, progress, seen: (Instant::now(), 0) };
        transfer.seen.1 = transfer.bytes().map_or(0, |(done, _)| done);
        transfer
    }

    /// The bytes done and in all, while that's known. Most count in bytes, while Finder counts in
    /// thousandths and tells the bytes on the side.
    pub fn bytes(&self) -> Option<(u64, u64)> {
        let info = self.progress.userInfo();
        let side = |key| info.objectForKey(&NSString::from_str(key))?.downcast::<NSNumber>().ok().map(|n| n.as_i64());
        let (done, total) = match (side("NSProgressByteCompletedCountKey"), side("NSProgressByteTotalCountKey")) {
            (Some(done), Some(total)) => (done, total),
            _ => (self.progress.completedUnitCount(), self.progress.totalUnitCount()),
        };
        (done >= 0 && total > 0).then_some((done as u64, total as u64))
    }

    /// How far it's got, from 0 to 1, while that's known.
    pub fn fraction(&self) -> Option<f64> {
        (!self.progress.isIndeterminate()).then(|| self.progress.fractionCompleted().clamp(0., 1.))
    }

    /// Bytes a second, once it's gone on long enough to tell.
    pub fn speed(&self) -> Option<f64> {
        let (since, from) = self.seen;
        let elapsed = since.elapsed();
        let (done, _) = self.bytes()?;
        let speed = done.saturating_sub(from) as f64 / elapsed.as_secs_f64();
        (elapsed >= Duration::from_secs(2) && speed > 0.).then_some(speed)
    }

    pub fn time_left(&self) -> Option<Duration> {
        let (done, total) = self.bytes()?;
        Duration::try_from_secs_f64(total.saturating_sub(done) as f64 / self.speed()?).ok()
    }

    pub fn is_mu(&self) -> bool {
        self.by == Some("Mu")
    }

    pub fn can_stop(&self) -> bool {
        self.progress.isCancellable()
    }

    /// Asks whoever's doing it to stop.
    pub fn stop(&self) {
        self.progress.cancel();
    }

    /// Whether it got all the way, rather than being stopped.
    pub fn finished(&self) -> bool {
        !self.progress.isCancelled() && self.fraction().is_some_and(|fraction| fraction >= 1.)
    }
}

/// Which way what `progress` tells of is going, by its kind of file operation.
fn direction(progress: &NSProgress) -> Direction {
    let kind = progress.userInfo().objectForKey(unsafe { NSProgressFileOperationKindKey });
    let Some(Ok(kind)) = kind.map(|kind| kind.downcast::<NSString>()) else { return Direction::Copy };
    let is = |other: &NSString| *kind == *other;
    unsafe {
        if is(NSProgressFileOperationKindUploading) {
            Direction::Upload
        } else if is(NSProgressFileOperationKindDownloading)
            || is(NSProgressFileOperationKindReceiving)
            || is(NSProgressFileOperationKindDecompressingAfterDownloading)
        {
            Direction::Download
        } else {
            Direction::Copy
        }
    }
}

/// What a subscription hears.
pub enum Heard {
    Started(Rc<Transfer>),
    /// What started has stopped, finished or not.
    Stopped(Rc<Transfer>),
}

/// Hears what's underway for the items in a folder, until it's dropped.
pub struct Subscription(Retained<AnyObject>);

/// Hears what's underway for the items in `dir`, on the main thread, where this is called.
pub fn subscribe(dir: &Path, heard: impl Fn(Heard) + 'static) -> Subscription {
    let heard = Rc::new(heard);
    let dir = dir.to_path_buf();
    let url = file_url(&dir);
    let started = RcBlock::new(move |progress: NonNull<NSProgress>| -> NSProgressUnpublishingHandler {
        let progress = unsafe { Retained::retain(progress.as_ptr()) }.unwrap();
        let Some(path) = item_path(&progress).and_then(|path| Some(dir.join(path.file_name()?))) else {
            return null_mut();
        };
        let transfer = Rc::new(Transfer::new(path, progress));
        heard(Heard::Started(transfer.clone()));
        let heard = heard.clone();
        let stopped = RcBlock::new(move || heard(Heard::Stopped(transfer.clone())));
        // Handed back without a hold on it, as an Objective-C block's result is.
        unsafe { objc2::ffi::objc_autorelease(RcBlock::into_raw(stopped).cast()).cast() }
    });
    Subscription(unsafe { NSProgress::addSubscriberForFileURL_withPublishingHandler(&url, RcBlock::as_ptr(&started)) })
}

impl Drop for Subscription {
    fn drop(&mut self) {
        unsafe { NSProgress::removeSubscriber(&self.0) };
    }
}

/// The item a published `progress` is for.
fn item_path(progress: &NSProgress) -> Option<PathBuf> {
    let url = progress.userInfo().objectForKey(unsafe { NSProgressFileURLKey })?;
    url.downcast::<NSURL>().ok()?.to_file_path()
}

/// What marks Mu's own, which the status bar lists with Mu's operations instead.
const BY_MU: &str = "MuOperation";

/// Tells other apps, and Mu's own folders, how far copying to an item has got, until it's dropped.
pub struct Published(Retained<NSProgress>);

impl Published {
    /// For `bytes` being copied to `path`.
    pub fn copying(path: &Path, bytes: u64) -> Self {
        let progress = NSProgress::discreteProgressWithTotalUnitCount(bytes as i64);
        unsafe {
            progress.setKind(Some(NSProgressKindFile));
            progress.setFileOperationKind(Some(NSProgressFileOperationKindCopying));
            progress.setUserInfoObject_forKey(Some(&NSNumber::new_bool(true)), &NSString::from_str(BY_MU));
        }
        // Mu's operation is what stops it, from the status bar.
        progress.setCancellable(false);
        progress.setFileURL(Some(&file_url(path)));
        progress.publish();
        Self(progress)
    }

    pub fn set_copied(&self, bytes: u64) {
        self.0.setCompletedUnitCount(bytes as i64);
    }
}

impl Drop for Published {
    fn drop(&mut self) {
        self.0.unpublish();
    }
}
