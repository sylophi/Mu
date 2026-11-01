//! Adding up what takes the room in a folder, the folders in it all at once, into a tree of what's
//! in it, biggest first.

use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    },
};

use rayon::prelude::*;
use smol::channel::{Receiver, Sender};

use crate::fs::{self, Shared, Usage};

/// Something in a folder and the room it takes.
pub(super) struct Node {
    pub(super) name: Box<OsStr>,
    pub(super) size: u64,
    pub(super) kind: Kind,
}

pub(super) enum Kind {
    File,
    /// What's in it, biggest first.
    Folder(Vec<Node>),
    /// This many files and folders, each too small to list on its own.
    Smaller(usize),
}

/// Files and folders under this are added up as one, so a folder of thousands of them takes one row
/// and none of the memory for theirs.
const SMALL: u64 = 1_000_000;

/// How far a scan has got, shared with the threads doing it, which stop once it's dropped.
#[derive(Default)]
pub(super) struct Counted {
    pub(super) bytes: AtomicU64,
    /// The bytes shared by clones or hard links already counted, which the first of them to be
    /// found takes, so they count once like they take up the disk once.
    shared: Mutex<HashSet<Shared>>,
    pub(super) stopped: AtomicBool,
}

/// Adds up what's in `root` on threads of its own, handing over each thing in it once it is. The
/// channel closes when everything is, or soon after `counted` is stopped.
pub(super) fn start(root: PathBuf, counted: Arc<Counted>) -> Receiver<Node> {
    let (found, nodes) = smol::channel::unbounded();
    std::thread::Builder::new()
        .name("storage scan".into())
        .spawn(move || {
            // A pool of the scan's own, whose threads end with it. Rayon's global pool would keep
            // them for as long as Mu runs.
            let pool = rayon::ThreadPoolBuilder::new().build().expect("the scan's threads start");
            pool.install(|| scan_root(&root, &counted, &found));
        })
        .expect("the scan's thread starts");
    nodes
}

/// Adds up what's in `root`, handing over each thing in it once it is.
fn scan_root(root: &Path, counted: &Counted, found: &Sender<Node>) {
    let (folders, files) = list(root, counted);
    for node in files {
        found.send_blocking(node).ok();
    }
    folders.into_par_iter().for_each(|folder| {
        found.send_blocking(scan_folder(root, folder, counted)).ok();
    });
}

/// Adds up `folder`, in `dir`, the folders in it all at once.
fn scan_folder(dir: &Path, folder: Usage, counted: &Counted) -> Node {
    let path = dir.join(&*folder.name);
    let (folders, files) = list(&path, counted);
    let folders: Vec<_> = folders.into_par_iter().map(|folder| scan_folder(&path, folder, counted)).collect();
    let mut nodes = Vec::new();
    for node in files.into_iter().chain(folders) {
        add(&mut nodes, node);
    }
    sort(&mut nodes);
    Node { name: folder.name, size: nodes.iter().map(|node| node.size).sum(), kind: Kind::Folder(nodes) }
}

/// Adds `node` to `nodes`, or if it's small, to the small ones there.
pub(super) fn add(nodes: &mut Vec<Node>, node: Node) {
    if node.size >= SMALL {
        return nodes.push(node);
    }
    match nodes.iter_mut().find(|node| matches!(node.kind, Kind::Smaller(_))) {
        Some(Node { size, kind: Kind::Smaller(count), .. }) => (*size, *count) = (*size + node.size, *count + 1),
        _ => nodes.push(Node { name: OsStr::new("").into(), size: node.size, kind: Kind::Smaller(1) }),
    }
}

/// What's in `dir`: the folders to add up, and the files.
fn list(dir: &Path, counted: &Counted) -> (Vec<Usage>, Vec<Node>) {
    if counted.stopped.load(Relaxed) {
        return Default::default();
    }
    let (folders, files): (Vec<_>, Vec<_>) =
        fs::list_usage(dir).unwrap_or_default().into_iter().partition(|item| item.folder);
    let files: Vec<_> = files
        .into_iter()
        .map(|file| match file.shared {
            Some((shared, bytes)) if counted.shared.lock().unwrap().insert(shared) => (file.name, file.size + bytes),
            _ => (file.name, file.size),
        })
        .collect();
    counted.bytes.fetch_add(files.iter().map(|(_, size)| size).sum(), Relaxed);
    (folders, files.into_iter().map(|(name, size)| Node { name, size, kind: Kind::File }).collect())
}

pub(super) fn sort(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
}

/// Takes out what `trail` leads to, and its size from each folder it was in. Resolves to that size.
pub(super) fn remove(nodes: &mut Vec<Node>, trail: &[Box<OsStr>]) -> Option<u64> {
    let (name, rest) = trail.split_first()?;
    let ix = nodes.iter().position(|node| node.name == *name)?;
    let removed = match (&mut nodes[ix].kind, rest) {
        (_, []) => nodes.remove(ix).size,
        (Kind::Folder(children), rest) => {
            let removed = remove(children, rest)?;
            nodes[ix].size -= removed;
            removed
        }
        _ => return None,
    };
    sort(nodes);
    Some(removed)
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::{CString, OsStr},
        os::unix::ffi::OsStringExt as _,
        path::{Path, PathBuf},
        sync::atomic::Ordering::Relaxed,
    };

    use super::{Counted, Kind, Node, add, remove, scan_root, sort};
    use crate::fs::Scratch;

    fn file(name: &str, size: u64) -> Node {
        Node { name: OsStr::new(name).into(), size, kind: Kind::File }
    }

    fn folder(name: &str, children: Vec<Node>) -> Node {
        Node {
            name: OsStr::new(name).into(),
            size: children.iter().map(|n| n.size).sum(),
            kind: Kind::Folder(children),
        }
    }

    fn names(nodes: &[Node]) -> Vec<&OsStr> {
        nodes.iter().map(|node| &*node.name).collect()
    }

    #[test]
    fn removing_takes_the_size_off_each_folder_above() {
        let mut nodes = vec![folder("a", vec![file("big", 30), folder("b", vec![file("c", 25)])]), file("d", 40)];
        let trail: Vec<Box<OsStr>> = ["a", "b", "c"].map(|name| OsStr::new(name).into()).to_vec();
        assert_eq!(remove(&mut nodes, &trail), Some(25));
        assert_eq!(names(&nodes), ["d", "a"]);
        assert_eq!(nodes[1].size, 30);
        let Kind::Folder(a) = &nodes[1].kind else { panic!() };
        assert_eq!(a[1].size, 0);
        assert_eq!(remove(&mut nodes, &trail), None);
    }

    #[test]
    fn scanning_adds_up_folders_and_groups_small_items() {
        let scratch = Scratch::new("storage-folders");
        let dir = &scratch.0;
        std::fs::create_dir_all(dir.join("inner/deeper")).unwrap();
        std::fs::create_dir_all(dir.join("little")).unwrap();
        std::fs::write(dir.join("inner/deeper/big"), vec![1u8; 2_000_000]).unwrap();
        std::fs::write(dir.join("inner/small"), b"hi").unwrap();
        std::fs::write(dir.join("little/small"), b"hi").unwrap();
        std::fs::write(dir.join("tiny"), b"hi").unwrap();
        let (nodes, counted) = scan(dir);

        assert_eq!(names(&nodes), ["inner", ""]);
        assert!(matches!(nodes[1].kind, Kind::Smaller(2)));
        let Kind::Folder(inner) = &nodes[0].kind else { panic!() };
        assert_eq!(names(inner), ["deeper", ""]);
        assert!(nodes[0].size >= 2_000_000);
        assert_eq!(counted, nodes.iter().map(|n| n.size).sum::<u64>());
    }

    #[test]
    fn clones_and_hard_links_count_once() {
        let scratch = Scratch::new("storage-shared");
        let dir = &scratch.0;
        std::fs::create_dir_all(dir.join("copies")).unwrap();
        std::fs::write(dir.join("cloned"), vec![1u8; 2_000_000]).unwrap();
        std::fs::write(dir.join("linked"), vec![2u8; 2_000_000]).unwrap();
        let c = |path: PathBuf| CString::new(path.into_os_string().into_vec()).unwrap();
        let (from, to) = (c(dir.join("cloned")), c(dir.join("copies/clone")));
        assert_eq!(unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) }, 0);
        std::fs::hard_link(dir.join("linked"), dir.join("copies/link")).unwrap();
        let (_, counted) = scan(dir);

        assert!((4_000_000..4_200_000).contains(&counted), "{counted}");
    }

    /// What's in `dir`, biggest first, and the bytes counted.
    fn scan(dir: &Path) -> (Vec<Node>, u64) {
        let counted = Counted::default();
        let (found, nodes) = smol::channel::unbounded();
        scan_root(dir, &counted, &found);
        drop(found);
        let mut nodes = std::iter::from_fn(|| nodes.try_recv().ok()).fold(Vec::new(), |mut nodes, node| {
            add(&mut nodes, node);
            nodes
        });
        sort(&mut nodes);
        (nodes, counted.bytes.load(Relaxed))
    }
}
