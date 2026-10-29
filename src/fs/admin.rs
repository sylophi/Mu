//! What only an administrator may do, like moving an app the system owns or making a folder in
//! /Library. A step of a file operation that you aren't allowed is done again by Mu's own
//! executable running as root, once macOS has asked for an administrator's password, as in Finder.
//! That stays for the rest of the operation, so it asks once, and goes when the operation's done.
//! An operation runs in a `session` on one thread from start to end, so root is held per thread,
//! and operations running side by side each ask for their own.

use std::{
    cell::RefCell,
    ffi::{CString, c_char, c_void},
    fs::{self, File},
    io::{self, BufRead, BufReader, Write},
    os::fd::FromRawFd as _,
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::fs::{self as files, Cancelled, Progress};

/// What Mu's executable is started with to run as root.
pub const ARGUMENT: &str = "--admin";

/// A step to do as root.
#[derive(Debug, Serialize, Deserialize)]
pub enum Step {
    Rename(PathBuf, PathBuf),
    Copy(PathBuf, PathBuf),
    MakeFolder(PathBuf),
    /// Makes a file with these bytes in it.
    Write(PathBuf, Vec<u8>),
    Delete(PathBuf),
    /// Moves an item into the Trash (from, to), noting where it came from.
    Trash(PathBuf, PathBuf),
}

impl Step {
    fn run(self) -> Result<()> {
        match self {
            Step::Rename(from, to) => fs::rename(from, to)?,
            Step::Copy(from, to) => files::copyfile(&from, &to, &Progress::default())?,
            Step::MakeFolder(path) => fs::create_dir_all(path)?,
            Step::Write(path, bytes) => File::create_new(path)?.write_all(&bytes)?,
            Step::Delete(path) => files::remove_all(&path)?,
            Step::Trash(from, to) => {
                fs::rename(&from, &to)?;
                files::set_origin(&to, &from);
            }
        }
        Ok(())
    }
}

/// Root for the operation underway: Mu's executable running as root, turned down (or it couldn't
/// start), or not asked for yet.
enum Root {
    Unasked,
    Running(Admin),
    Declined,
}

thread_local! {
    static ROOT: RefCell<Root> = const { RefCell::new(Root::Unasked) };
}

/// Does `step` as root, if `error`, what doing it as you failed with, says you weren't allowed.
pub fn retry(error: io::Error, step: Step) -> Result<()> {
    if error.raw_os_error() != Some(libc::EACCES) {
        return Err(error.into());
    }
    run(step)
}

/// Does `step` as root, asking for the password first if this operation hasn't yet. Turned down,
/// it's cancelled, and so is the rest of the operation.
pub fn run(step: Step) -> Result<()> {
    ROOT.with_borrow_mut(|root| {
        if let Root::Unasked = root {
            match Admin::start() {
                Ok(admin) => *root = Root::Running(admin),
                // Asked once, so the steps after don't ask again.
                Err(error) => {
                    *root = Root::Declined;
                    return Err(error);
                }
            }
        }
        match root {
            Root::Running(admin) => admin.ask(&step),
            _ => Err(Cancelled.into()),
        }
    })
}

/// Runs `work`, a file operation, then lets go of root if it took that, so the next one asks again.
pub fn session<R>(work: impl FnOnce() -> R) -> R {
    let result = work();
    release();
    result
}

fn release() {
    if let Root::Running(Admin { pid, pipe }) = ROOT.replace(Root::Unasked) {
        // Hung up on, it quits.
        drop(pipe);
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) };
    }
}

/// Mu's executable running as root, and the pipe to it: steps go in a line each, and how each
/// went comes back.
struct Admin {
    pid: i32,
    pipe: BufReader<File>,
}

impl Admin {
    /// Starts Mu's executable as root, once macOS has asked for an administrator's password.
    fn start() -> Result<Self> {
        #[link(name = "Security", kind = "framework")]
        unsafe extern "C" {
            fn AuthorizationCreate(
                rights: *const c_void,
                environment: *const c_void,
                flags: u32,
                authorization: *mut *mut c_void,
            ) -> i32;
            // Deprecated, but the one way to run something as root without installing a helper.
            fn AuthorizationExecuteWithPrivileges(
                authorization: *mut c_void,
                tool: *const c_char,
                options: u32,
                arguments: *const *const c_char,
                pipe: *mut *mut libc::FILE,
            ) -> i32;
            fn AuthorizationFree(authorization: *mut c_void, flags: u32) -> i32;
        }
        let tool = files::c_path(&std::env::current_exe()?)?;
        let argument = CString::new(ARGUMENT)?;
        let arguments = [argument.as_ptr(), std::ptr::null()];
        let mut pipe = std::ptr::null_mut();
        let status = unsafe {
            let mut authorization = std::ptr::null_mut();
            AuthorizationCreate(std::ptr::null(), std::ptr::null(), 0, &mut authorization);
            let status =
                AuthorizationExecuteWithPrivileges(authorization, tool.as_ptr(), 0, arguments.as_ptr(), &mut pipe);
            AuthorizationFree(authorization, 0);
            status
        };
        match status {
            0 => {}
            // Cancelled.
            -60006 => return Err(Cancelled.into()),
            code => bail!("Couldn't get an administrator's rights (error {code})"),
        }
        // A descriptor of its own, closed in whatever Mu starts later, like a terminal's shell, so
        // only Mu holds the pipe and root hears when Mu hangs up.
        let pipe = unsafe {
            let fd = libc::fcntl(libc::fileno(pipe), libc::F_DUPFD_CLOEXEC, 0);
            libc::fclose(pipe);
            File::from_raw_fd(fd)
        };
        Self::connect(pipe)
    }

    /// Takes the pipe to Mu's executable, which starts by saying its process ID.
    fn connect(pipe: File) -> Result<Self> {
        let mut pipe = BufReader::new(pipe);
        let mut pid = String::new();
        pipe.read_line(&mut pid)?;
        let pid = pid.trim().parse().context("Mu didn't start as an administrator")?;
        Ok(Self { pid, pipe })
    }

    fn ask(&mut self, step: &Step) -> Result<()> {
        writeln!(self.pipe.get_mut(), "{}", serde_json::to_string(step)?)?;
        let mut reply = String::new();
        self.pipe.read_line(&mut reply)?;
        let reply: Result<(), String> =
            serde_json::from_str(&reply).context("Mu stopped running as an administrator")?;
        reply.map_err(|message| anyhow!(message))
    }
}

/// What Mu's executable does when it's started as root: each step it's sent, till it's hung up on.
pub fn serve() -> ExitCode {
    match serve_on(io::stdin().lock(), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn serve_on(input: impl BufRead, mut output: impl Write) -> Result<()> {
    writeln!(output, "{}", std::process::id())?;
    for line in input.lines() {
        let done = serde_json::from_str(&line?).map_err(anyhow::Error::from).and_then(Step::run);
        let reply: Result<(), String> = done.map_err(|error| format!("{error:#}"));
        writeln!(output, "{}", serde_json::to_string(&reply)?)?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::Scratch;
    use std::os::{fd::OwnedFd, unix::net::UnixStream};

    #[test]
    fn does_each_step_it_is_sent() {
        let scratch = Scratch::new("admin");
        let dir = &scratch.0;
        let (ours, theirs) = UnixStream::pair().unwrap();
        std::thread::spawn(move || serve_on(BufReader::new(theirs.try_clone().unwrap()), theirs));
        let mut admin = Admin::connect(File::from(OwnedFd::from(ours))).unwrap();
        assert_eq!(admin.pid, std::process::id() as i32);

        let at = |name: &str| dir.join(name);
        admin.ask(&Step::MakeFolder(at("a/b"))).unwrap();
        admin.ask(&Step::Write(at("a/b/note.txt"), b"hi".to_vec())).unwrap();
        admin.ask(&Step::Copy(at("a"), at("copy"))).unwrap();
        assert_eq!(fs::read(at("copy/b/note.txt")).unwrap(), b"hi");
        admin.ask(&Step::Rename(at("copy"), at("renamed"))).unwrap();
        admin.ask(&Step::Trash(at("renamed"), at("trashed"))).unwrap();
        assert_eq!(files::origin(&at("trashed")), Some(at("renamed")));
        admin.ask(&Step::Delete(at("trashed"))).unwrap();
        assert!(!at("trashed").exists() && at("a/b/note.txt").exists());
        // What fails says why, and the next step still goes.
        let error = admin.ask(&Step::Write(at("a/b/note.txt"), Vec::new())).unwrap_err();
        assert_eq!(error.to_string(), "File exists (os error 17)");
    }
}
