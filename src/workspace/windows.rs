//! Mu's windows: opening one, opening what other apps ask for, opening them again at launch, saving
//! them as they close, and quitting.

use std::{ffi::c_void, path::PathBuf};

use gpui_kit::component::{Root, TitleBar};
use gpui_kit::*;
use objc2::{Message as _, rc::Retained};
use objc2_app_kit::{NSView, NSWindow};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::{Item, Workspace};
use crate::{
    editor::FileEditor,
    fs::home_dir,
    quicklook,
    session::{self, Tab},
};

pub(super) fn init(cx: &mut App) {
    let app = cx.to_async();
    quicklook::init(move |view, at| app.update(|cx| look_up(view, at, cx)));
    // Closing the last window saves it as it goes, so quitting then has nothing to save.
    cx.on_app_quit(|cx| {
        let windows = open_windows(cx);
        if !windows.is_empty() {
            session::save(&windows);
        }
        async {}
    })
    .detach();
}

/// Opens a window on `path`. It steps down and right from the window in front, the way macOS
/// cascades them, so it doesn't land right on top of it, and past the screen's edge it starts again
/// from the top left. It waits for any window being updated, like the one whose action asked for
/// it, since only then can that window say where it is.
pub fn open_window(path: PathBuf, cx: &mut App) {
    cx.defer(move |cx| {
        let front = cx
            .active_window()
            .and_then(|handle| handle.update(cx, |_, window, cx| (window.bounds(), window.display(cx))).ok());
        let (display_id, bounds) = match front {
            Some((front, Some(display))) => {
                let mut bounds = Bounds::new(front.origin + point(px(24.), px(24.)), front.size);
                // Where windows go on this screen, measured from its corner like the window is.
                let (screen, visible) = (display.bounds(), display.visible_bounds());
                let visible = Bounds::new(visible.origin - screen.origin, visible.size);
                if bounds.right() > visible.right() || bounds.bottom() > visible.bottom() {
                    bounds.origin = visible.origin;
                }
                (Some(display.id()), bounds)
            }
            _ => (None, Bounds::centered(None, WINDOW_SIZE, cx)),
        };
        open(WindowBounds::Windowed(bounds), display_id, cx, move |this, window, cx| {
            this.open_tabs(vec![Tab::Folder(path)], 0, window, cx)
        });
    });
}

/// The tabs asked for, each with the files to select in it. A folder tab for a file is one for its
/// folder instead, which one tab shows for all the files asked for there.
fn group_files(tabs: Vec<Tab>) -> Vec<(Tab, Vec<PathBuf>)> {
    let mut asked: Vec<(Tab, Vec<PathBuf>)> = Vec::new();
    for tab in tabs {
        match tab {
            Tab::Folder(path) if path.is_file() => {
                let dir = path.parent().expect("a file is in a folder").to_path_buf();
                match asked.iter_mut().find(|(tab, _)| matches!(tab, Tab::Folder(folder) if *folder == dir)) {
                    Some((_, files)) => files.push(path),
                    None => asked.push((Tab::Folder(dir), vec![path])),
                }
            }
            tab => asked.push((tab, Vec::new())),
        }
    }
    asked
}

/// Opens tabs another app asked for, after the one in front in the window in front, the way a
/// browser opens a link from another app, or in a window of its own when there's none. A folder
/// that's open already shows in its tab instead. A folder tab for a file shows it selected in its
/// folder, along with any others asked for there.
pub fn open_asked(tabs: Vec<Tab>, cx: &mut App) {
    let mut asked = group_files(tabs);
    let windows = workspaces(cx);
    // A folder that's open already shows in its tab, in the window furthest in front that has it.
    let mut shown_in = None;
    asked.retain_mut(|(tab, files)| {
        let Tab::Folder(dir) = tab else { return true };
        let open = windows.iter().find_map(|(handle, workspace)| {
            let ix =
                workspace.read(cx).items.iter().position(
                    |item| matches!(item, Item::Folder(browser) if browser.read(cx).path() == dir.as_path()),
                )?;
            Some((*handle, workspace.clone(), ix))
        });
        let Some((handle, workspace, ix)) = open else { return true };
        let files = std::mem::take(files);
        handle
            .update(cx, |_, window, cx| {
                workspace.update(cx, |this, cx| {
                    this.activate(ix, window, cx);
                    if !files.is_empty() {
                        this.reveal(files, window, cx);
                    }
                })
            })
            .ok();
        shown_in = Some(handle);
        false
    });
    let all_shown = asked.is_empty();
    let show = move |this: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>| {
        for (tab, files) in asked {
            if this.open_tab_again(tab, window, cx) && !files.is_empty() {
                this.reveal(files, window, cx);
            }
        }
    };
    let handle = match (shown_in, windows.into_iter().next()) {
        (Some(handle), _) if all_shown => handle,
        (_, Some((handle, workspace))) => {
            handle.update(cx, |_, window, cx| workspace.update(cx, |this, cx| show(this, window, cx))).ok();
            handle
        }
        (_, None) => {
            let bounds = WindowBounds::Windowed(Bounds::centered(None, WINDOW_SIZE, cx));
            open(bounds, None, cx, move |this, window, cx| {
                show(this, window, cx);
                if this.items.is_empty() {
                    this.open_tab(Tab::Folder(home_dir()), window, cx);
                }
            })
            .0
        }
    };
    handle.update(cx, |_, window, _| window.activate_window()).ok();
    cx.activate(true);
}

/// Opens the windows open when Mu last closed, where they were, or a window on the home folder.
pub fn restore(cx: &mut App) {
    let windows = session::load();
    // At once, so a tab asked for at launch has a window to go in.
    if windows.is_empty() {
        return open_asked(vec![Tab::Folder(home_dir())], cx);
    }
    // The front one last, so it ends up in front.
    for saved in windows.into_iter().rev() {
        let display = cx.displays().into_iter().find(|display| {
            saved.display.is_some() && display.uuid().ok().map(|uuid| uuid.to_string()) == saved.display
        });
        // Off its display, it keeps its size in the middle of the main one.
        let bounds = match &display {
            Some(_) => saved.bounds,
            None => Bounds::centered(None, saved.bounds.size, cx),
        };
        let bounds = saved.window_bounds(bounds);
        open(bounds, display.map(|display| display.id()), cx, move |this, window, cx| {
            this.open_tabs(saved.tabs, saved.active, window, cx)
        });
    }
}

/// How big a window opens when there's none to go by.
const WINDOW_SIZE: Size<Pixels> = size(px(950.), px(650.));

/// Opens a window, with the tabs `fill` puts in it.
pub(super) fn open(
    bounds: WindowBounds,
    display_id: Option<DisplayId>,
    cx: &mut App,
    fill: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> (AnyWindowHandle, Entity<Workspace>) {
    let options = WindowOptions {
        window_bounds: Some(bounds),
        display_id,
        window_min_size: Some(size(px(640.), px(360.))),
        app_id: Some("mu".into()),
        ..TitleBar::window_options()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| {
            let mut workspace = Workspace::new(window, cx);
            fill(&mut workspace, window, cx);
            workspace
        })
    })
    .expect("failed to open a window")
}

/// Shows what's at `at` in Quick Look, in the window whose view is `view`, as a tap with three
/// fingers does in Finder.
fn look_up(view: *const c_void, at: Point<Pixels>, cx: &mut App) {
    for (handle, workspace) in workspaces(cx) {
        handle
            .update(cx, |_, window, cx| {
                if ns_view(window).is_some_and(|ns_view| Retained::as_ptr(&ns_view).cast() == view) {
                    workspace.update(cx, |this, cx| this.with_folder(cx, |browser, cx| browser.look_up(at, cx)));
                }
            })
            .ok();
    }
}

/// Every window as it is now, the front one first.
fn open_windows(cx: &mut App) -> Vec<session::Window> {
    workspaces(cx)
        .into_iter()
        .filter_map(|(handle, workspace)| {
            handle.update(cx, |_, window, cx| workspace.read(cx).session(window, cx)).ok()
        })
        .collect()
}

/// Every window's workspace, the front one first, but for a window being updated, which can't be
/// read until it's done.
pub(super) fn workspaces(cx: &App) -> Vec<(AnyWindowHandle, Entity<Workspace>)> {
    let handles = cx.window_stack().unwrap_or_else(|| cx.windows());
    handles
        .into_iter()
        .filter_map(|handle| {
            let root = handle.downcast::<Root>()?.read(cx).ok()?;
            Some((handle, root.view().clone().downcast::<Workspace>().ok()?))
        })
        .collect()
}

/// The AppKit view a window draws in.
fn ns_view(window: &Window) -> Option<Retained<NSView>> {
    let Ok(RawWindowHandle::AppKit(handle)) = HasWindowHandle::window_handle(window).map(|handle| handle.as_raw())
    else {
        return None;
    };
    // A GPUI window's handle is its view's, which lives as long as it does.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    Some(view.retain())
}

/// The AppKit window of a window.
pub(super) fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    ns_view(window)?.window()
}

/// Asks whether to save unsaved editors before they close, resolving to whether closing can go on.
pub(super) fn confirm_discard(editors: Vec<Entity<FileEditor>>, window: &mut Window, cx: &mut App) -> Task<bool> {
    if editors.is_empty() {
        return Task::ready(true);
    }
    let message = match editors.as_slice() {
        [editor] => format!("Save changes to “{}”?", editor.read(cx).title()),
        _ => format!("Save changes to {} files?", editors.len()),
    };
    let answer = window.prompt(
        PromptLevel::Warning,
        &message,
        Some("Your changes will be lost if you don't save them."),
        &["Save", "Don't Save", "Cancel"],
        cx,
    );
    window.spawn(cx, async move |cx| match answer.await {
        Ok(0) => {
            let saves = cx.update(|window, cx| {
                editors.iter().map(|editor| editor.update(cx, |editor, cx| editor.save(window, cx))).collect::<Vec<_>>()
            });
            let mut saved = saves.is_ok();
            for save in saves.into_iter().flatten() {
                saved &= save.await;
            }
            saved
        }
        Ok(1) => true,
        _ => false,
    })
}

/// Quits once every window's unsaved changes are saved or let go, doing `then` first.
pub fn quit(cx: &mut App, then: impl FnOnce() + 'static) {
    cx.spawn(async move |cx| {
        // Read once the window handling the keystroke is done, as it can't be read until then.
        for (handle, workspace) in cx.update(|cx| workspaces(cx)) {
            let confirm = handle.update(cx, |_, window, cx| {
                let unsaved = workspace.read(cx).unsaved(cx);
                if !unsaved.is_empty() {
                    window.activate_window();
                }
                confirm_discard(unsaved, window, cx)
            });
            if let Ok(confirm) = confirm
                && !confirm.await
            {
                return;
            }
        }
        then();
        cx.update(|cx| cx.quit());
    })
    .detach();
}

impl Workspace {
    /// Opens tabs as they were saved, showing `active`, or the home folder when none of them can.
    fn open_tabs(&mut self, tabs: Vec<Tab>, active: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Tabs whose folders are gone don't open, so the active one moves up past them.
        let mut shown_before_active = 0;
        for (ix, tab) in tabs.into_iter().enumerate() {
            if self.open_tab_again(tab, window, cx) && ix < active {
                shown_before_active += 1;
            }
        }
        if self.items.is_empty() {
            self.open_tab(Tab::Folder(home_dir()), window, cx);
        }
        self.activate(shown_before_active.min(self.items.len() - 1), window, cx);
    }

    /// The window as it is now, to open again.
    pub(super) fn session(&self, window: &Window, cx: &App) -> session::Window {
        let tabs = self.items.iter().map(|item| item.saved(cx)).collect();
        let display = window.display(cx).and_then(|display| display.uuid().ok()).map(|uuid| uuid.to_string());
        session::Window::new(tabs, self.items.active(), window.window_bounds(), display)
    }

    /// Saves the window as it closes, when it's the last one, so Mu opens it again next time.
    pub(super) fn save_if_last(&self, window: &Window, cx: &App) {
        if cx.windows().len() == 1 {
            session::save(&[self.session(window, cx)]);
        }
    }

    /// Closes the window, asking first about unsaved changes.
    pub(super) fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let confirm = confirm_discard(self.unsaved(cx), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if confirm.await {
                this.update_in(cx, |this, window, cx| {
                    this.save_if_last(window, cx);
                    window.remove_window();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn unsaved(&self, cx: &App) -> Vec<Entity<FileEditor>> {
        self.items
            .iter()
            .filter_map(|item| match item {
                Item::Editor(editor) if editor.read(cx).is_dirty() => Some(editor.clone()),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::group_files;
    use crate::{fs::Scratch, session::Tab};

    #[test]
    fn files_asked_for_open_selected_in_one_tab_for_their_folder() {
        let scratch = Scratch::new("group-files");
        let (dir, other) = (scratch.0.join("dir"), scratch.0.join("other"));
        let sub = dir.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(&other).unwrap();
        let [a, b, c] = [dir.join("a.txt"), dir.join("b.txt"), other.join("c.txt")];
        for file in [&a, &b, &c] {
            fs::write(file, "").unwrap();
        }
        let tabs = vec![
            Tab::Folder(a.clone()),
            Tab::Folder(sub.clone()),
            Tab::Folder(b.clone()),
            // Only a folder tab opens on a file's folder.
            Tab::Editor(c.clone()),
            Tab::Folder(c.clone()),
        ];
        let grouped: Vec<(Option<PathBuf>, Vec<PathBuf>)> =
            group_files(tabs).into_iter().map(|(tab, files)| (tab.path().cloned(), files)).collect();
        assert_eq!(
            grouped,
            [(Some(dir), vec![a, b]), (Some(sub), vec![]), (Some(c.clone()), vec![]), (Some(other), vec![c])]
        );
    }
}
