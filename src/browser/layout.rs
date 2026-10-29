//! How Finder lays out a folder that has a picture behind its icons, as a disk image's window
//! usually does: the picture, how big the icons and their names are, and where each one goes.
//! Finder keeps it in the folder's .DS_Store.

use std::{collections::HashMap, fs, path::Path, sync::Arc};

use gpui_kit::{Bounds, Pixels, Point, RenderImage, Size, point, px, size};
use image::Frame;
use objc2::{
    AnyThread as _,
    rc::{Retained, autoreleasepool},
};
use objc2_app_kit::{NSGraphicsContext, NSImage};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSData, NSDictionary, NSNumber, NSPropertyListMutabilityOptions, NSPropertyListSerialization, NSString, ns_string,
};

use crate::{fs::volume_root, thumbnails};

pub struct Layout {
    /// The picture, drawn for the screen.
    pub picture: Arc<RenderImage>,
    /// How big the picture is, in points.
    pub size: Size<Pixels>,
    /// Whether the picture is light, so names on it read better dark, or None when it's mostly
    /// see-through, so they're as usual.
    pub light: Option<bool>,
    pub icon_size: Pixels,
    pub text_size: Pixels,
    /// Where each item's icon is centered, by name.
    positions: HashMap<String, Point<Pixels>>,
}

impl Layout {
    /// The layout of the folder at `dir`, with its picture drawn `scale` pixels to a point. None
    /// when Finder hasn't given it a picture.
    pub fn read(dir: &Path, scale: f32) -> Option<Self> {
        let data = fs::read(dir.join(".DS_Store")).ok()?;
        let records = records(&data)?;
        let (_, _, view) = records.iter().find(|(name, code, _)| name == "." && code == b"icvp")?;
        // What AppKit leaves to be let go of later goes here, as this runs off the main thread.
        let (picture, size, light, icon_size, text_size) = autoreleasepool(|_| {
            let view = plist(view)?;
            // A picture. The others are plain white and a color.
            if number(&view, ns_string!("backgroundType"))? != 2. {
                return None;
            }
            let alias = view.objectForKey(ns_string!("backgroundImageAlias"))?.downcast::<NSData>().ok()?.to_vec();
            let (picture, size, light) = draw(&volume_root(dir)?.join(alias_path(&alias)?), scale)?;
            let (icon_size, text_size) = (
                number(&view, ns_string!("iconSize")).unwrap_or(64.),
                number(&view, ns_string!("textSize")).unwrap_or(12.),
            );
            Some((picture, size, light, px(icon_size as f32), px(text_size as f32)))
        })?;
        let positions = records
            .iter()
            .filter(|(_, code, _)| code == b"Iloc")
            .filter_map(|(name, _, at)| Some((name.clone(), point(px(be32(at, 0)? as f32), px(be32(at, 4)? as f32)))))
            .collect();
        Some(Self { picture, size, light, icon_size, text_size, positions })
    }

    /// Where the icons of the items named `names` are centered: where Finder put them, or for any
    /// it hasn't, in rows below the rest.
    pub fn spots<'a>(&self, names: impl Iterator<Item = &'a str>) -> Vec<Point<Pixels>> {
        let step = self.tile_width();
        let bottom = self.positions.values().map(|at| at.y).reduce(Pixels::max).unwrap_or(-step / 2.);
        let per_row = ((self.size.width / step) as usize).max(1);
        let mut unplaced = 0;
        names
            .map(|name| match self.positions.get(name) {
                Some(&at) => at,
                None => {
                    let (row, column) = (unplaced / per_row, unplaced % per_row);
                    unplaced += 1;
                    point(step * (column as f32 + 0.5), bottom + step * (row as f32 + 1.))
                }
            })
            .collect()
    }

    /// How wide an item is, its name below its icon.
    pub fn tile_width(&self) -> Pixels {
        self.icon_size + self.text_size * 2. + px(16.)
    }

    /// Where the item whose icon is centered at `at` is, with room for two lines of its name.
    pub fn tile(&self, at: Point<Pixels>) -> Bounds<Pixels> {
        let width = self.tile_width();
        // The icon sits inside the tile's border and padding.
        let origin = point(at.x - width / 2., at.y - self.icon_size / 2. - px(5.));
        Bounds::new(origin, size(width, self.icon_size + self.text_size * 3. + px(14.)))
    }
}

/// A record in a .DS_Store: who it's for (an item's name, or "." for the folder), which one it is,
/// and its value's bytes.
type Record<'a> = (String, [u8; 4], &'a [u8]);

fn records(data: &[u8]) -> Option<Vec<Record<'_>>> {
    // Where things are counts from after the first 4 bytes.
    let data = data.get(4..)?;
    if data.get(..4)? != b"Bud1" {
        return None;
    }
    // The allocator's own block: where each block is, then which block holds what.
    let root = be32(data, 4)?;
    let count = be32(data, root)?;
    let blocks: Vec<usize> = (0..count).map(|ix| be32(data, root + 8 + 4 * ix)).collect::<Option<_>>()?;
    let mut at = root + 8 + 4 * count.div_ceil(256) * 256;
    let mut directory = None;
    for _ in 0..be32(data, at)? {
        let length = *data.get(at + 4)? as usize;
        if data.get(at + 5..at + 5 + length)? == b"DSDB" {
            directory = Some(be32(data, at + 5 + length)?);
        }
        at += 5 + length;
    }
    let tree = *blocks.get(directory?)? & !0x1f;
    let mut records = Vec::new();
    node(data, &blocks, be32(data, tree)?, be32(data, tree + 4)?, &mut records)?;
    Some(records)
}

/// Reads the records in node `id` of the tree and in the nodes below it, `levels` deep.
fn node<'a>(data: &'a [u8], blocks: &[usize], id: usize, levels: usize, records: &mut Vec<Record<'a>>) -> Option<()> {
    let mut at = *blocks.get(id)? & !0x1f;
    // A node with nodes below it says so with the last of them, which follows its records.
    let (last, count) = (be32(data, at)?, be32(data, at + 4)?);
    let below = last != 0 && levels > 1;
    at += 8;
    for _ in 0..count {
        if below {
            node(data, blocks, be32(data, at)?, levels - 1, records)?;
            at += 4;
        }
        let length = be32(data, at)? * 2;
        let name: Vec<u16> = data
            .get(at + 4..at + 4 + length)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_be_bytes(pair))
            .collect();
        at += 4 + length;
        let code = data.get(at..at + 4)?.try_into().ok()?;
        let kind = data.get(at + 4..at + 8)?;
        at += 8;
        let length = match kind {
            b"bool" => 1,
            b"long" | b"shor" | b"type" => 4,
            b"comp" | b"dutc" => 8,
            // Blobs and strings say how long they are first, strings in UTF-16 units.
            b"blob" | b"ustr" => {
                at += 4;
                be32(data, at - 4)? * if kind == b"ustr" { 2 } else { 1 }
            }
            _ => return None,
        };
        records.push((String::from_utf16_lossy(&name), code, data.get(at..at + length)?));
        at += length;
    }
    if below {
        node(data, blocks, last, levels - 1, records)?;
    }
    Some(())
}

fn be32(data: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize)
}

/// Where an alias leads, from the root of its volume.
fn alias_path(alias: &[u8]) -> Option<String> {
    let be16 = |at: usize| Some(u16::from_be_bytes(alias.get(at..at + 2)?.try_into().ok()?));
    // Tagged parts follow a header, which version 3 made shorter.
    let mut at = match be16(6)? {
        2 => 150,
        3 => 58,
        _ => return None,
    };
    loop {
        let (tag, length) = (be16(at)?, be16(at + 2)? as usize);
        match tag {
            0xffff => return None,
            18 => {
                return Some(
                    String::from_utf8_lossy(alias.get(at + 4..at + 4 + length)?).trim_start_matches('/').into(),
                );
            }
            _ => at += 4 + length + length % 2,
        }
    }
}

fn plist(bytes: &[u8]) -> Option<Retained<NSDictionary>> {
    let data = NSData::with_bytes(bytes);
    let options = NSPropertyListMutabilityOptions::Immutable;
    unsafe {
        NSPropertyListSerialization::propertyListWithData_options_format_error(&data, options, std::ptr::null_mut())
    }
    .ok()?
    .downcast()
    .ok()
}

fn number(dictionary: &NSDictionary, key: &NSString) -> Option<f64> {
    Some(dictionary.objectForKey(key)?.downcast::<NSNumber>().ok()?.doubleValue())
}

/// The picture at `path`, drawn `scale` pixels to a point, how big it is in points, and whether
/// it's light, unless it's mostly see-through.
fn draw(path: &Path, scale: f32) -> Option<(Arc<RenderImage>, Size<Pixels>, Option<bool>)> {
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path.to_str()?))?;
    let points = image.size();
    let (width, height) = ((points.width * scale as f64) as usize, (points.height * scale as f64) as usize);
    let pixels = thumbnails::bitmap(width, height, |context| {
        NSGraphicsContext::setCurrentContext(Some(&NSGraphicsContext::graphicsContextWithCGContext_flipped(
            context, false,
        )));
        image.drawInRect(CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64)));
        NSGraphicsContext::setCurrentContext(None);
    })?;
    // Each pixel counts as much as it covers.
    let (mut lightness, mut cover) = (0., 0.);
    for &[blue, green, red, alpha] in pixels.as_raw().as_chunks::<4>().0 {
        let alpha = alpha as f64 / 255.;
        lightness += alpha * (0.0722 * blue as f64 + 0.7152 * green as f64 + 0.2126 * red as f64);
        cover += alpha;
    }
    let light = (cover > (width * height) as f64 / 2.).then(|| lightness / cover > 128.);
    let size = size(px(points.width as f32), px(points.height as f32));
    Some((Arc::new(RenderImage::new([Frame::new(pixels)])), size, light))
}

#[cfg(test)]
mod tests {
    use super::{alias_path, be32, records};

    /// A .DS_Store holding `records`, each a name, its code and type, and its value, in one node.
    fn store(records: &[(&str, &[u8; 8], &[u8])]) -> Vec<u8> {
        let mut data = vec![0; 0x1020];
        let mut put = |at: usize, value: u32| data[at..at + 4].copy_from_slice(&value.to_be_bytes());
        // The allocator's block, then the tree's, then its one node.
        put(4, 0x20);
        put(0x20, 3);
        put(0x28, 0x20 | 5);
        put(0x2c, 0x1000 | 5);
        put(0x30, 0x1020 | 5);
        put(0x428, 1);
        put(0x431, 1);
        put(0x1000, 2);
        put(0x1004, 1);
        data[..4].copy_from_slice(b"Bud1");
        data[0x42c] = 4;
        data[0x42d..0x431].copy_from_slice(b"DSDB");
        data.extend([0, 0, 0, 0]);
        data.extend((records.len() as u32).to_be_bytes());
        for (name, code, value) in records {
            data.extend((name.encode_utf16().count() as u32).to_be_bytes());
            data.extend(name.encode_utf16().flat_map(u16::to_be_bytes));
            data.extend(*code);
            data.extend(*value);
        }
        [&[0, 0, 0, 1], &data[..]].concat()
    }

    #[test]
    fn reads_records_and_aliases() {
        let at = [16, 120, 180, u32::MAX, 0].map(u32::to_be_bytes).concat();
        let data =
            store(&[(".", b"vSrnlong", &[0, 0, 0, 1]), ("Mü.app", b"Ilocblob", &at), ("Mü.app", b"dilcbool", &[1])]);
        let records = records(&data).unwrap();
        assert_eq!(records.len(), 3);
        let (name, code, at) = &records[1];
        assert_eq!((name.as_str(), code, be32(at, 0), be32(at, 4)), ("Mü.app", b"Iloc", Some(120), Some(180)));

        // A version 3 alias, past the volume's name, padded to an even length.
        let mut alias = vec![0; 58];
        alias[7] = 3;
        alias.extend([0, 0, 0, 5].iter().chain(b"dmg.x\0"));
        alias.extend([0, 18, 0, 27].iter().chain(b"/.background/background.png\0"));
        alias.extend([0xff, 0xff, 0, 0]);
        assert_eq!(alias_path(&alias).as_deref(), Some(".background/background.png"));
    }
}
