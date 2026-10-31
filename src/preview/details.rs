//! What the preview pane lists about an item past what listing its folder tells, like an image's
//! dimensions, a song's artist, a PDF's pages, or the address a download came from. Each is read
//! from the start of the file or what macOS keeps about it, without decoding what it holds.

use std::{
    io::Read as _,
    path::{Path, PathBuf},
    time::SystemTime,
};

use objc2_av_foundation::{AVAssetTrack, AVURLAsset};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFURL};
use objc2_core_graphics::{CGPDFBox, CGPDFDictionary, CGPDFDocument, CGPDFPage, CGPDFString, CGPDFStringRef};
use objc2_core_media::{CMAudioFormatDescriptionGetStreamBasicDescription, CMFormatDescription};
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontManagerCreateFontDescriptorsFromURL, kCTFontStyleNameAttribute,
    kCTFontVersionNameKey,
};
use objc2_foundation::{
    NSArray, NSBundle, NSData, NSPropertyListMutabilityOptions, NSPropertyListSerialization, NSString,
};
use objc2_image_io::CGImageSource;

use crate::fs::{self, Category, Entry};

/// A label and its value.
pub type Detail = (&'static str, String);

#[derive(Default)]
pub struct Details {
    /// What it is, like an image's dimensions or an app's version.
    pub about: Vec<Detail>,
    pub created: Option<SystemTime>,
    /// The address it was downloaded from.
    pub source: Option<String>,
}

pub fn read(entry: &Entry) -> Details {
    let path = entry.target();
    let about = match entry.category {
        // Reading what's only in iCloud would download it.
        _ if entry.cloud_only => Vec::new(),
        _ if entry.package => bundle(path),
        Category::Image => image(path),
        Category::Video | Category::Audio => media(path),
        Category::Pdf => pdf(path),
        Category::Font => font(path),
        Category::Executable | Category::Other => architectures(path).into_iter().collect(),
        _ => Vec::new(),
    };
    let created = std::fs::metadata(path).and_then(|metadata| metadata.created()).ok();
    Details { about, created, source: source(path) }
}

/// An app's version, what it needs and what it's built for, from its Info.plist.
fn bundle(path: &Path) -> Vec<Detail> {
    let Some(bundle) = NSBundle::bundleWithPath(&NSString::from_str(&path.to_string_lossy())) else {
        return Vec::new();
    };
    let info = |key| {
        let value = bundle.objectForInfoDictionaryKey(&NSString::from_str(key))?;
        Some(value.downcast::<NSString>().ok()?.to_string())
    };
    let mut details = Vec::new();
    details.extend(info("CFBundleShortVersionString").map(|version| ("Version", version)));
    details.extend(bundle.bundleIdentifier().map(|id| ("Identifier", id.to_string())));
    details.extend(info("LSMinimumSystemVersion").map(|version| ("Requires", format!("macOS {version}"))));
    let executable = bundle.executablePath().map(|path| PathBuf::from(path.to_string()));
    details.extend(executable.and_then(|path| architectures(&path)));
    details
}

/// The Macs a program runs on natively, by the processors its Mach-O header names.
fn architectures(path: &Path) -> Option<Detail> {
    let mut header = Vec::with_capacity(1024);
    std::fs::File::open(path).ok()?.take(1024).read_to_end(&mut header).ok()?;
    let word = |at: usize| Some(u32::from_be_bytes(header.get(at..at + 4)?.try_into().ok()?));
    let cpus: Vec<u32> = match word(0)? {
        // A universal binary, which lists each one's processor, 64-bit offsets taking 32 bytes. A
        // Java class starts the same, then has its version of 45 or more where the count would be.
        magic @ (0xcafebabe | 0xcafebabf) => {
            let stride = if magic == 0xcafebabf { 32 } else { 20 };
            let count = word(4).filter(|&count| count < 45)?;
            (0..count as usize).map_while(|ix| word(8 + ix * stride)).collect()
        }
        // A single one, which is little-endian.
        0xcffaedfe | 0xcefaedfe => vec![word(4)?.swap_bytes()],
        _ => return None,
    };
    // By CPU type, less the bit saying it's 64-bit.
    let apple = cpus.iter().any(|cpu| cpu & 0xff == 12);
    let intel = cpus.iter().any(|cpu| cpu & 0xff == 7);
    let built = match (apple, intel) {
        (true, true) => "Apple silicon and Intel",
        (true, false) => "Apple silicon",
        (false, true) => "Intel",
        (false, false) => return None,
    };
    Some(("Built for", built.into()))
}

/// The address Safari, Chrome and the like note on what they download.
fn source(path: &Path) -> Option<String> {
    const WHERE_FROMS: &std::ffi::CStr = c"com.apple.metadata:kMDItemWhereFroms";
    let value = fs::xattr(path, WHERE_FROMS, 0)?;
    let data = NSData::with_bytes(&value);
    let options = NSPropertyListMutabilityOptions::Immutable;
    let froms = unsafe {
        NSPropertyListSerialization::propertyListWithData_options_format_error(&data, options, std::ptr::null_mut())
    }
    .ok()?;
    // The file's own address, then the page that linked to it.
    let address = froms.downcast::<NSArray>().ok()?.firstObject()?.downcast::<NSString>().ok()?.to_string();
    // Less its query, which can run longer than the rest.
    let address = address.split(['?', '#']).next()?;
    (!address.is_empty()).then(|| address.to_owned())
}

/// An image's dimensions and color, and how a photo was taken.
fn image(path: &Path) -> Vec<Detail> {
    let Some(image) = CFURL::from_file_path(path)
        .and_then(|url| unsafe { CGImageSource::with_url(&url, None) })
        .and_then(|source| unsafe { source.properties_at_index(0, None) })
    else {
        return Vec::new();
    };
    let image = Properties(unsafe { CFRetained::cast_unchecked(image) });
    let exif = image.dictionary("{Exif}");
    let exif = |key| exif.as_ref()?.get(key);
    let tiff = image.dictionary("{TIFF}");
    let tiff = |key| tiff.as_ref()?.string(key);
    let mut details = Vec::new();
    if let (Some(width), Some(height)) = (image.number("PixelWidth"), image.number("PixelHeight")) {
        // Orientations 5 to 8 turn it on its side.
        let turned = image.number("Orientation").is_some_and(|orientation| orientation >= 5.);
        details.push(("Dimensions", dimensions(width, height, turned)));
    }
    details.extend(image.string("ProfileName").or_else(|| image.string("ColorModel")).map(|color| ("Color", color)));
    if let Some(depth) = image.number("Depth") {
        let alpha =
            image.get("HasAlpha").and_then(|has| has.downcast::<CFBoolean>().ok()).is_some_and(|has| has.as_bool());
        let alpha = if alpha { " with alpha" } else { "" };
        details.push(("Depth", format!("{depth}-bit{alpha}")));
    }
    let camera = match (tiff("Make"), tiff("Model")) {
        // Most models start with their make already, like "Canon EOS R5", or "NIKON D90" of
        // "NIKON CORPORATION".
        (Some(make), Some(model)) if !model.starts_with(make.split(' ').next().unwrap_or_default()) => {
            Some(format!("{make} {model}"))
        }
        (make, model) => model.or(make),
    };
    details.extend(camera.map(|camera| ("Camera", camera)));
    details.extend(exif("LensModel").and_then(|lens| string(&lens)).map(|lens| ("Lens", lens)));
    let exif_number = |key| exif(key).and_then(|value| number(&value));
    let focal_length = exif_number("FocalLength").map(|length| format!("{} mm", tenths(length)));
    details.extend(focal_length.map(|length| ("Focal length", length)));
    details.extend(exif_number("FNumber").map(|number| ("Aperture", format!("f/{}", tenths(number)))));
    let exposure = exif_number("ExposureTime").map(|time| match time < 1. {
        true => format!("1/{} s", (1. / time).round()),
        false => format!("{time} s"),
    });
    details.extend(exposure.map(|time| ("Exposure", time)));
    // One rating for each of the camera's sensitivity settings, the first of which it used.
    let iso = exif("ISOSpeedRatings")
        .and_then(|ratings| ratings.downcast::<CFArray>().ok())
        .and_then(|ratings| unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(ratings) }.get(0))
        .and_then(|iso| number(&iso));
    details.extend(iso.map(|iso| ("ISO", iso.to_string())));
    // In the camera's time zone, which the photo seldom says, so it's taken as this Mac's.
    let taken = exif("DateTimeOriginal")
        .and_then(|taken| chrono::NaiveDateTime::parse_from_str(&string(&taken)?, "%Y:%m:%d %H:%M:%S").ok())
        .and_then(|taken| taken.and_local_timezone(chrono::Local).earliest());
    details.extend(taken.map(|taken| ("Taken", fs::format_date(taken.into()))));
    details
}

/// One of ImageIO's dictionaries of an image's properties.
struct Properties(CFRetained<CFDictionary<CFString, CFType>>);

impl Properties {
    fn get(&self, key: &str) -> Option<CFRetained<CFType>> {
        self.0.get(&CFString::from_str(key))
    }

    fn number(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(|value| number(&value))
    }

    fn string(&self, key: &str) -> Option<String> {
        self.get(key).and_then(|value| string(&value))
    }

    fn dictionary(&self, key: &str) -> Option<Properties> {
        let dictionary = self.get(key)?.downcast::<CFDictionary>().ok()?;
        Some(Properties(unsafe { CFRetained::cast_unchecked(dictionary) }))
    }
}

fn number(value: &CFType) -> Option<f64> {
    value.downcast_ref::<CFNumber>()?.as_f64()
}

fn string(value: &CFType) -> Option<String> {
    Some(value.downcast_ref::<CFString>()?.to_string())
}

/// A song's or a movie's title and length, and how its picture and sound are encoded.
fn media(path: &Path) -> Vec<Detail> {
    let asset = unsafe { AVURLAsset::URLAssetWithURL_options(&fs::file_url(path), None) };
    // Each of these loads as it's asked for, which on this thread is just what's wanted.
    let (duration, tracks, metadata) = unsafe { (asset.duration(), asset.tracks(), asset.commonMetadata()) };
    let mut details = Vec::new();
    for (key, label) in [("title", "Title"), ("artist", "Artist"), ("albumName", "Album")] {
        let item =
            metadata.iter().find(|item| unsafe { item.commonKey() }.is_some_and(|common| common.to_string() == key));
        details.extend(item.and_then(|item| unsafe { item.stringValue() }).map(|value| (label, value.to_string())));
    }
    let seconds = unsafe { duration.seconds() };
    if seconds.is_finite() && seconds > 0. {
        details.push(("Duration", clock(seconds)));
    }
    let track = |kind: &str| tracks.iter().find(|track| unsafe { track.mediaType() }.to_string() == kind);
    if let Some(video) = track("vide") {
        let size = unsafe { video.naturalSize() };
        // Shot on its side, as phones held upright are, it plays turned.
        let turned = unsafe { video.preferredTransform() }.a == 0.;
        details.push(("Dimensions", dimensions(size.width, size.height, turned)));
        let rate = unsafe { video.nominalFrameRate() } as f64;
        if rate > 0. {
            details.push(("Frame rate", format!("{} fps", (rate * 100.).round() / 100.)));
        }
        details.extend(format(&video).map(|format| ("Video", codec(unsafe { format.media_sub_type() }))));
    }
    if let Some(format) = track("soun").and_then(|audio| format(&audio)) {
        let mut parts = vec![codec(unsafe { format.media_sub_type() })];
        if let Some(stream) = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&format).as_ref() } {
            // Only what's uncompressed, like PCM or Apple Lossless, has a bit depth.
            if stream.mBitsPerChannel > 0 {
                parts.push(format!("{}-bit", stream.mBitsPerChannel));
            }
            parts.push(format!("{} kHz", stream.mSampleRate / 1000.));
            parts.push(match stream.mChannelsPerFrame {
                1 => "mono".into(),
                2 => "stereo".into(),
                6 => "5.1".into(),
                8 => "7.1".into(),
                channels => format!("{channels} channels"),
            });
        }
        details.push(("Audio", parts.join(", ")));
    }
    let rate: f64 = tracks.iter().map(|track| unsafe { track.estimatedDataRate() } as f64).sum();
    if rate > 0. {
        details.push(("Bit rate", bit_rate(rate)));
    }
    details
}

/// How `track` is encoded.
fn format(track: &AVAssetTrack) -> Option<CFRetained<CMFormatDescription>> {
    let format = unsafe { track.formatDescriptions() }.firstObject()?;
    // The array holds CMFormatDescriptions, which as Core Foundation types bridge to objects.
    Some(unsafe { CFRetained::retain(std::ptr::NonNull::from(&*format).cast()) })
}

/// The name of an encoding, by its four-character code.
fn codec(code: u32) -> String {
    let code = code.to_be_bytes();
    match &code {
        b"avc1" | b"avc3" => "H.264".into(),
        b"hvc1" | b"hev1" => "HEVC".into(),
        b"dvh1" | b"dvhe" => "Dolby Vision".into(),
        b"av01" => "AV1".into(),
        b"vp09" => "VP9".into(),
        b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" | b"aprn" | b"aprh" => "ProRes".into(),
        b"mp4v" => "MPEG-4".into(),
        b"jpeg" => "Motion JPEG".into(),
        b"aac " => "AAC".into(),
        b".mp3" => "MP3".into(),
        b"alac" => "Apple Lossless".into(),
        b"flac" | b"fLaC" => "FLAC".into(),
        b"opus" => "Opus".into(),
        b"lpcm" => "PCM".into(),
        b"ac-3" => "Dolby Digital".into(),
        b"ec-3" => "Dolby Digital Plus".into(),
        _ => String::from_utf8_lossy(&code).trim().to_owned(),
    }
}

/// A PDF's pages, their size, and who made it with what.
fn pdf(path: &Path) -> Vec<Detail> {
    let Some(document) = CFURL::from_file_path(path).and_then(|url| CGPDFDocument::with_url(Some(&url))) else {
        return Vec::new();
    };
    let mut details = Vec::new();
    let pages = CGPDFDocument::number_of_pages(Some(&document));
    if pages > 0 {
        details.push(("Pages", pages.to_string()));
    }
    if let Some(page) = CGPDFDocument::page(Some(&document), 1) {
        let size = CGPDFPage::box_rect(Some(&page), CGPDFBox::MediaBox).size;
        let turned = CGPDFPage::rotation_angle(Some(&page)) % 180 != 0;
        let (width, height) = if turned { (size.height, size.width) } else { (size.width, size.height) };
        details.push(("Page size", paper(width, height)));
    }
    let info = CGPDFDocument::info(Some(&document));
    let text = |key: &std::ffi::CStr| {
        let mut value: CGPDFStringRef = std::ptr::null_mut();
        if info.is_null() || !unsafe { CGPDFDictionary::string(info, std::ptr::NonNull::from(key).cast(), &mut value) }
        {
            return None;
        }
        let text = unsafe { CGPDFString::text_string(value) }?.to_string();
        Some(text.trim().to_owned()).filter(|text| !text.is_empty())
    };
    details.extend(text(c"Title").map(|title| ("Title", title)));
    details.extend(text(c"Author").map(|author| ("Author", author)));
    // The app it was made in, or else what turned it into a PDF.
    details.extend(text(c"Creator").or_else(|| text(c"Producer")).map(|app| ("Made with", app)));
    details
}

/// A page's size, by its paper's name if it's a common one, or else in millimeters.
fn paper(width: f64, height: f64) -> String {
    const PAPERS: [(&str, f64, f64); 6] = [
        ("A3", 842., 1191.),
        ("A4", 595., 842.),
        ("A5", 420., 595.),
        ("Letter", 612., 792.),
        ("Legal", 612., 1008.),
        ("Tabloid", 792., 1224.),
    ];
    let (short, long) = (width.min(height), width.max(height));
    let landscape = if width > height { ", landscape" } else { "" };
    match PAPERS.iter().find(|(_, w, h)| (short - w).abs() < 2. && (long - h).abs() < 2.) {
        Some((name, ..)) => format!("{name}{landscape}"),
        // 72 points to an inch.
        None => format!("{} × {} mm", (width / 72. * 25.4).round(), (height / 72. * 25.4).round()),
    }
}

/// A font's family, its styles, version and how many glyphs it has. A collection holds several.
fn font(path: &Path) -> Vec<Detail> {
    let Some(descriptors) =
        CFURL::from_file_path(path).and_then(|url| unsafe { CTFontManagerCreateFontDescriptorsFromURL(&url) })
    else {
        return Vec::new();
    };
    let descriptors = unsafe { CFRetained::cast_unchecked::<CFArray<CTFontDescriptor>>(descriptors) };
    let Some(first) = descriptors.get(0) else { return Vec::new() };
    let first = unsafe { CTFont::with_font_descriptor(&first, 12., std::ptr::null()) };
    let mut details = vec![("Family", unsafe { first.family_name() }.to_string())];
    let styles: Vec<String> = descriptors
        .iter()
        .filter_map(|descriptor| unsafe { descriptor.attribute(kCTFontStyleNameAttribute) })
        .filter_map(|style| string(&style))
        .collect();
    if !styles.is_empty() {
        details.push((if styles.len() == 1 { "Style" } else { "Styles" }, styles.join(", ")));
    }
    let version = unsafe { first.name(kCTFontVersionNameKey) }.map(|version| version.to_string());
    // Fonts mostly say "Version 1.002", and some more after a semicolon.
    let version =
        version.map(|version| version.trim_start_matches("Version ").split(';').next().unwrap_or_default().to_owned());
    details.extend(version.filter(|version| !version.is_empty()).map(|version| ("Version", version)));
    details.push(("Glyphs", unsafe { first.glyph_count() }.to_string()));
    details
}

/// `width` by `height`, or the other way around when `turned`.
fn dimensions(width: f64, height: f64, turned: bool) -> String {
    let (width, height) = if turned { (height, width) } else { (width, height) };
    format!("{} × {}", width.round(), height.round())
}

/// `value` to a tenth, as an f-number or a focal length is written.
fn tenths(value: f64) -> f64 {
    (value * 10.).round() / 10.
}

/// `seconds` as a clock shows them, like 3:07 or 1:02:09.
fn clock(seconds: f64) -> String {
    let seconds = seconds.round() as u64;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    match hours {
        0 => format!("{minutes}:{seconds:02}"),
        _ => format!("{hours}:{minutes:02}:{seconds:02}"),
    }
}

/// Bits a second, in kilobits or megabits.
fn bit_rate(rate: f64) -> String {
    match rate >= 1e6 {
        true => format!("{} Mbit/s", tenths(rate / 1e6)),
        false => format!("{} kbit/s", (rate / 1e3).round()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn papers() {
        assert_eq!(paper(595.3, 841.9), "A4");
        assert_eq!(paper(792., 612.), "Letter, landscape");
        assert_eq!(paper(360., 360.), "127 × 127 mm");
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(42.4), "0:42");
        assert_eq!(clock(187.), "3:07");
        assert_eq!(clock(3729.), "1:02:09");
    }

    #[test]
    fn codecs() {
        assert_eq!(codec(u32::from_be_bytes(*b"hvc1")), "HEVC");
        assert_eq!(codec(u32::from_be_bytes(*b"xyz ")), "xyz");
    }
}
