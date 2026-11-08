//! Reads the `.DS_Store` files Finder keeps in folders: a B-tree of records, in blocks laid out by a
//! buddy allocator.

use std::collections::HashSet;

/// A record in a .DS_Store: who it's for (an item's name, or "." for the folder), which one it is,
/// and its value's bytes.
pub type Record<'a> = (String, [u8; 4], &'a [u8]);

pub fn records(data: &[u8]) -> Option<Vec<Record<'_>>> {
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
    let (mut nodes, mut seen) = (vec![be32(data, tree)?], HashSet::new());
    while let Some(id) = nodes.pop() {
        let at = *blocks.get(id)? & !0x1f;
        // A file that loops back on itself is read once around.
        if seen.insert(at) {
            node(data, at, &mut nodes, &mut records)?;
        }
    }
    Some(records)
}

/// Reads the records in the node at `at`, and adds the nodes below it to `nodes`.
fn node<'a>(data: &'a [u8], mut at: usize, nodes: &mut Vec<usize>, records: &mut Vec<Record<'a>>) -> Option<()> {
    // A node with nodes below it says so with the last of them, and has one before each record.
    let (last, count) = (be32(data, at)?, be32(data, at + 4)?);
    at += 8;
    if last != 0 {
        nodes.push(last);
    }
    for _ in 0..count {
        if last != 0 {
            nodes.push(be32(data, at)?);
            at += 4;
        }
        let length = be32(data, at)? * 2;
        let name = text(data.get(at + 4..at + 4 + length)?);
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
        records.push((name, code, data.get(at..at + length)?));
        at += length;
    }
    Some(())
}

/// What a string record's value says.
pub fn text(value: &[u8]) -> String {
    let units: Vec<u16> = value.as_chunks::<2>().0.iter().map(|&pair| u16::from_be_bytes(pair)).collect();
    String::from_utf16_lossy(&units)
}

pub fn be32(data: &[u8], at: usize) -> Option<usize> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// From a real Trash: Finder moved these items there, from a disk made for this.
    const FIXTURE: &[u8] = include_bytes!("fixtures/finder-trash.ds_store");

    #[test]
    fn reads_what_finder_wrote() {
        let found = records(FIXTURE).unwrap();
        assert_eq!(found.len(), 652);
        let value = |name: &str, code: &[u8; 4]| {
            found.iter().find(|(n, c, _)| n == name && c == code).map(|(_, _, value)| text(value))
        };

        // Names stay as they are on the disk, whichever way their accents are composed.
        assert_eq!(value("cafe\u{301}-nfd.txt", b"ptbL").as_deref(), Some("/uni/"));
        assert_eq!(value("th\u{e9}-nfc.txt", b"ptbN").as_deref(), Some("th\u{e9}-nfc.txt"));
        assert_eq!(value("missing", b"ptbL"), None);

        // Enough of them that the records are spread over a tree, rather than sitting in one block.
        for index in 0..320 {
            let name = format!("item-{index:03}-with-a-longer-name-to-fill-the-tree.txt");
            assert_eq!(value(&name, b"ptbL").as_deref(), Some("/bulk/"));
            assert_eq!(value(&name, b"ptbN"), Some(name));
        }
    }

    #[test]
    fn makes_nothing_of_what_it_cant_read() {
        for file in [&b""[..], b"not a store", &FIXTURE[..FIXTURE.len() / 2], &FIXTURE[..64]] {
            assert!(records(file).is_none());
        }
    }
}
