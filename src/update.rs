//! A copy of Mu in an Applications folder keeps itself up to date from the latest release on
//! GitHub, which the release workflow builds, signs and notarizes. A newer one is downloaded beside
//! this one and checked, then swapped in when Mu quits, or by Restart to Update.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::LazyLock,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui_kit::{component::notification::Notification, *};
use objc2_foundation::NSBundle;
use serde::Deserialize;

use crate::{
    fs::{c_path, replacement_folder, run},
    theme::Toasts as _,
    workspace,
};

gpui_kit::actions!(update, [CheckForUpdates, RestartToUpdate]);

/// How often GitHub is asked, besides at launch.
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);

/// The latest release that isn't a prerelease.
const LATEST: &str = "https://api.github.com/repos/sylophi/mu/releases/latest";

enum Status {
    Idle,
    Checking,
    UpToDate,
    /// Downloaded beside this version and checked, waiting to be swapped in.
    Ready {
        version: String,
        staged: PathBuf,
    },
    Failed(anyhow::Error),
}

pub struct Updater {
    /// This copy of Mu, when it's one that updates itself.
    app: Option<PathBuf>,
    status: Status,
    _checks: Task<()>,
}

struct GlobalUpdater(Entity<Updater>);

impl Global for GlobalUpdater {}

pub fn init(cx: &mut App) {
    let updater = cx.new(|cx| {
        let app = this_app();
        let checks = match app {
            Some(_) => cx.spawn(async |this: WeakEntity<Updater>, cx| {
                while let Ok(check) = this.update(cx, |this, cx| this.check(cx)) {
                    check.await;
                    cx.background_executor().timer(CHECK_EVERY).await;
                }
            }),
            None => Task::ready(()),
        };
        Updater { app, status: Status::Idle, _checks: checks }
    });
    cx.on_app_quit({
        let updater = updater.clone();
        move |cx| {
            updater.read(cx).install();
            async {}
        }
    })
    .detach();
    cx.on_action(|_: &CheckForUpdates, cx| check_now(cx));
    cx.on_action(|_: &RestartToUpdate, cx| restart(cx));
    cx.set_global(GlobalUpdater(updater));
}

impl Updater {
    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalUpdater>().0.clone()
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.status, Status::Ready { .. })
    }

    /// Looks for a newer version, unless one is ready already.
    pub fn check(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(app) = self.app.clone() else { return Task::ready(()) };
        if matches!(self.status, Status::Checking | Status::Ready { .. }) {
            return Task::ready(());
        }
        let Some(current) = version(&app) else {
            self.status = Status::Failed(anyhow!("this copy has no version"));
            return Task::ready(());
        };
        self.status = Status::Checking;
        cx.notify();
        let stage = cx.background_spawn(async move { stage_if_newer(&current, &app) });
        cx.spawn(async move |this, cx| {
            let status = match stage.await {
                Ok(None) => Status::UpToDate,
                Ok(Some((staged, version))) => Status::Ready { version, staged },
                Err(error) => Status::Failed(error),
            };
            this.update(cx, |this, cx| {
                this.status = status;
                cx.notify();
            })
            .ok();
        })
    }

    /// How the last check went, for when one was asked for.
    fn notification(&self) -> Notification {
        if self.app.is_none() {
            return Notification::info("Only a copy of Mu in Applications updates itself.");
        }
        match &self.status {
            Status::Idle | Status::Checking => Notification::info("Still checking for updates."),
            Status::UpToDate => Notification::info("Mu is up to date."),
            Status::Ready { version, .. } => {
                Notification::success(format!("Mu {version} is ready. Restart to update."))
            }
            Status::Failed(error) => Notification::error(format!("Couldn't update: {error:#}.")),
        }
    }

    /// Swaps the new version in, if there's one ready, so the next launch runs it. Should that fail,
    /// the next launch looks again.
    fn install(&self) {
        if let (Some(app), Status::Ready { staged, .. }) = (&self.app, &self.status) {
            swap_in(staged, app).ok();
        }
    }
}

/// Checks for updates now, and says what it found in the window in front.
fn check_now(cx: &mut App) {
    let Some(window) = cx.active_window() else { return };
    let updater = Updater::global(cx);
    let check = updater.update(cx, |updater, cx| updater.check(cx));
    cx.spawn(async move |cx| {
        check.await;
        window.update(cx, |_, window, cx| window.toast(updater.read(cx).notification(), cx)).ok();
    })
    .detach();
}

/// Quits, once any unsaved changes are seen to, and opens the new version in Mu's place.
fn restart(cx: &mut App) {
    let Some(app) = Updater::global(cx).read(cx).app.clone() else { return };
    workspace::windows::quit(cx, move || {
        // Waits for this process to be gone, and with it the swap made as it quits.
        let reopen =
            r#"pid=$1; shift; while /bin/kill -0 "$pid" 2>/dev/null; do /bin/sleep 0.2; done; /usr/bin/open "$@""#;
        let pid = std::process::id().to_string();
        Command::new("/bin/sh").args(["-c", reopen, "sh", &pid]).arg(&app).spawn().ok();
    });
}

/// This copy of Mu, when it's in an Applications folder. One run from anywhere else is left as it
/// is.
fn this_app() -> Option<PathBuf> {
    let app = bundle();
    (app.extension()? == "app" && app.parent()?.file_name()? == "Applications").then_some(app)
}

fn bundle() -> PathBuf {
    PathBuf::from(NSBundle::mainBundle().bundlePath().to_string())
}

/// The version this is, wherever it runs from. None when it isn't run as an app.
pub fn this_version() -> Option<&'static str> {
    static VERSION: LazyLock<Option<String>> = LazyLock::new(|| version(&bundle()));
    VERSION.as_deref()
}

/// The version in an app's Info.plist, which `make app` writes.
fn version(app: &Path) -> Option<String> {
    let plist = fs::read_to_string(app.join("Contents/Info.plist")).ok()?;
    let (_, rest) = plist.split_once("<key>CFBundleShortVersionString</key>")?;
    Some(rest.split_once("<string>")?.1.split_once("</string>")?.0.trim().to_string())
}

/// Whether `version` comes after `current`, both like 0.2.0.
fn newer(version: &str, current: &str) -> bool {
    let numbers = |version: &str| version.split('.').map(str::parse).collect::<Result<Vec<u32>, _>>().ok();
    matches!((numbers(version), numbers(current)), (Some(version), Some(current)) if version > current)
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Downloads a version newer than `current` to `app`'s volume, ready to swap in, and checks it, so
/// what's swapped in is what was checked. None when there's no newer version. MU_UPDATE_FEED asks
/// somewhere else for the latest release, to try an update from a local build.
fn stage_if_newer(current: &str, app: &Path) -> Result<Option<(PathBuf, String)>> {
    let feed = std::env::var("MU_UPDATE_FEED").unwrap_or_else(|_| LATEST.to_string());
    let latest = run(Command::new("/usr/bin/curl").args(["-fsSL", &feed])).context("couldn't reach GitHub")?;
    let release: Release = serde_json::from_str(&latest)?;
    let available = release.tag_name.trim_start_matches('v').to_string();
    if !newer(&available, current) {
        return Ok(None);
    }
    let asset = release.assets.iter().find(|asset| asset.name == "Mu.zip").context("the release has no Mu.zip")?;
    let folder = replacement_folder(app)?;
    let staged = folder.join("Mu.app");
    let checked = (|| {
        let zip = folder.join("Mu.zip");
        run(Command::new("/usr/bin/curl").args(["-fsSL", "-o"]).arg(&zip).arg(&asset.browser_download_url))?;
        run(Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(&zip).arg(&folder))?;
        fs::remove_file(&zip)?;
        if version(&staged).as_ref() != Some(&available) {
            bail!("the download isn't Mu {available}");
        }
        check_signature(&staged, app)
    })();
    if checked.is_err() {
        fs::remove_dir_all(&folder).ok();
    }
    checked.map(|()| Some((staged, available)))
}

/// Errs unless `staged` is intact and signed the way `app` is: same identifier, same Developer ID
/// team.
fn check_signature(staged: &Path, app: &Path) -> Result<()> {
    let shown = run(Command::new("/usr/bin/codesign").args(["-d", "-r-"]).arg(app)).unwrap_or_default();
    let requirement = shown.trim().strip_prefix("designated => ").context("this copy isn't signed")?;
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "--deep", "-R"])
        .arg(format!("={requirement}"))
        .arg(staged))
    .map_err(|_| anyhow!("the new version isn't signed like this one"))?;
    Ok(())
}

/// Swaps the staged copy in for `app` in one step, then lets go of the old one, which is left where
/// the new one was.
fn swap_in(staged: &Path, app: &Path) -> io::Result<()> {
    let (from, to) = (c_path(staged)?, c_path(app)?);
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(io::Error::last_os_error());
    }
    fs::remove_dir_all(staged.parent().unwrap_or(staged))
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions_by_their_numbers() {
        assert!(newer("0.2.0", "0.1.9") && newer("0.10.0", "0.9.0") && newer("1.0.0", "0.99.1"));
        assert!(!newer("0.1.0", "0.1.0") && !newer("0.1.0", "0.2.0") && !newer("0.2.0-beta", "0.1.0"));
    }
}
