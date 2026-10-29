//! What kind of file something is, mostly by its extension: its category and the kind the list
//! shows, and whether opening it mounts or extracts it. And reading one that's text.

use std::{
    fs::{self, File},
    io::Read as _,
    path::Path,
};

use anyhow::{Result, bail};
use gpui_kit::SharedString;
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::NSString;

use super::disk_name;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Folder,
    App,
    Image,
    Video,
    Audio,
    Archive,
    DiskImage,
    Code,
    Text,
    Pdf,
    Document,
    Spreadsheet,
    Presentation,
    Font,
    Executable,
    Other,
}

/// Whether the folder at `path` is a package, like an app, which opens instead of being browsed.
/// LaunchServices knows, by its extension, the apps that open it or the bundle bit in its Finder
/// info.
pub fn is_package(path: &Path) -> bool {
    autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace().isFilePackageAtPath(&NSString::from_str(&path.to_string_lossy()))
    })
}

/// `name`'s extension, lowercased.
pub fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// What opening extracts, by extension: zips, tarballs, and 7-Zip and RAR archives.
const ARCHIVES: [&str; 8] = ["zip", "tar", "tgz", "tbz", "tbz2", "txz", "7z", "rar"];
/// What a tarball can be compressed with. A file that's only compressed isn't an archive, and opens
/// in its app.
const COMPRESSIONS: [&str; 5] = ["gz", "bz2", "xz", "zst", "lz4"];
/// What opening mounts, by extension.
const DISK_IMAGES: [&str; 5] = ["dmg", "iso", "img", "sparseimage", "sparsebundle"];

/// The category and kind of a file named `name`.
pub(super) fn describe(name: &str, folder: bool, package: bool, executable: bool) -> (Category, SharedString) {
    use Category::*;
    if folder {
        return (Folder, "Folder".into());
    }
    let Some(ext) = extension(name) else {
        return if executable { (Executable, "Unix executable".into()) } else { (Other, "Document".into()) };
    };
    let upper = ext.to_ascii_uppercase();
    let (category, kind): (Category, String) = match ext.as_str() {
        "app" => (App, "Application".into()),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "tiff" | "tif" | "bmp" | "ico" | "icns" | "psd"
        | "avif" | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" => (Image, format!("{upper} image")),
        "svg" => (Image, "SVG image".into()),
        "mov" => (Video, "QuickTime movie".into()),
        "mp4" | "m4v" | "mkv" | "avi" | "webm" | "wmv" | "flv" | "mpg" | "mpeg" => (Video, format!("{upper} video")),
        "mp3" | "m4a" | "wav" | "aiff" | "aif" | "flac" | "ogg" | "opus" | "aac" | "mid" | "midi" | "caf" => {
            (Audio, format!("{upper} audio"))
        }
        _ if extracts(name) => (Archive, format!("{upper} archive")),
        ext if COMPRESSIONS.contains(&ext) => (Archive, format!("{upper} compressed file")),
        ext if DISK_IMAGES.contains(&ext) => (DiskImage, "Disk image".into()),
        "ttf" | "otf" | "woff" | "woff2" | "ttc" => (Font, format!("{upper} font")),
        "pdf" => (Pdf, "PDF document".into()),
        "doc" | "docx" => (Document, "Word document".into()),
        "pages" => (Document, "Pages document".into()),
        "rtf" => (Document, "Rich text document".into()),
        "xls" | "xlsx" => (Spreadsheet, "Excel spreadsheet".into()),
        "numbers" => (Spreadsheet, "Numbers spreadsheet".into()),
        "csv" | "tsv" => (Spreadsheet, format!("{upper} table")),
        "ppt" | "pptx" => (Presentation, "PowerPoint presentation".into()),
        "key" => (Presentation, "Keynote presentation".into()),
        "txt" => (Text, "Plain text".into()),
        "log" => (Text, "Log file".into()),
        "md" | "markdown" => (Text, "Markdown".into()),
        ext => match code_language(ext) {
            Some(language) => (Code, language.into()),
            None if executable => (Executable, format!("{upper} executable")),
            None if package => (Other, format!("{upper} package")),
            None => (Other, format!("{upper} file")),
        },
    };
    (category, kind.into())
}

fn code_language(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "Rust source",
        "ts" | "mts" | "cts" => "TypeScript",
        "tsx" => "TypeScript JSX",
        "js" | "mjs" | "cjs" => "JavaScript",
        "jsx" => "JavaScript JSX",
        "py" => "Python script",
        "go" => "Go source",
        "c" => "C source",
        "h" => "C header",
        "cpp" | "cc" | "cxx" => "C++ source",
        "hpp" | "hh" => "C++ header",
        "m" => "Objective-C source",
        "swift" => "Swift source",
        "java" => "Java source",
        "kt" | "kts" => "Kotlin source",
        "cs" => "C# source",
        "rb" => "Ruby script",
        "php" => "PHP script",
        "lua" => "Lua script",
        "zig" => "Zig source",
        "dart" => "Dart source",
        "ex" | "exs" => "Elixir source",
        "hs" => "Haskell source",
        "sh" | "bash" | "zsh" | "fish" => "Shell script",
        "html" | "htm" => "HTML document",
        "css" | "scss" | "sass" | "less" => "Stylesheet",
        "vue" => "Vue component",
        "svelte" => "Svelte component",
        "astro" => "Astro component",
        "json" | "jsonc" => "JSON",
        "toml" => "TOML",
        "yaml" | "yml" => "YAML",
        "xml" | "plist" => "XML",
        "sql" => "SQL",
        "lock" => "Lockfile",
        "gd" => "GDScript",
        "glsl" | "wgsl" | "metal" | "hlsl" => "Shader",
        _ => return None,
    })
}

/// Up to `limit` bytes from the start of the file at `path`, if it's text. Only a regular file is
/// read, as reading a pipe or a device could wait forever, and one is binary, as git has it, with a
/// NUL in its first 8000 bytes.
pub fn read_text(path: &Path, limit: u64) -> Result<Vec<u8>> {
    if !fs::metadata(path)?.is_file() {
        bail!("This isn't a regular file.");
    }
    let mut file = File::open(path)?.take(limit);
    let mut bytes = Vec::new();
    (&mut file).take(8000).read_to_end(&mut bytes)?;
    if bytes.contains(&0) {
        bail!("This is a binary file.");
    }
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Whether opening `path` mounts it.
pub fn is_disk_image(path: &Path) -> bool {
    extension(&disk_name(path).to_string_lossy()).is_some_and(|ext| DISK_IMAGES.contains(&ext.as_str()))
}

/// Whether opening `path` extracts it.
pub fn is_archive(path: &Path) -> bool {
    extracts(&disk_name(path).to_string_lossy())
}

/// Whether opening a file named `name` extracts it.
fn extracts(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    match name.rsplit_once('.') {
        Some((_, ext)) if ARCHIVES.contains(&ext) => true,
        Some((stem, ext)) if COMPRESSIONS.contains(&ext) => stem.ends_with(".tar"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_are_what_opening_extracts() {
        let kind = |name| describe(name, false, false, false);
        assert_eq!(kind("x.tbz"), (Category::Archive, "TBZ archive".into()));
        assert_eq!(kind("x.tar.gz"), (Category::Archive, "GZ archive".into()));
        assert_eq!(kind("x.gz"), (Category::Archive, "GZ compressed file".into()));
        assert_eq!(kind("x.dmg"), (Category::DiskImage, "Disk image".into()));
        assert!(is_archive(Path::new("x.TAR.GZ")) && !is_archive(Path::new("x.txt.gz")));
        assert!(is_disk_image(Path::new("x.ISO")) && !is_disk_image(Path::new("x.zip")));
    }
}
