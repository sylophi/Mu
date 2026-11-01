//! Which processes are using a file or a disk: have it open, or are working in it, as a shell is in
//! its current folder. Without root, only your own processes show.

use std::{
    ffi::{OsStr, c_char, c_int, c_void},
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
};

use super::{c_path, program_name};

/// A process using something.
#[derive(Clone, Debug)]
pub struct Holder {
    pub pid: u32,
    /// Its program, when macOS says.
    pub exe: Option<PathBuf>,
    /// What it goes by: its app's name, for a program inside an app.
    pub name: String,
}

impl Holder {
    pub(super) fn new(pid: u32, exe: Option<PathBuf>) -> Self {
        let name = exe.as_deref().and_then(program_name).unwrap_or_else(|| format!("process {pid}"));
        Self { pid, exe, name }
    }

    /// Whether it's one of macOS's own, like Quick Look making a thumbnail.
    pub fn is_system(&self) -> bool {
        self.exe.as_ref().is_some_and(|exe| exe.starts_with("/System/"))
    }
}

unsafe extern "C" {
    fn proc_listpidspath(
        kind: u32,
        info: u32,
        path: *const c_char,
        flags: u32,
        buffer: *mut c_void,
        size: c_int,
    ) -> c_int;
}

const PROC_ALL_PIDS: u32 = 1;
const PROC_LISTPIDSPATH_PATH_IS_VOLUME: u32 = 1;
const PROC_LISTPIDSPATH_EXCLUDE_EVTONLY: u32 = 2;

/// The processes using `path`, or with `volume`, anything on the volume mounted there. Those only
/// watching it for changes, as Mu does the folders it shows, don't count, and neither does Mu.
pub fn holders(path: &Path, volume: bool) -> Vec<Holder> {
    let Ok(path) = c_path(path) else { return Vec::new() };
    let flags = PROC_LISTPIDSPATH_EXCLUDE_EVTONLY | if volume { PROC_LISTPIDSPATH_PATH_IS_VOLUME } else { 0 };
    let mut pids = vec![0 as c_int; 1024];
    let size = (pids.len() * size_of::<c_int>()) as c_int;
    let bytes = unsafe { proc_listpidspath(PROC_ALL_PIDS, 0, path.as_ptr(), flags, pids.as_mut_ptr().cast(), size) };
    pids.truncate(bytes.max(0) as usize / size_of::<c_int>());
    pids.into_iter()
        .map(|pid| pid as u32)
        .filter(|&pid| pid != std::process::id())
        .map(|pid| Holder::new(pid, executable(pid)))
        .collect()
}

/// The processes whose program is in `dir`, like an app and its helpers.
pub fn running_in(dir: &Path) -> Vec<Holder> {
    // Programs go by their real paths, like /private/var for /var.
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let mut pids = vec![0 as c_int; 8192];
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * size_of::<c_int>()) as c_int) };
    pids.truncate(count.max(0) as usize);
    pids.into_iter()
        .filter_map(|pid| {
            let exe = executable(pid as u32).filter(|exe| exe.starts_with(&dir))?;
            Some(Holder::new(pid as u32, Some(exe)))
        })
        .collect()
}

fn executable(pid: u32) -> Option<PathBuf> {
    let mut path = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let len = unsafe { libc::proc_pidpath(pid as c_int, path.as_mut_ptr().cast(), path.len() as u32) };
    (len > 0).then(|| PathBuf::from(OsStr::from_bytes(&path[..len as usize])))
}

/// Their names as a sentence would have them, like "Photos, Preview and tail".
pub fn names(holders: &[Holder]) -> String {
    match distinct(holders).split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// Their names, each once, as an app can have several processes at it.
pub(super) fn distinct(holders: &[Holder]) -> Vec<&str> {
    let mut names = Vec::new();
    for holder in holders {
        if !names.contains(&holder.name.as_str()) {
            names.push(holder.name.as_str());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use std::{fs::File, process::Command};

    use super::{Holder, holders, names, running_in};
    use crate::fs::Scratch;

    #[test]
    fn finds_what_has_it_open() {
        let scratch = Scratch::new("using");
        let path = scratch.0.join("held");
        std::fs::write(&path, "").unwrap();
        // Others, like Spotlight, can have it open too.
        let held_by =
            |pid| holders(&path, false).into_iter().find(|holder| holder.pid == pid).map(|holder| holder.name);
        assert_eq!(held_by(std::process::id()), None, "Mu itself doesn't count");
        let mut sleep = Command::new("/bin/sleep").arg("30").stdin(File::open(&path).unwrap()).spawn().unwrap();
        let found = held_by(sleep.id());
        sleep.kill().unwrap();
        sleep.wait().unwrap();
        assert_eq!(found.as_deref(), Some("sleep"));
    }

    #[test]
    fn finds_what_runs_from_a_folder() {
        let scratch = Scratch::new("running-in");
        let program = scratch.0.join("sleep");
        std::fs::copy("/bin/sleep", &program).unwrap();
        let mut sleep = Command::new(&program).arg("30").spawn().unwrap();
        let found = running_in(&scratch.0).into_iter().any(|holder| holder.pid == sleep.id());
        sleep.kill().unwrap();
        sleep.wait().unwrap();
        assert!(found);
    }

    #[test]
    fn names_each_once() {
        let holder = |pid, name: &str| Holder { pid, exe: None, name: name.into() };
        assert_eq!(names(&[holder(1, "Slack"), holder(2, "Slack")]), "Slack");
        assert_eq!(names(&[holder(1, "Photos"), holder(2, "tail")]), "Photos and tail");
        assert_eq!(names(&[holder(1, "Photos"), holder(2, "Preview"), holder(3, "tail")]), "Photos, Preview and tail");
    }
}
