//! Zipping items like Finder, and extracting archives like Archive Utility.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, anyhow};
use objc2_foundation::{NSFileManager, NSSearchPathDirectory, NSSearchPathDomainMask};

use super::{Done, Progress, c_path, disk_name, display_name, each, file_url, move_path, run, unique_path};

/// Zips `paths` (all in `dir`) like Finder: "name.zip" for one item, "Archive.zip" for several.
pub fn compress(paths: &[PathBuf], dir: &Path) -> Done {
    let name = match paths {
        [path] => format!("{}.zip", display_name(path)),
        _ => "Archive.zip".into(),
    };
    let target = unique_path(dir, &name, false);
    // Where you can't write, it's zipped in the temporary folder and moved in as an administrator.
    let writable = c_path(dir).is_ok_and(|dir| unsafe { libc::access(dir.as_ptr(), libc::W_OK) } == 0);
    let zip = if writable { target.clone() } else { unique_path(&std::env::temp_dir(), &name, false) };
    let zipped = run(Command::new("/usr/bin/zip")
        .current_dir(dir)
        .arg("-qry")
        .arg(&zip)
        .args(paths.iter().map(|path| Path::new(".").join(disk_name(path)))))
    .context("Couldn't compress");
    let made = zipped.and_then(|_| if writable { Ok(target) } else { move_path(&zip, &target, &Progress::default()) });
    match made {
        Ok(target) => Done::created(vec![target], None),
        Err(error) => {
            let _ = fs::remove_file(&zip);
            Done::failed(error)
        }
    }
}

/// Extracts the archives at `paths` into `dir` like Archive Utility: one that holds a single item
/// gives that item, and one that holds more gives a folder named after it.
pub fn extract(paths: &[PathBuf], dir: &Path) -> Done {
    let (made, error) = each(paths, |archive| {
        extract_one(archive, dir).with_context(|| format!("Couldn't extract “{}”", display_name(archive)))
    });
    Done::created(made, error)
}

fn extract_one(archive: &Path, dir: &Path) -> Result<PathBuf> {
    // Unpacked out of sight on the same disk, so nothing half done shows, then moved in whole.
    let folder = replacement_folder(dir)?;
    let name = Path::new(archive.file_stem().unwrap_or_default());
    let name = match name.extension() {
        Some(ext) if ext.eq_ignore_ascii_case("tar") => name.file_stem().unwrap_or_default(),
        _ => name.as_os_str(),
    };
    let unpacked = folder.join(name);
    let extracted = (|| {
        fs::create_dir(&unpacked)?;
        // ditto for a zip, as Archive Utility does, and tar for the rest. Both carry over the
        // quarantine of a download, so Gatekeeper still checks what's in it.
        if archive.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("zip")) {
            run(Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(archive).arg(&unpacked))?;
        } else {
            run(Command::new("/usr/bin/tar").arg("-xf").arg(archive).arg("-C").arg(&unpacked))?;
        }
        let items = fs::read_dir(&unpacked)?.map(|entry| Ok(entry?.path())).collect::<io::Result<Vec<_>>>()?;
        let item = match items.as_slice() {
            [only] => only,
            _ => &unpacked,
        };
        move_path(item, &dir.join(disk_name(item)), &Progress::default())
    })();
    fs::remove_dir_all(&folder).ok();
    extracted
}

/// A new folder on the same disk as `path`, out of sight, for making something that's then moved
/// in whole.
pub fn replacement_folder(path: &Path) -> Result<PathBuf> {
    let folder = NSFileManager::defaultManager()
        .URLForDirectory_inDomain_appropriateForURL_create_error(
            NSSearchPathDirectory::ItemReplacementDirectory,
            NSSearchPathDomainMask::UserDomainMask,
            Some(&file_url(path)),
            true,
        )
        .map_err(|error| anyhow!("{}", error.localizedDescription()))?;
    folder.to_file_path().context("No folder to work in")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Scratch, tests::names};

    #[test]
    fn extracts_like_archive_utility() {
        let scratch = Scratch::new("extract");
        let (from, to) = (scratch.0.join("from"), scratch.0.join("to"));
        fs::create_dir_all(from.join("Mu.app/Contents")).unwrap();
        fs::write(from.join("Mu.app/Contents/Info.plist"), "").unwrap();
        fs::write(from.join("a.txt"), "a").unwrap();
        fs::write(from.join("b.txt"), "b").unwrap();
        fs::write(from.join("broken.zip"), "not a zip").unwrap();
        let app = compress(&[from.join("Mu.app")], &from).paths;
        let both = compress(&[from.join("a.txt"), from.join("b.txt")], &from).paths;
        Command::new("/usr/bin/tar")
            .current_dir(&from)
            .args(["-czf", "notes.tar.gz", "a.txt", "b.txt"])
            .status()
            .unwrap();
        // The name a folder would get is taken.
        fs::create_dir_all(to.join("notes")).unwrap();

        let done = extract(&[app[0].clone(), both[0].clone(), from.join("notes.tar.gz")], &to);
        assert!(done.error.is_none());
        assert_eq!(done.paths, [to.join("Mu.app"), to.join("Archive"), to.join("notes 2")]);
        assert!(to.join("Mu.app/Contents/Info.plist").exists());
        assert_eq!(names(&to.join("notes 2")), ["a.txt", "b.txt"]);

        let done = extract(&[from.join("broken.zip")], &to);
        assert!(done.error.is_some() && done.paths.is_empty());
        assert_eq!(names(&to), ["Archive", "Mu.app", "notes", "notes 2"]);
    }
}
