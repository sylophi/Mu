//! Files on the macOS pasteboard, so copying in Mu pastes in Finder, Mail and the rest, and the
//! other way round. And pictures and text copied anywhere, which paste into a folder as new files.

use std::path::PathBuf;

use objc2::{
    rc::{Retained, autoreleasepool},
    runtime::ProtocolObject,
};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString, NSURL};

use crate::fs::file_url;

/// Puts `paths` on the pasteboard and returns its change count afterwards.
pub fn write_files(paths: &[PathBuf]) -> isize {
    let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> =
        paths.iter().map(|path| ProtocolObject::from_retained(file_url(path))).collect();
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    pasteboard.writeObjects(&NSArray::from_retained_slice(&objects));
    pasteboard.changeCount()
}

/// The files that are copied, here or in another app.
pub fn files() -> Vec<PathBuf> {
    let Some(items) = NSPasteboard::generalPasteboard().pasteboardItems() else { return Vec::new() };
    let kind = NSString::from_str("public.file-url");
    items
        .iter()
        .filter_map(|item| {
            let url = NSURL::URLWithString(&*item.stringForType(&kind)?)?;
            url.filePathURL()?.to_file_path()
        })
        .collect()
}

/// Goes up whenever anything is copied, here or in another app.
pub fn change_count() -> isize {
    NSPasteboard::generalPasteboard().changeCount()
}

/// What pastes as a new file, by the type it's copied as, and that file's name.
const CONTENTS: [(&str, &str); 4] = [
    ("public.png", "Pasted image.png"),
    ("public.tiff", "Pasted image.tiff"),
    ("public.jpeg", "Pasted image.jpg"),
    ("public.utf8-plain-text", "Pasted text.txt"),
];

/// The first type of what's copied that pastes as a file, in the order the app that copied it
/// lists them, which puts what it is before what it can also be read as. With the file's name.
fn pastable() -> Option<(Retained<NSPasteboardItem>, Retained<NSString>, &'static str)> {
    let item = NSPasteboard::generalPasteboard().pasteboardItems()?.firstObject()?;
    let (kind, name) = item.types().iter().find_map(|kind| {
        let name = CONTENTS.iter().find(|(copied, _)| kind.to_string() == *copied)?.1;
        Some((kind, name))
    })?;
    Some((item, kind, name))
}

/// Whether a picture or text is copied, to paste as a file.
pub fn has_contents() -> bool {
    pastable().is_some()
}

/// A picture or text that's copied, to paste as a file.
pub struct Contents {
    name: &'static str,
    data: Vec<u8>,
    tiff: bool,
}

/// The picture or text that's copied, if that's what it is.
pub fn contents() -> Option<Contents> {
    let (item, kind, name) = pastable()?;
    let data = item.dataForType(&kind)?.to_vec();
    Some(Contents { name, data, tiff: kind.to_string() == "public.tiff" })
}

impl Contents {
    /// The file to paste it as: its name, and what's in it. A TIFF, which is how some apps copy
    /// pictures, is saved as a PNG, which takes a while for a big one.
    pub fn into_file(self) -> (&'static str, Vec<u8>) {
        if self.tiff
            && let Some(png) = png(&self.data)
        {
            return ("Pasted image.png", png);
        }
        (self.name, self.data)
    }
}

fn png(tiff: &[u8]) -> Option<Vec<u8>> {
    autoreleasepool(|_| {
        let image = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(tiff))?;
        let png =
            unsafe { image.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
        Some(png.to_vec())
    })
}
