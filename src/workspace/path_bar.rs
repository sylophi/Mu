//! The path bar above a folder: its segments, and typing a path or a server to go to.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    notification::Notification,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::{
    Workspace,
    actions::{CancelPathEdit, CompletePath, NextSuggestion, PreviousSuggestion},
    go,
    parts::{opens_folder, row, truncated},
};
use crate::{
    drag::{self, DropFiles as _},
    fs::{self, home_dir, shown_path},
    icons, network,
    session::Tab,
    settings::Settings,
    theme::Toasts as _,
};

/// The path bar while it's typed in.
pub(super) struct PathEditor {
    input: Entity<InputState>,
    /// Folders to go to, for what's typed, once something is.
    suggestions: Vec<PathBuf>,
    /// The suggestion picked with the arrow keys, which Return goes to.
    picked: Option<usize>,
    _subscription: Subscription,
    /// Finding the suggestions for what's typed, which the next keystroke replaces.
    _suggest: Task<()>,
}

impl PathEditor {
    fn picked(&self) -> Option<&PathBuf> {
        self.suggestions.get(self.picked?)
    }
}

/// The note that a server's being connected to, up until it is.
struct Connecting;

/// The folders the path bar shows for `path`, starting at the home folder, iCloud Drive or the
/// volume it's on.
fn path_segments(path: &Path, home: &Path, icloud: &Path) -> Vec<PathBuf> {
    let base = if path.starts_with(icloud) {
        icloud.to_path_buf()
    } else if path.starts_with(home) {
        home.to_path_buf()
    } else if let Some(volume) = path.strip_prefix("/Volumes").ok().and_then(|p| p.components().next()) {
        Path::new("/Volumes").join(volume)
    } else {
        PathBuf::from("/")
    };
    let rest = path.strip_prefix(&base).unwrap_or(path);
    let mut dir = base.clone();
    let mut segments = vec![base];
    for component in rest.components() {
        dir.push(component);
        segments.push(dir.clone());
    }
    segments
}

impl Workspace {
    /// Edits the path in the path bar, selected, or with `server`, starts a server address there.
    pub(super) fn edit_path(&mut self, server: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_folder().is_none() && self.folder_tab(window, cx).is_none() {
            let dir = self.current_dir(cx);
            self.open_tab(Tab::Folder(dir), window, cx);
        }
        let Some(browser) = self.active_folder() else { return };
        let text = match server {
            true => "smb://".to_string(),
            false => browser.read(cx).path().to_string_lossy().into_owned(),
        };
        let selected = if server { text.len()..text.len() } else { 0..text.len() };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.set_selected_range(selected, cx);
        });
        let subscription = cx.subscribe_in(&input, window, |this, input, event, window, cx| match event {
            InputEvent::Change => this.suggest(cx),
            InputEvent::PressEnter { .. } => {
                let picked = this.path_editor.as_ref().and_then(PathEditor::picked).cloned();
                match picked {
                    Some(dir) => {
                        this.path_editor = None;
                        this.show_folder(dir, window, cx);
                    }
                    None => {
                        let text = input.read(cx).value().trim().to_string();
                        this.go_to_typed_path(&text, window, cx);
                    }
                }
            }
            InputEvent::Blur => {
                this.path_editor = None;
                cx.notify();
            }
            _ => {}
        });
        self.path_editor = Some(PathEditor {
            input,
            suggestions: Vec::new(),
            picked: None,
            _subscription: subscription,
            _suggest: Task::ready(()),
        });
        cx.notify();
    }

    /// Suggests folders for what's typed in the path bar, once they're found.
    fn suggest(&mut self, cx: &mut Context<Self>) {
        let cwd = self.current_dir(cx);
        let hidden = Settings::get(cx).show_hidden;
        let Some(editor) = &mut self.path_editor else { return };
        let suggestions = go::suggestions(editor.input.read(cx).value().trim(), &cwd, hidden, cx);
        // What was found was for what was typed before.
        editor.suggestions.clear();
        editor.picked = None;
        editor._suggest = cx.spawn(async move |this, cx| {
            let suggestions = suggestions.await;
            this.update(cx, |this, cx| {
                if let Some(editor) = &mut this.path_editor {
                    editor.suggestions = suggestions;
                    editor.picked = None;
                    cx.notify();
                }
            })
            .ok();
        });
        cx.notify();
    }

    /// Completes the folder name being typed, or fills in the suggestion picked, ready for the next.
    fn complete_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.current_dir(cx);
        let hidden = Settings::get(cx).show_hidden;
        let Some(editor) = &self.path_editor else { return };
        let completed = match editor.picked() {
            Some(dir) => Some(format!("{}/", fs::typed_path(dir))),
            None => go::complete(editor.input.read(cx).value().trim(), &cwd, hidden),
        };
        if let Some(text) = completed {
            editor.input.clone().update(cx, |input, cx| input.set_value(text, window, cx));
            self.suggest(cx);
        }
    }

    /// Picks the suggestion `delta` away from the one picked, wrapping around, with none picked
    /// before the first.
    fn pick_suggestion(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.path_editor else { return };
        let len = editor.suggestions.len() as isize + 1;
        let at = editor.picked.map_or(0, |ix| ix as isize + 1);
        editor.picked = match (at + delta).rem_euclid(len) {
            0 => None,
            at => Some(at as usize - 1),
        };
        cx.notify();
    }

    fn go_to_typed_path(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if text.contains("://") {
            self.path_editor = None;
            self.focus_active(window, cx);
            self.connect(text.to_string(), window, cx);
            return cx.notify();
        }
        let path = go::resolve(text, &self.current_dir(cx));
        if path.is_dir() {
            self.path_editor = None;
            self.show_folder(path, window, cx);
        } else if path.exists() {
            self.path_editor = None;
            // Shown in its folder and opened from there, as a double-click would.
            self.reveal(vec![path.clone()], window, cx);
            if let Some(browser) = self.folder_tab(window, cx) {
                browser.update(cx, |browser, cx| browser.open_files(vec![path], false, window, cx));
            }
        } else {
            window.toast(Notification::error(format!("Can't find “{text}”")), cx);
        }
        cx.notify();
    }

    /// Mounts the share at `address` and shows it.
    pub(super) fn connect(&mut self, address: String, window: &mut Window, cx: &mut Context<Self>) {
        let note = Notification::new()
            .message(format!("Connecting to “{}”…", network::server_name(&address)))
            .id::<Connecting>();
        window.toast(note.autohide(false), cx);
        let task = cx.background_spawn(async move { network::connect(&address) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                window.remove_notification::<Connecting>(cx);
                match result {
                    Ok(mount) => this.show_folder(mount, window, cx),
                    Err(error) => window.toast_error(&error, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn cancel_path_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.path_editor = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn render_path_bar(&self, path: &Path, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(editor) = &self.path_editor {
            return div()
                .relative()
                .key_context("PathEditor")
                .on_action(cx.listener(|this, _: &CancelPathEdit, window, cx| this.cancel_path_edit(window, cx)))
                .on_action(cx.listener(|this, _: &CompletePath, window, cx| this.complete_path(window, cx)))
                .on_action(cx.listener(|this, _: &NextSuggestion, _, cx| this.pick_suggestion(1, cx)))
                .on_action(cx.listener(|this, _: &PreviousSuggestion, _, cx| this.pick_suggestion(-1, cx)))
                .child(Input::new(&editor.input).small())
                .children(self.render_suggestions(editor, cx))
                .into_any_element();
        }
        let theme = cx.theme();
        let segments: Vec<(SharedString, PathBuf)> = path_segments(path, &home_dir(), &fs::icloud_drive())
            .into_iter()
            .map(|dir| (fs::display_name(&dir).into(), dir))
            .collect();
        // Short of room, the outermost folders give way to "…", and the one you're in stays.
        let style = window.text_style();
        let font_size = rems(0.875).to_pixels(window.rem_size());
        let widths: Vec<Pixels> = segments
            .iter()
            .enumerate()
            .map(|(ix, (name, _))| {
                let text = window.text_system().shape_line(name.clone(), font_size, &[style.to_run(name.len())], None);
                // Its padding and icon, and the chevron and gap before it.
                text.width + px(16.) + if ix == 0 { px(18.) } else { Pixels::ZERO }
            })
            .collect();
        let fits = |skip: usize| {
            let ellipsis = if skip > 0 { px(24.) } else { Pixels::ZERO };
            widths[skip..].iter().sum::<Pixels>() + ellipsis <= self.path_bar_width - px(10.)
        };
        let last = segments.len() - 1;
        let skip = match self.path_bar_width {
            // Before the first frame there's no width to go by yet.
            Pixels::ZERO => 0,
            _ => (0..last).find(|&skip| fits(skip)).unwrap_or(last),
        };
        h_flex()
            .id("path-bar")
            .h(px(26.))
            .px_1()
            .gap_0p5()
            .overflow_hidden()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.input)
            .bg(theme.background)
            .text_sm()
            .cursor_text()
            .when(skip > 0, |el| el.child(div().px_1p5().text_color(theme.muted_foreground).child("…")))
            .children(segments.into_iter().enumerate().skip(skip).map(|(ix, (name, path))| {
                h_flex()
                    .min_w_0()
                    .when(ix > 0, |el| {
                        el.child(
                            Icon::new(IconName::ChevronRight).xsmall().flex_none().text_color(theme.muted_foreground),
                        )
                    })
                    .child(
                        h_flex()
                            .id(("segment", ix))
                            .min_w_0()
                            .px_1p5()
                            .py_0p5()
                            .gap_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .when(ix != last, |el| el.text_color(theme.muted_foreground))
                            .hover(|s| s.bg(theme.secondary_hover).text_color(theme.foreground))
                            .drop_target()
                            .when(ix == 0, |el| el.child(icons::item(&path, true, px(14.))))
                            .child(truncated(name))
                            .map(|el| opens_folder(el, path.clone(), cx))
                            .on_drop_files(move |paths, window, cx| {
                                drag::drop_into(paths, path.clone(), window, cx).detach()
                            }),
                    )
            }))
            .child(div().flex_1().h_full())
            .on_click(cx.listener(|this, _, window, cx| this.edit_path(false, window, cx)))
            .into_any_element()
    }

    /// The folders suggested for what's typed in the path bar, under it.
    fn render_suggestions(&self, editor: &PathEditor, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        if editor.suggestions.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let rows = editor.suggestions.iter().enumerate().map(|(ix, dir)| {
            let parent = dir.parent().map(shown_path).unwrap_or_default();
            row(("suggestion", ix), theme)
                .cursor_pointer()
                .when(editor.picked == Some(ix), |el| el.bg(theme.list_active))
                .child(icons::item(dir, true, px(14.)))
                .child(div().flex_none().child(fs::display_name(dir)))
                .child(truncated(parent).text_color(theme.muted_foreground))
                .on_click(cx.listener({
                    let dir = dir.clone();
                    move |this, _, window, cx| {
                        this.path_editor = None;
                        this.show_folder(dir.clone(), window, cx);
                    }
                }))
        });
        Some(deferred(
            v_flex()
                .absolute()
                .top_full()
                .mt_1()
                .left_0()
                .right_0()
                .p_1()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .shadow_lg()
                .occlude()
                .children(rows),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::path_segments;

    fn segments(path: &str) -> Vec<PathBuf> {
        let home = Path::new("/Users/me");
        path_segments(Path::new(path), home, &home.join("Library/Mobile Documents/com~apple~CloudDocs"))
    }

    fn paths(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn segments_start_at_home_icloud_or_a_volume() {
        assert_eq!(segments("/Users/me"), paths(&["/Users/me"]));
        assert_eq!(
            segments("/Users/me/Documents/Taxes"),
            paths(&["/Users/me", "/Users/me/Documents", "/Users/me/Documents/Taxes"])
        );
        let icloud = "/Users/me/Library/Mobile Documents/com~apple~CloudDocs";
        assert_eq!(segments(&format!("{icloud}/Notes")), paths(&[icloud, &format!("{icloud}/Notes")]));
        assert_eq!(segments("/Volumes/Backup/Photos"), paths(&["/Volumes/Backup", "/Volumes/Backup/Photos"]));
    }

    #[test]
    fn segments_elsewhere_start_at_the_startup_disk() {
        assert_eq!(segments("/"), paths(&["/"]));
        assert_eq!(segments("/usr/local"), paths(&["/", "/usr", "/usr/local"]));
        assert_eq!(segments("/Volumes"), paths(&["/", "/Volumes"]));
        assert_eq!(segments("/Users/someone"), paths(&["/", "/Users", "/Users/someone"]));
    }
}
