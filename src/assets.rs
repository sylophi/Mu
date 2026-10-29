//! What Mu bundles for GPUI to draw: the line icons it uses beyond GPUI Kit's own, and its logo.

use std::borrow::Cow;

use gpui_kit::*;

gpui_kit::assets::icon_assets!(
    ExtraIcons,
    [
        Activity,
        AppWindow,
        AppWindowMac,
        ArchiveRestore,
        ClipboardPaste,
        CloudDownload,
        Columns2,
        CopyPlus,
        Download,
        Eject,
        FileArchive,
        Files,
        FolderInput,
        FolderPlus,
        FolderSearch,
        Hammer,
        HardDrive,
        Inbox,
        LayoutGrid,
        Link,
        List,
        Network,
        Package,
        PackageOpen,
        Pencil,
        Redo2,
        Replace,
        RotateCcwClock,
        Scissors,
        Server,
        Settings,
        ShieldAlert,
        SquareArrowOutUpRight,
        SquarePen,
        Trash,
        X,
    ]
);

/// Mu's logo and icons, with GPUI Kit's.
pub struct Assets;

/// Mu's logo, the layers of its app icon.
pub const LOGO: [(&str, &[u8]); 4] = [
    ("logo/cyan.svg", include_bytes!("../assets/icon/AppIcon.icon/Assets/cyan.svg")),
    ("logo/green.svg", include_bytes!("../assets/icon/AppIcon.icon/Assets/green.svg")),
    ("logo/yellow.svg", include_bytes!("../assets/icon/AppIcon.icon/Assets/yellow.svg")),
    ("logo/pink.svg", include_bytes!("../assets/icon/AppIcon.icon/Assets/pink.svg")),
];

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = LOGO.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
