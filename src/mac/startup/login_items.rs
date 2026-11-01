//! The login items apps register for themselves, as System Settings lists them in Login Items &
//! Extensions: apps that open at login, and helpers inside them that run in the background. Only the
//! app or System Settings can switch them, so they're read from the database macOS keeps of them.
//! Anyone can read it, though its format is Apple's own, a keyed archive of its records.

use std::path::PathBuf;

use objc2_foundation::{NSString, NSURL};
use plist::{Dictionary, Value};

/// Where they're switched.
pub const SETTINGS: &str = "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";

const DATABASES: &str = "/private/var/db/com.apple.backgroundtaskmanagement";

/// What each record is, by its type's bits.
const APP: u64 = 0x2;
const LOGIN_ITEM: u64 = 0x4;
const AGENT: u64 = 0x8;
const DAEMON: u64 = 0x10;
/// One of launchd's plists, in the folders launchd starts them from.
const LEGACY: u64 = 0x10000;
/// A disposition's bits for being switched on, by the app, and for being let run, which System
/// Settings switches.
const ENABLED: u64 = 0x1;
const ALLOWED: u64 = 0x2;

#[derive(Debug, PartialEq)]
pub struct LoginItem {
    pub name: String,
    /// The app it's in, or is.
    pub app: Option<PathBuf>,
    pub on: bool,
}

/// Yours, when the database can be read.
pub fn login_items() -> Vec<LoginItem> {
    database().and_then(|path| Value::from_file(path).ok()).map(|archive| parse(&archive)).unwrap_or_default()
}

/// Your database, by your UUID, in the latest version there is, which goes up with macOS.
fn database() -> Option<PathBuf> {
    let uuid = user_uuid()?;
    let suffix = format!("-{uuid}.btm");
    let names =
        std::fs::read_dir(DATABASES).ok()?.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned());
    let version =
        |name: &str| -> Option<u32> { name.strip_prefix("BackgroundItems-v")?.strip_suffix(&suffix)?.parse().ok() };
    names.filter_map(|name| Some((version(&name)?, name))).max().map(|(_, name)| PathBuf::from(DATABASES).join(name))
}

fn user_uuid() -> Option<String> {
    unsafe extern "C" {
        fn mbr_uid_to_uuid(uid: libc::uid_t, uuid: *mut [u8; 16]) -> libc::c_int;
    }
    let mut uuid = [0; 16];
    if unsafe { mbr_uid_to_uuid(libc::getuid(), &mut uuid) } != 0 {
        return None;
    }
    let hex: String = uuid.iter().map(|byte| format!("{byte:02X}")).collect();
    Some(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]))
}

/// The objects of a keyed archive, which refer to each other by their place among them.
struct Archive<'a>(&'a [Value]);

impl<'a> Archive<'a> {
    fn get(&self, value: Option<&'a Value>) -> Option<&'a Value> {
        match value? {
            // The first is `$null`.
            Value::Uid(uid) if uid.get() == 0 => None,
            Value::Uid(uid) => self.0.get(uid.get() as usize),
            value => Some(value),
        }
    }

    fn dictionary(&self, value: Option<&'a Value>) -> Option<&'a Dictionary> {
        self.get(value)?.as_dictionary()
    }

    fn string(&self, record: &'a Dictionary, key: &str) -> Option<&'a str> {
        self.get(record.get(key))?.as_string()
    }

    fn number(&self, record: &Dictionary, key: &str) -> u64 {
        record.get(key).and_then(Value::as_unsigned_integer).unwrap_or(0)
    }

    /// Where a record's URL leads, when it's a file's.
    fn path(&self, record: &Dictionary) -> Option<PathBuf> {
        let url = self.string(self.dictionary(record.get("url"))?, "NS.relative")?;
        NSURL::URLWithString(&NSString::from_str(url))?.to_file_path()
    }
}

fn parse(archive: &Value) -> Vec<LoginItem> {
    let Some(archive) = archive.as_dictionary() else { return Vec::new() };
    let Some(objects) = archive.get("$objects").and_then(Value::as_array) else { return Vec::new() };
    let objects = Archive(objects);
    // Under a key of its own, like `userStore`.
    let root = objects.dictionary(archive.get("$top").and_then(|top| top.as_dictionary()?.values().next()));
    let records = objects.dictionary(root.and_then(|root| root.get("records")));
    let records: Vec<&Dictionary> = records
        .and_then(|records| records.get("NS.objects")?.as_array())
        .into_iter()
        .flatten()
        .filter_map(|record| objects.dictionary(Some(record)))
        .collect();
    // An app's own record, which those in it name as their container.
    let app = |identifier: &str| {
        let record = records.iter().find(|record| objects.string(record, "identifier") == Some(identifier))?;
        objects.path(record)
    };
    let mut items: Vec<LoginItem> = records
        .iter()
        .filter_map(|record| {
            let kind = objects.number(record, "type");
            let disposition = objects.number(record, "disposition");
            let name = objects.string(record, "name")?.to_owned();
            // An app's own record is enabled when the app opens at login.
            let app = match kind {
                APP if disposition & ENABLED != 0 => objects.path(record),
                _ if kind & LOGIN_ITEM != 0 || (kind & (AGENT | DAEMON) != 0 && kind & LEGACY == 0) => {
                    objects.string(record, "container").and_then(app).or_else(|| objects.path(record))
                }
                _ => return None,
            };
            let on = disposition & (ENABLED | ALLOWED) == ENABLED | ALLOWED;
            Some(LoginItem { name, app, on })
        })
        .collect();
    items.sort_by(|a, b| crate::fs::natural_cmp(&a.name, &b.name));
    items
}

#[cfg(test)]
mod tests {
    use plist::{Dictionary, Uid, Value};

    use super::{LoginItem, parse};

    #[test]
    fn reads_the_records_apps_register() {
        let uid = |ix| Value::Uid(Uid::new(ix));
        let record = |kind: u64, disposition: u64, name, identifier, url, container| {
            let mut record = Dictionary::new();
            record.insert("type".into(), kind.into());
            record.insert("disposition".into(), disposition.into());
            record.insert("name".into(), uid(name));
            record.insert("identifier".into(), uid(identifier));
            record.insert("url".into(), uid(url));
            record.insert("container".into(), uid(container));
            Value::Dictionary(record)
        };
        let url = |relative| Value::Dictionary(Dictionary::from_iter([("NS.relative".to_owned(), uid(relative))]));
        let objects = vec![
            "$null".into(),
            Value::Dictionary(Dictionary::from_iter([("records".to_owned(), uid(2))])),
            Value::Dictionary(Dictionary::from_iter([(
                "NS.objects".to_owned(),
                Value::Array(vec![uid(3), uid(4), uid(5), uid(6)]),
            )])),
            // An app that opens at login, a helper in it that System Settings stopped, an app that
            // doesn't, and an agent from ~/Library/LaunchAgents, which launchd's own list has.
            record(0x2, 0b1011, 7, 8, 9, 0),
            record(0x4, 0b1001, 11, 12, 13, 8),
            record(0x2, 0b1010, 15, 16, 17, 0),
            record(0x10008, 0b1011, 15, 16, 17, 0),
            "Swish".into(),
            "2.co.swish".into(),
            url(10),
            "file:///Applications/Swish.app/".into(),
            "Swish Helper".into(),
            "4.co.swish.helper".into(),
            url(14),
            "Contents/Library/LoginItems/Swish%20Helper.app".into(),
            "Ghostty".into(),
            "2.com.ghostty".into(),
            url(18),
            "file:///Applications/Ghostty.app/".into(),
        ];
        let archive = Value::Dictionary(Dictionary::from_iter([
            ("$objects".to_owned(), Value::Array(objects)),
            ("$top".to_owned(), Value::Dictionary(Dictionary::from_iter([("userStore".to_owned(), uid(1))]))),
        ]));
        let swish = Some("/Applications/Swish.app".into());
        assert_eq!(
            parse(&archive),
            [
                LoginItem { name: "Swish".into(), app: swish.clone(), on: true },
                LoginItem { name: "Swish Helper".into(), app: swish, on: false },
            ]
        );
    }
}
