//! File servers on the network, like Macs sharing their files: finding them over Bonjour and
//! connecting to them, the way Finder does.

use std::{
    collections::HashSet,
    ffi::{CStr, c_char, c_void},
    io,
    path::PathBuf,
};

use anyhow::{Context as _, Result, bail};
use gpui_kit::*;
use objc2_core_foundation::{CFArray, CFRetained, CFString, CFURL};

use crate::{
    fs::{self, Cancelled},
    link,
};

/// The names of the file servers found so far, in order.
#[derive(Default)]
pub struct Servers(pub Vec<SharedString>);

impl Global for Servers {}

/// A server coming or going on one of the Mac's network connections.
struct Change {
    name: String,
    interface: u32,
    added: bool,
}

pub fn init(cx: &mut App) {
    cx.set_global(Servers::default());
    let (tx, rx) = smol::channel::unbounded();
    std::thread::Builder::new().name("bonjour".into()).spawn(move || browse(&tx)).expect("the Bonjour thread starts");
    // This Mac shares its files under its own name, but there's no point connecting to it.
    let this_mac = crate::activity::monitor::computer_name();
    cx.spawn(async move |cx| {
        // A server shows up once for each connection it's on.
        let mut found = HashSet::new();
        while let Ok(change) = rx.recv().await {
            let server = (change.name, change.interface);
            if change.added {
                found.insert(server);
            } else {
                found.remove(&server);
            }
            // News comes in bursts, and the list changes once for each.
            if !rx.is_empty() {
                continue;
            }
            let mut names: Vec<&String> =
                found.iter().map(|(name, _)| name).filter(|name| Some(*name) != this_mac.as_ref()).collect();
            names.sort_by(|a, b| fs::natural_cmp(a, b));
            names.dedup();
            let names = names.into_iter().map(|name| SharedString::from(name.clone())).collect();
            cx.update(|cx| cx.set_global(Servers(names)));
        }
    })
    .detach();
}

/// The address that connects to the server found as `name`, by its Bonjour name, the way Finder
/// does.
pub fn address(name: &str) -> String {
    format!("smb://{}{BONJOUR}", link::escape(name.as_bytes(), b""))
}

/// What follows a server's name in its Bonjour address.
const BONJOUR: &str = "._smb._tcp.local";

/// Mounts the share at `address`, like smb://server/share, the way Finder's Connect to Server does:
/// macOS asks for a login if the server wants one and, given just a server, which of its shares to
/// mount. Resolves to where it's mounted.
pub fn connect(address: &str) -> Result<PathBuf> {
    #[link(name = "NetFS", kind = "framework")]
    unsafe extern "C" {
        fn NetFSMountURLSync(
            url: &CFURL,
            mountpath: *const c_void,
            user: *const c_void,
            passwd: *const c_void,
            open_options: *const c_void,
            mount_options: *const c_void,
            mountpoints: *mut *mut CFArray,
        ) -> i32;
    }
    let url = CFURL::from_string(None, &CFString::from_str(address), None)
        .with_context(|| format!("“{address}” isn't a server address"))?;
    let host = server_name(address);
    let null = std::ptr::null();
    let mut mountpoints = std::ptr::null_mut();
    let status = unsafe { NetFSMountURLSync(&url, null, null, null, null, null, &mut mountpoints) };
    // Mount points, as strings, the caller's to release.
    let mountpoints = std::ptr::NonNull::new(mountpoints)
        .map(|array| unsafe { CFRetained::cast_unchecked::<CFArray<CFString>>(CFRetained::from_raw(array)) });
    match status {
        0 => mountpoints
            .and_then(|points| points.get(0))
            .map(|point| PathBuf::from(point.to_string()))
            .with_context(|| format!("Couldn't connect to “{host}”")),
        libc::EEXIST => mounted(address).with_context(|| format!("“{host}” is connected already")),
        // Cancelled.
        -128 => Err(Cancelled.into()),
        code if code > 0 => bail!("Couldn't connect to “{host}”: {}", io::Error::from_raw_os_error(code)),
        code => bail!("Couldn't connect to “{host}” (error {code})"),
    }
}

/// The server in `address`, for messages. One found over Bonjour goes by the name it's found as.
pub fn server_name(address: &str) -> String {
    CFURL::from_string(None, &CFString::from_str(address), None)
        .and_then(|url| url.host_name())
        .map_or_else(|| address.to_string(), |host| host.to_string().trim_end_matches(BONJOUR).into())
}

/// The network volume already mounted from `address`, going by the share's name, the last part of
/// both.
fn mounted(address: &str) -> Option<PathBuf> {
    let share = |source: &str| source.trim_end_matches('/').rsplit(['/', ':']).next().map(str::to_owned);
    let wanted = share(address)?;
    fs::volumes()
        .into_iter()
        .filter(|volume| volume.network)
        .map(|volume| volume.path)
        .find(|path| fs::mounted_from(path).is_some_and(|source| share(&source).as_ref() == Some(&wanted)))
}

type ServiceRef = *mut c_void;
type BrowseReply = extern "C" fn(ServiceRef, u32, u32, i32, *const c_char, *const c_char, *const c_char, *mut c_void);

unsafe extern "C" {
    fn DNSServiceBrowse(
        service: *mut ServiceRef,
        flags: u32,
        interface: u32,
        kind: *const c_char,
        domain: *const c_char,
        reply: BrowseReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceProcessResult(service: ServiceRef) -> i32;
}

/// kDNSServiceFlagsAdd: the server came, rather than went.
const ADDED: u32 = 0x2;

/// Looks for servers sharing files over SMB for as long as Mu runs, sending what comes and goes.
fn browse(tx: &smol::channel::Sender<Change>) {
    extern "C" fn reply(
        _: ServiceRef,
        flags: u32,
        interface: u32,
        error: i32,
        name: *const c_char,
        _: *const c_char,
        _: *const c_char,
        context: *mut c_void,
    ) {
        if error != 0 || name.is_null() {
            return;
        }
        let tx = unsafe { &*context.cast::<smol::channel::Sender<Change>>() };
        let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        tx.send_blocking(Change { name, interface, added: flags & ADDED != 0 }).ok();
    }
    let mut service = std::ptr::null_mut();
    let context = std::ptr::from_ref(tx).cast_mut().cast();
    if unsafe { DNSServiceBrowse(&mut service, 0, 0, c"_smb._tcp".as_ptr(), std::ptr::null(), reply, context) } != 0 {
        return;
    }
    // Each call waits for the next news and hands it to `reply`.
    while unsafe { DNSServiceProcessResult(service) } == 0 {}
}

#[cfg(test)]
mod tests {
    use super::{address, server_name};

    #[test]
    fn addresses_servers_by_their_bonjour_names() {
        assert_eq!(address("Kiwi IV"), "smb://Kiwi%20IV._smb._tcp.local");
        assert_eq!(address("Romi’s Mac"), "smb://Romi%E2%80%99s%20Mac._smb._tcp.local");
        assert_eq!(server_name(&address("Kiwi IV")), "Kiwi IV");
    }
}
