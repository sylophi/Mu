//! Which processes are using a file or a disk: have it open, or are working in it, as a shell is in
//! its current folder. Without root, only your own processes show.

use std::{
    ffi::{OsStr, c_char, c_int, c_void},
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
};

use super::{app_bundle, c_path};

/// A process using something.
#[derive(Debug)]
pub struct Holder {
    pub pid: u32,
    /// What it goes by: its app's name, for a program inside an app.
    pub name: String,
}

impl Holder {
    pub(super) fn new(pid: u32, exe: Option<&Path>) -> Self {
        let name = exe
            .map(|exe| app_bundle(exe).unwrap_or_else(|| exe.to_path_buf()))
            .and_then(|path| path.file_stem().map(|name| name.to_string_lossy().into_owned()))
            .unwrap_or_else(|| format!("process {pid}"));
        Self { pid, name }
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
        .map(|pid| Holder::new(pid, executable(pid).as_deref()))
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

    use super::{Holder, holders, names};
    use crate::fs::Scratch;

    #[test]
    fn finds_what_has_it_open() {
        let scratch = Scratch::new("using");
        let path = scratch.0.join("held");
        std::fs::write(&path, "").unwrap();
        assert!(holders(&path, false).is_empty(), "Mu itself doesn't count");
        let mut sleep = Command::new("/bin/sleep").arg("30").stdin(File::open(&path).unwrap()).spawn().unwrap();
        let found = holders(&path, false);
        sleep.kill().unwrap();
        sleep.wait().unwrap();
        assert_eq!(
            found.iter().map(|holder| (holder.pid, holder.name.as_str())).collect::<Vec<_>>(),
            [(sleep.id(), "sleep")]
        );
    }

    #[test]
    fn names_each_once() {
        let holder = |pid, name: &str| Holder { pid, name: name.into() };
        assert_eq!(names(&[holder(1, "Slack"), holder(2, "Slack")]), "Slack");
        assert_eq!(names(&[holder(1, "Photos"), holder(2, "tail")]), "Photos and tail");
        assert_eq!(names(&[holder(1, "Photos"), holder(2, "Preview"), holder(3, "tail")]), "Photos, Preview and tail");
    }
}
