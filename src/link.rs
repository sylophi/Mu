//! Opening a tab in Mu from anywhere: a link like `mu://terminal/Users/me/code` from any app, or
//! `mu terminal ~/code` from a shell. The `mu` command is Mu's own executable, which Settings links
//! into ~/.local/bin, and it hands Mu a link, so Mu starts if it isn't running.

use std::{
    ffi::OsString,
    fs, io,
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use objc2_foundation::{NSString, NSURL};

use crate::{fs::home_dir, session::Tab};

const USAGE: &str = "usage: mu [terminal | edit | find | storage | activity | history | settings] [path]

Opens a tab in Mu: the folder at path, or a file selected in its folder, unless it's one of those.
Without a path, it's the current folder.";

/// A kind of tab: what it's called in links and as the command's first word, and the tab it is at a
/// path. Whether it takes that path is up to `Tab::path`.
type Kind = (&'static str, fn(PathBuf) -> Tab);

const KINDS: [Kind; 8] = [
    ("folder", Tab::Folder),
    ("terminal", Tab::Terminal),
    ("edit", Tab::Editor),
    ("find", Tab::Search),
    ("storage", Tab::Storage),
    ("activity", |_| Tab::Activity),
    ("history", |_| Tab::History),
    ("settings", |_| Tab::Settings),
];

/// The tab a link is for: one of Mu's own, or a folder or file another app hands Mu, as
/// `open -a Mu ~/Downloads` does.
pub fn tab(url: &str) -> Option<Tab> {
    let url = NSURL::URLWithString(&NSString::from_str(url))?;
    if url.isFileURL() {
        return url.to_file_path().map(Tab::Folder);
    }
    if url.scheme()?.to_string() != "mu" {
        return None;
    }
    let path = url.path().map(|path| path.to_string()).filter(|path| !path.is_empty());
    let path = path.map_or_else(home_dir, PathBuf::from);
    let host = url.host()?.to_string();
    let (_, tab) = KINDS.iter().find(|(kind, _)| *kind == host)?;
    Some(tab(path))
}

/// The app this is, when it was started as the `mu` command, from a shell or a script. macOS starts
/// apps itself, from launchd.
pub fn command() -> Option<PathBuf> {
    if std::os::unix::process::parent_id() == 1 {
        return None;
    }
    // Mu.app/Contents/MacOS/mu
    executable()?.ancestors().nth(3).map(Path::to_path_buf)
}

/// Mu's executable, when it's in an app. A build that isn't has no command.
fn executable() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    (exe.ancestors().nth(3)?.extension()? == "app").then_some(exe)
}

/// Where Settings puts the `mu` command, with your own commands.
pub fn command_link() -> PathBuf {
    home_dir().join(".local/bin/mu")
}

#[derive(Clone, Copy, PartialEq)]
pub enum Installed {
    Yes,
    No,
    /// Something else has the name, maybe another copy of Mu.
    Other,
}

/// Whether the `mu` command is this Mu, unless this Mu has no command.
pub fn installed() -> Option<Installed> {
    let exe = executable()?;
    let link = command_link();
    Some(match fs::symlink_metadata(&link) {
        Err(_) => Installed::No,
        Ok(_) if link.canonicalize().is_ok_and(|target| target == exe) => Installed::Yes,
        Ok(_) => Installed::Other,
    })
}

/// Makes the `mu` command this Mu, in place of anything else with the name.
pub fn install() -> io::Result<()> {
    let exe = executable().ok_or(io::ErrorKind::NotFound)?;
    let link = command_link();
    fs::create_dir_all(link.parent().expect("it's in a folder"))?;
    match fs::remove_file(&link) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    std::os::unix::fs::symlink(exe, link)
}

pub fn uninstall() -> io::Result<()> {
    fs::remove_file(command_link())
}

/// Runs the `mu` command, handing what it asks for to the Mu at `app`.
pub fn run(app: &Path) -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "-h" || arg == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let (kind, tab) = match args.first().and_then(|arg| KINDS.iter().find(|(kind, _)| arg == kind)) {
        Some(&found) => {
            args.remove(0);
            found
        }
        // Without a kind, it's a folder.
        None => KINDS[0],
    };
    // Only a tab of a kind that opens somewhere takes the path.
    let url = match tab(PathBuf::new()).path() {
        None => format!("mu://{kind}"),
        Some(_) => {
            let path = args.first().map_or(Path::new("."), Path::new);
            let Ok(path) = path.canonicalize() else {
                eprintln!("mu: nothing at {}", path.display());
                return ExitCode::FAILURE;
            };
            format!("mu://{kind}{}", encode(&path))
        }
    };
    match Command::new("/usr/bin/open").arg("-a").arg(app).arg(url).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// `path` as a link has it, with anything but letters, digits, slashes and `-._~` escaped.
fn encode(path: &Path) -> String {
    escape(path.as_os_str().as_bytes(), b"/.")
}

/// `bytes` as a URL has them, with anything but letters, digits, `-_~` and `keep` escaped.
pub fn escape(bytes: &[u8], keep: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'~' => char::from(byte).to_string(),
            byte if keep.contains(&byte) => char::from(byte).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{encode, tab};
    use crate::session::Tab;

    #[test]
    fn links_round_trip() {
        let path = Path::new("/Users/me/My Code/#1 ?ü");
        let url = format!("mu://terminal{}", encode(path));
        assert_eq!(url, "mu://terminal/Users/me/My%20Code/%231%20%3F%C3%BC");
        assert!(matches!(tab(&url), Some(Tab::Terminal(p)) if p == path));
        assert!(matches!(tab("mu://settings"), Some(Tab::Settings)));
        assert!(matches!(tab("file:///tmp/"), Some(Tab::Folder(p)) if p == Path::new("/tmp")));
        assert!(tab("mu://nothing/tmp").is_none());
    }
}
