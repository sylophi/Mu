//! The menu a right click on the listing brings up.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    Icon, h_flex,
    menu::{PopupMenu, PopupMenuItem},
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::{
    AddToSidebar, Browser, Compress, Copy, CopyPath, Cut, Duplicate, NewFolder, Open, OpenInEditor, OpenInNewTab,
    OpenInNewWindow, OpenInTerminal, Paste, PutBack, QuickLook, Refresh, Rename, ShowPackageContents, Trash,
    listing::SortKey,
};
use crate::{
    fs::{self, Entry},
    icons, pasteboard,
    workspace::actions::{AddToShelf, EmptyTrash},
};

impl Browser {
    pub(super) fn context_menu(
        entity: &Entity<Self>,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let this = entity.read(cx);
        let menu = menu.action_context(this.focus_handle.clone());
        let selected: Vec<Entry> = this.selected_entries().into_iter().cloned().collect();
        let in_trash = this.in_trash();
        let sort = this.listing.sort;
        let can_paste = !pasteboard::files().is_empty() || pasteboard::has_contents();
        if selected.is_empty() {
            return menu
                .menu_with_icon("New Folder", IconName::FolderPlus, Box::new(NewFolder))
                .menu_with_icon_and_disabled("Paste", IconName::ClipboardPaste, Box::new(Paste), !can_paste)
                .separator()
                .menu_with_icon("Open in Terminal", IconName::SquareTerminal, Box::new(OpenInTerminal))
                .menu_with_icon("Copy Path", IconName::Link, Box::new(CopyPath))
                .menu_with_icon("Add to Sidebar", IconName::PanelLeft, Box::new(AddToSidebar))
                .separator()
                .menu_with_icon("Refresh", IconName::RefreshCw, Box::new(Refresh))
                .when(in_trash, |menu| {
                    menu.separator().menu_with_icon("Empty Trash…", IconName::Trash, Box::new(EmptyTrash))
                })
                .separator()
                .submenu("Sort By", window, cx, {
                    let entity = entity.clone();
                    move |menu, _, _| {
                        SortKey::ALL.into_iter().fold(menu, |menu, key| {
                            let entity = entity.clone();
                            menu.item(
                                PopupMenuItem::new(key.label()).checked(sort == key).on_click(move |_, _, cx| {
                                    entity.update(cx, |browser, cx| browser.set_sort(key, cx))
                                }),
                            )
                        })
                    }
                });
        }
        // Only what Mu moved to the Trash says where it goes back to.
        let known = in_trash && selected.iter().any(|e| fs::origin(&e.path).is_some());
        let single = selected.len() == 1;
        let single_dir = single && selected[0].folder;
        let package = single && selected[0].package;
        let menu = menu
            .when(in_trash, |menu| {
                menu.menu_with_icon_and_disabled("Put Back", IconName::Undo2, Box::new(PutBack), !known).separator()
            })
            .menu_with_icon("Open", IconName::SquareArrowOutUpRight, Box::new(Open));
        let paths: Vec<PathBuf> = selected.iter().map(|e| e.path.clone()).collect();
        let apps = if in_trash { Vec::new() } else { fs::apps_to_open(&paths) };
        let menu = match apps.split_first() {
            None => menu,
            Some((default, others)) => {
                menu.submenu_with_icon(Some(Icon::new(IconName::AppWindow)), "Open With", window, cx, {
                    let (default, others) = (default.clone(), others.to_vec());
                    move |menu, _, _| {
                        let item = |label: String, app: PathBuf| {
                            let paths = paths.clone();
                            let icon = app.clone();
                            PopupMenuItem::element(move |_, _| {
                                h_flex().gap_2().child(icons::item(&icon, true, px(16.))).child(label.clone())
                            })
                            .on_click(move |_, _, _| fs::open_with(&paths, &app))
                        };
                        let menu = menu.item(item(format!("{} (default)", default.0), default.1.clone()));
                        others
                            .iter()
                            .fold(menu.when(!others.is_empty(), |menu| menu.separator()), |menu, (name, app)| {
                                menu.item(item(name.clone(), app.clone()))
                            })
                    }
                })
            }
        };
        menu.when(selected.iter().any(|e| e.folder), |menu| {
            menu.menu_with_icon("Open in New Tab", IconName::Plus, Box::new(OpenInNewTab)).menu_with_icon(
                "Open in New Window",
                IconName::AppWindowMac,
                Box::new(OpenInNewWindow),
            )
        })
        .when(package, |menu| {
            menu.menu_with_icon("Show Package Contents", IconName::PackageOpen, Box::new(ShowPackageContents))
        })
        .when(single_dir, |menu| {
            menu.menu_with_icon("Open in Terminal", IconName::SquareTerminal, Box::new(OpenInTerminal))
        })
        .when(selected.iter().any(|e| !e.folder && !e.package), |menu| {
            menu.menu_with_icon("Open in Editor", IconName::SquarePen, Box::new(OpenInEditor))
        })
        .menu_with_icon("Quick Look", IconName::Eye, Box::new(QuickLook))
        .separator()
        .menu_with_icon("Copy", IconName::Copy, Box::new(Copy))
        .menu_with_icon("Cut", IconName::Scissors, Box::new(Cut))
        .menu_with_icon_and_disabled("Paste", IconName::ClipboardPaste, Box::new(Paste), !can_paste)
        .menu_with_icon("Duplicate", IconName::CopyPlus, Box::new(Duplicate))
        .menu_with_icon("Compress", IconName::FileArchive, Box::new(Compress))
        .menu_with_icon("Copy Path", IconName::Link, Box::new(CopyPath))
        .separator()
        .when(single, |menu| menu.menu_with_icon("Rename", IconName::Pencil, Box::new(Rename)))
        .menu_with_icon("Add to Shelf", IconName::Inbox, Box::new(AddToShelf))
        .when(selected.iter().all(|e| e.folder), |menu| {
            menu.menu_with_icon("Add to Sidebar", IconName::PanelLeft, Box::new(AddToSidebar))
        })
        .when(!in_trash, |menu| menu.menu_with_icon("Move to Trash", IconName::Trash, Box::new(Trash)))
    }
}
