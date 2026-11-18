//! The path bar above a folder: its segments, and typing a path or a server to go to.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Theme, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    tooltip::Tooltip,
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
    // An app's folder is at the top of iCloud Drive, wherever iCloud keeps it.
    let app = icloud.parent().and_then(|root| fs::icloud::app_folder_in(path, root));
    let base = if let Some(app) = &app {
        app.clone()
    } else if path.starts_with(icloud) {
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
    let mut segments = if app.is_some() { vec![icloud.to_path_buf(), base] } else { vec![base] };
    for component in rest.components() {
        dir.push(component);
        segments.push(dir.clone());
    }
    segments
}

/// What the path bar shows of a folder's segments, in the room it has.
#[derive(Debug, Default, PartialEq)]
struct Fit {
    /// Whether the first folder shows its icon and not its name.
    icon_only: bool,
    /// How many folders after the first make way for a "…" menu of them.
    hidden: usize,
    /// The widest a name can be, short of room, but the last's.
    cap: Option<Pixels>,
}

// How `render_path_bar` lays out the path bar, for `fit` to measure it by.
/// Between the path bar's parts.
const GAP: Rems = rems(0.125);
/// On each side of the path bar, inside its border.
const BAR_PADDING: Rems = rems(0.25);
/// On each side of a segment's name.
const PADDING: Rems = rems(0.375);
/// Before each segment but the first, as `.xsmall()` makes it.
const CHEVRON: Rems = rems(0.75);
const FONT_SIZE: Rems = rems(0.875);
/// The first segment's icon.
const ICON: Pixels = px(14.);
/// Between the first segment's icon and its name.
const ICON_GAP: Rems = rems(0.25);
/// The "…" for the folders that make way.
const ELLIPSIS_BUTTON: Rems = rems(1.375);
/// Names aren't truncated shorter than this, or there'd be too little left to tell them apart.
const MIN_NAME: Pixels = px(64.);

/// Fits segments with names `widths` wide into `room`, at the window's `rem`. Short of room, the
/// first folder gives way to its icon, then the longest names are truncated to the same width, then
/// the folders after the first make way from the outside in. The folder you're in stays whole for
/// as long as it can.
fn fit(widths: &[Pixels], room: Pixels, rem: Pixels) -> Fit {
    let [gap, padding, chevron, icon_gap, ellipsis_button] =
        [GAP, PADDING, CHEVRON, ICON_GAP, ELLIPSIS_BUTTON].map(|rems| rems.to_pixels(rem));
    let last = widths.len() - 1;
    let first = gap + padding * 2. + ICON;
    let after_first = gap + chevron + padding * 2.;
    let full = first + icon_gap + widths[0] + widths[1..].iter().map(|&w| after_first + w).sum::<Pixels>();
    if full <= room || last == 0 {
        return Fit::default();
    }
    for hidden in 0..last - 1 {
        let ellipsis = if hidden == 0 { Pixels::ZERO } else { gap + chevron + ellipsis_button };
        let shown = &widths[hidden + 1..last];
        let fixed = first + ellipsis + after_first * (shown.len() + 1) as f32 + widths[last];
        let cap = cap(shown, room - fixed);
        if cap.is_none_or(|cap| cap >= MIN_NAME) {
            return Fit { icon_only: true, hidden, cap };
        }
    }
    Fit { icon_only: true, hidden: last - 1, cap: None }
}

/// What comes before each segment but the first.
fn chevron(theme: &Theme) -> Icon {
    Icon::new(IconName::ChevronRight).xsmall().flex_none().text_color(theme.muted_foreground)
}

/// The "…" in the path bar, with a menu of the folders that made way for it.
fn hidden_folders(hidden: Vec<(SharedString, PathBuf)>, cx: &Context<Workspace>) -> impl IntoElement + use<> {
    let workspace = cx.entity().downgrade();
    h_flex()
        .id("hidden-folders")
        .flex_none()
        .child(chevron(cx.theme()))
        .child(
            Button::new("hidden-folders-menu")
                .ghost()
                .xsmall()
                .w(ELLIPSIS_BUTTON)
                .label("…")
                .text_color(cx.theme().muted_foreground)
                .dropdown_menu(move |menu, _, _| {
                    hidden.iter().fold(menu, |menu, (name, dir)| {
                        let (name, icon, dir, workspace) = (name.clone(), dir.clone(), dir.clone(), workspace.clone());
                        let item = PopupMenuItem::element(move |_, _| {
                            h_flex().gap_2().child(icons::item(&icon, true, ICON)).child(name.clone())
                        });
                        menu.item(item.on_click(move |_, window, cx| {
                            workspace.update(cx, |this, cx| this.show_folder(dir.clone(), window, cx)).ok();
                        }))
                    })
                }),
        )
        // Opening the menu isn't a click to type a path.
        .on_click(|_, _, cx| cx.stop_propagation())
}

/// The widest each of `widths` can be to add up to `room` at most, or none if they fit as they are.
fn cap(widths: &[Pixels], room: Pixels) -> Option<Pixels> {
    let mut sorted = widths.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut spent = Pixels::ZERO;
    for (ix, &width) in sorted.iter().enumerate() {
        let left = (sorted.len() - ix) as f32;
        if spent + width * left > room {
            return Some((room - spent) / left);
        }
        spent += width;
    }
    None
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
        let mut segments: Vec<(SharedString, PathBuf)> = path_segments(path, &home_dir(), &fs::icloud_drive())
            .into_iter()
            .map(|dir| (fs::display_name(&dir).into(), dir))
            .collect();
        let style = window.text_style();
        let rem = window.rem_size();
        let font_size = FONT_SIZE.to_pixels(rem);
        let widths: Vec<Pixels> = segments
            .iter()
            .map(|(name, _)| {
                window.text_system().shape_line(name.clone(), font_size, &[style.to_run(name.len())], None).width
            })
            .collect();
        let fit = match self.path_bar_width {
            // Before the first frame there's no width to go by yet.
            Pixels::ZERO => Fit::default(),
            // Less its border and padding.
            width => fit(&widths, width - px(2.) - BAR_PADDING.to_pixels(rem) * 2., rem),
        };
        let last = segments.len() - 1;
        // Short of room, names lose their middle, which keeps how they start and end apart.
        if let Some(cap) = fit.cap {
            let mut wrapper = cx.text_system().line_wrapper(style.font(), font_size);
            let shown = fit.hidden + 1..last;
            for ((name, _), &width) in segments[shown.clone()].iter_mut().zip(&widths[shown]) {
                if width > cap {
                    let runs = [style.to_run(name.len())];
                    *name = wrapper.truncate_line(name.clone(), cap, "…", &runs, TruncateFrom::Middle).0;
                }
            }
        }
        let segment = |ix: usize, name: SharedString, path: PathBuf| {
            let icon_only = ix == 0 && fit.icon_only;
            h_flex()
                // Only the folder you're in gives way, once nothing else can.
                .map(|el| if ix == last { el.min_w_0() } else { el.flex_none() })
                .when(ix > 0, |el| el.child(chevron(theme)))
                .child(
                    h_flex()
                        .id(("segment", ix))
                        .min_w_0()
                        .px(PADDING)
                        .py_0p5()
                        .gap(ICON_GAP)
                        .rounded_sm()
                        .cursor_pointer()
                        .when(ix != last, |el| el.text_color(theme.muted_foreground))
                        .hover(|s| s.bg(theme.secondary_hover).text_color(theme.foreground))
                        .drop_target()
                        .when(ix == 0, |el| el.child(icons::item(&path, true, ICON)))
                        .map(|el| match icon_only {
                            true => el.tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx)),
                            false => el.child(truncated(name).text_ellipsis_middle()),
                        })
                        .map(|el| opens_folder(el, path.clone(), cx))
                        .on_drop_files(move |paths, window, cx| {
                            drag::drop_into(paths, path.clone(), window, cx).detach()
                        }),
                )
        };
        let mut segments = segments.into_iter().enumerate();
        let root = segments.next().map(|(ix, (name, path))| segment(ix, name, path));
        let hidden: Vec<_> = segments.by_ref().take(fit.hidden).map(|(_, segment)| segment).collect();
        h_flex()
            .id("path-bar")
            .h(px(26.))
            .px(BAR_PADDING)
            .gap(GAP)
            .overflow_hidden()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.input)
            .bg(theme.background)
            .text_size(FONT_SIZE)
            .cursor_text()
            .children(root)
            .when(!hidden.is_empty(), |el| el.child(hidden_folders(hidden, cx)))
            .children(segments.map(|(ix, (name, path))| segment(ix, name, path)))
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
            let parent = fs::icloud::enclosing(dir).map(|dir| shown_path(&dir)).unwrap_or_default();
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

    use gpui_kit::{Pixels, px};

    use super::{Fit, fit, path_segments};

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
    fn an_apps_folder_is_in_icloud_drive() {
        let root = "/Users/me/Library/Mobile Documents";
        let (icloud, app) = (format!("{root}/com~apple~CloudDocs"), format!("{root}/com~apple~TextEdit/Documents"));
        assert_eq!(segments(&app), paths(&[&icloud, &app]));
        assert_eq!(segments(&format!("{app}/Notes")), paths(&[&icloud, &app, &format!("{app}/Notes")]));
        // iCloud Drive's own Documents is just a folder in it.
        let own = format!("{icloud}/Documents");
        assert_eq!(segments(&own), paths(&[&icloud, &own]));
    }

    #[test]
    fn segments_elsewhere_start_at_the_startup_disk() {
        assert_eq!(segments("/"), paths(&["/"]));
        assert_eq!(segments("/usr/local"), paths(&["/", "/usr", "/usr/local"]));
        assert_eq!(segments("/Volumes"), paths(&["/", "/Volumes"]));
        assert_eq!(segments("/Users/someone"), paths(&["/", "/Users", "/Users/someone"]));
    }

    /// Names 40, 90, 50, 150 and 50 wide take 516 in all, or 472 with the first as an icon.
    fn fit_in(room: f32) -> Fit {
        let widths: Vec<Pixels> = [40., 90., 50., 150., 50.].into_iter().map(px).collect();
        fit(&widths, px(room), px(16.))
    }

    #[test]
    fn segments_that_fit_stay_whole() {
        assert_eq!(fit_in(516.), Fit::default());
    }

    #[test]
    fn short_of_room_the_first_folder_shows_its_icon() {
        assert_eq!(fit_in(515.), Fit { icon_only: true, hidden: 0, cap: None });
        assert_eq!(fit_in(472.), Fit { icon_only: true, hidden: 0, cap: None });
    }

    #[test]
    fn then_the_longest_names_are_truncated() {
        assert_eq!(fit_in(432.), Fit { icon_only: true, hidden: 0, cap: Some(px(110.)) });
        // The next longest is truncated with it, once it's as short.
        assert_eq!(fit_in(372.), Fit { icon_only: true, hidden: 0, cap: Some(px(70.)) });
    }

    #[test]
    fn then_folders_make_way_from_the_outside_in() {
        assert_eq!(fit_in(359.), Fit { icon_only: true, hidden: 1, cap: Some(px(117.)) });
        assert_eq!(fit_in(305.), Fit { icon_only: true, hidden: 2, cap: Some(px(139.)) });
        assert_eq!(fit_in(229.), Fit { icon_only: true, hidden: 3, cap: None });
        assert_eq!(fit_in(40.), Fit { icon_only: true, hidden: 3, cap: None });
    }

    #[test]
    fn a_single_folder_is_never_hidden() {
        assert_eq!(fit(&[px(500.)], px(100.), px(16.)), Fit::default());
    }
}
