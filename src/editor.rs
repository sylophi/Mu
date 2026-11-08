//! An editor tab for quick changes without leaving Mu, or a new file: syntax highlighting, find
//! (⌘F) and save (⌘S).

use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use gpui_kit::component::{
    ActiveTheme as _,
    input::{Editor, EditorState, InputEvent, Position, TabSize},
    notification::Notification,
    v_flex,
};
use gpui_kit::*;

use crate::{fs, session::Tab, theme::Toasts as _, workspace::TabEvent};

gpui_kit::actions!(editor, [Save]);

const CONTEXT: &str = "FileEditor";
/// Bigger files are better off in a real editor.
const SIZE_LIMIT: u64 = 8 * 1024 * 1024;
/// How long a change from another app waits for the rest of its burst.
const SETTLE: Duration = Duration::from_millis(120);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-s", Save, Some(CONTEXT))]);
}

/// What an editor saves to.
enum File {
    At(PathBuf),
    /// A new file, saved in this folder unless it's put somewhere else.
    New(PathBuf),
}

pub struct FileEditor {
    file: File,
    state: Entity<EditorState>,
    error: Option<SharedString>,
    loading: bool,
    /// The line, 0-based, to put the cursor on once the file's loaded.
    line: Option<u32>,
    dirty: bool,
    /// The file's text when last read or saved, which the text being edited started from.
    disk: SharedString,
    saving: bool,
    _subscriptions: Vec<Subscription>,
    _task: Task<()>,
    _watch: Task<()>,
}

impl EventEmitter<TabEvent> for FileEditor {}

impl FileEditor {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::blank(File::At(path.clone()), window, cx);
        this.loading = true;
        this.watch(path.clone(), window, cx);
        let load = cx.background_spawn(async move { read_text(&path) });
        this._task = cx.spawn_in(window, async move |this, cx| {
            let text = load.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                this.show(text, window, cx);
                if let Some(line) = this.line.take() {
                    this.move_cursor(Position::new(line, 0), window, cx);
                }
            })
            .ok();
        });
        this
    }

    /// An empty editor for a new file, saved in `dir` unless it's put somewhere else.
    pub fn untitled(dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::blank(File::New(dir), window, cx)
    }

    fn blank(file: File, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let language = match &file {
            File::At(path) => language(path),
            File::New(_) => String::new(),
        };
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .indent_guides(true)
                .soft_wrap(false)
                .tab_size(TabSize { tab_size: 4, hard_tabs: false })
        });
        let subscriptions = vec![
            cx.subscribe(&state, |this, _, event, cx| {
                if let InputEvent::Change = event {
                    this.dirty = true;
                }
                cx.notify();
            }),
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        Self {
            file,
            state,
            error: None,
            loading: false,
            line: None,
            dirty: false,
            disk: SharedString::default(),
            saving: false,
            _subscriptions: subscriptions,
            _task: Task::ready(()),
            _watch: Task::ready(()),
        }
    }

    /// Keeps up with what other apps do to the file at `path`. Many save by swapping in a new
    /// file, so it's the folder that's watched, and a link changes where it leads.
    fn watch(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self._watch = cx.spawn_in(window, async move |this, cx| {
            let file = cx.background_spawn(async move { std::fs::canonicalize(&path).unwrap_or(path) }).await;
            let Some(dir) = file.parent() else { return };
            let changes = fs::watch(vec![dir.to_owned()], false, SETTLE, cx.background_executor());
            while let Some(changed) = changes.next().await {
                if !changed.paths.contains(&file) {
                    continue;
                }
                let read = {
                    let file = file.clone();
                    cx.background_spawn(async move { read_text(&file) }).await
                };
                if this.update_in(cx, |this, window, cx| this.changed_outside(read, window, cx)).is_err() {
                    return;
                }
            }
        });
    }

    /// Follows its tab into another window, which it keeps up with the file in from then on.
    pub fn moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let File::At(path) = &self.file {
            self.watch(path.clone(), window, cx);
        }
    }

    /// Takes in `read`, what's in the file now that something's changed it. It only replaces what's
    /// here if that has nothing unsaved in it.
    fn changed_outside(&mut self, read: Result<String>, window: &mut Window, cx: &mut Context<Self>) {
        // What it would find could be half done, and a save looks at the disk for itself.
        if self.loading || self.saving {
            return;
        }
        let text = self.state.read(cx).value();
        match &read {
            // This is the only copy now, if it's showing the file at all.
            Err(error) if is_gone(error) => {
                self.dirty = self.error.is_none();
                cx.emit(TabEvent::Changed);
            }
            // Includes Mu's own saves.
            Ok(found) if *found == *text => {
                self.disk = text;
                self.dirty = false;
                self.error = None;
                cx.emit(TabEvent::Changed);
            }
            Ok(found) if *found == *self.disk => {}
            // A file it can't read now stays as it is here, as its text is still good to save.
            Ok(_) if text == self.disk => self.reload(read, window, cx),
            _ => {
                let note = Notification::warning(format!("“{}” changed on disk", self.title()));
                window.toast(note.id::<Self>(), cx);
            }
        }
    }

    /// Shows what's been read from the file, or why it can't be edited here.
    fn show(&mut self, read: Result<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.dirty = false;
        match read {
            Ok(text) => {
                self.error = None;
                self.disk = text.clone().into();
                self.state.update(cx, |state, cx| state.set_value(text, window, cx));
            }
            Err(error) => self.error = Some(format!("{error:#}").into()),
        }
        cx.emit(TabEvent::Changed);
        cx.notify();
    }

    /// Like `show`, staying where it was in the file.
    fn reload(&mut self, read: Result<String>, window: &mut Window, cx: &mut Context<Self>) {
        let (cursor, scroll) = {
            let state = self.state.read(cx);
            (state.cursor_position(), state.scroll_offset())
        };
        self.show(read, window, cx);
        self.move_cursor(cursor, window, cx);
        self.state.update(cx, |state, cx| state.set_scroll_offset(scroll, cx));
    }

    /// Where it's saved, unless it's a new file.
    pub fn path(&self) -> Option<&Path> {
        match &self.file {
            File::At(path) => Some(path),
            File::New(_) => None,
        }
    }

    /// The tab it is, to open it again.
    pub fn saved(&self) -> Tab {
        match &self.file {
            File::At(path) => Tab::Editor(path.clone()),
            File::New(dir) => Tab::Untitled(dir.clone()),
        }
    }

    pub fn title(&self) -> SharedString {
        self.path().map_or("Untitled".into(), |path| fs::display_name(path).into())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// "Ln 12, Col 4", 1-based like every editor shows it.
    pub fn cursor_label(&self, cx: &App) -> String {
        let position = self.state.read(cx).cursor_position();
        format!("Ln {}, Col {}", position.line + 1, position.character + 1)
    }

    /// Puts the cursor on `line`, 0-based, or once the file's loaded, if it's loading.
    pub fn go_to_line(&mut self, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        match self.loading {
            true => self.line = Some(line),
            false => self.move_cursor(Position::new(line, 0), window, cx),
        }
    }

    /// Moves the cursor without taking focus, which `set_cursor_position` does, so a file that
    /// finishes loading in a background tab leaves focus where it is.
    fn move_cursor(&self, position: Position, window: &mut Window, cx: &mut App) {
        let focused = window.focused(cx);
        self.state.update(cx, |state, cx| state.set_cursor_position(position, window, cx));
        if let Some(focused) = focused {
            focused.focus(window, cx);
        }
    }

    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if self.loading || self.saving || self.error.is_some() {
            return Task::ready(false);
        }
        self.saving = true;
        let saving = match &self.file {
            File::At(path) => self.save_over(path.clone(), window, cx),
            File::New(dir) => {
                let picked = cx.prompt_for_new_path(dir, Some("Untitled"));
                cx.spawn_in(window, async move |this, cx| {
                    let Ok(Ok(Some(path))) = picked.await else { return Ok(false) };
                    let write = this.update_in(cx, |this, window, cx| this.write(path.clone(), window, cx))?;
                    let saved = write.await;
                    if saved {
                        // It's that file from now on, highlighted as its name says.
                        this.update_in(cx, |this, window, cx| {
                            this.state.update(cx, |state, cx| state.set_highlighter(language(&path), cx));
                            this.file = File::At(path.clone());
                            this.watch(path, window, cx);
                            cx.emit(TabEvent::Changed);
                        })?;
                    }
                    Ok(saved)
                })
            }
        };
        cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await.unwrap_or(false);
            this.update(cx, |this, _| this.saving = false).ok();
            saved
        })
    }

    /// Saves to the file at `path`, asking first if something else changed it. Told right before
    /// writing, as a watcher hears of a change only after a moment.
    fn save_over(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Task<Result<bool>> {
        let changed = cx.background_spawn({
            let (path, disk) = (path.clone(), self.disk.clone());
            async move { std::fs::read(path).is_ok_and(|bytes| bytes != disk.as_bytes()) }
        });
        cx.spawn_in(window, async move |this, cx| {
            if changed.await {
                let answer = this.update_in(cx, |this, window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        &format!("“{}” changed on disk", this.title()),
                        Some("Overwrite replaces that version with yours. Revert discards your changes."),
                        &["Overwrite", "Revert", "Cancel"],
                        cx,
                    )
                })?;
                match answer.await {
                    Ok(0) => {}
                    Ok(1) => {
                        let read = cx.background_spawn(async move { read_text(&path) }).await;
                        this.update_in(cx, |this, window, cx| this.reload(read, window, cx))?;
                        return Ok(false);
                    }
                    _ => return Ok(false),
                }
            }
            let write = this.update_in(cx, |this, window, cx| this.write(path, window, cx))?;
            Ok(write.await)
        })
    }

    fn write(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let text = self.state.read(cx).value();
        let saved = text.clone();
        let write = cx.background_spawn(async move { fs::save(&path, text.as_bytes()) });
        cx.spawn_in(window, async move |this, cx| {
            let result = write.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    // Typing during the save makes changes the file doesn't have yet.
                    this.dirty = this.state.read(cx).value() != saved;
                    this.disk = saved;
                    cx.notify();
                    cx.emit(TabEvent::Changed);
                    true
                }
                Err(error) => {
                    window.toast_error(&error, cx);
                    false
                }
            })
            .unwrap_or(false)
        })
    }
}

impl Focusable for FileEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.focus_handle(cx)
    }
}

impl Render for FileEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let content = match &self.error {
            // Tracks the editor's focus so the tab's keys keep working.
            Some(error) => div()
                .track_focus(&self.state.focus_handle(cx))
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(error.clone())
                .into_any_element(),
            None => Editor::new(&self.state)
                .bordered(false)
                .h_full()
                .font_family(theme.mono_font_family.clone())
                .text_size(theme.mono_font_size)
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .key_context(CONTEXT)
            .bg(theme.background)
            .on_action(cx.listener(|this, _: &Save, window, cx| this.save(window, cx).detach()))
            .child(content)
    }
}

/// The text of the file at `path`, or what keeps it from being edited here.
fn read_text(path: &Path) -> Result<String> {
    // Told by its size, as reading it all could be slow, like from a server or iCloud.
    if std::fs::metadata(path)?.len() > SIZE_LIMIT {
        bail!("This file is too big to edit here.");
    }
    let bytes = fs::read_text(path, SIZE_LIMIT)?;
    String::from_utf8(bytes).map_err(|_| anyhow!("This file isn't UTF-8 text."))
}

fn is_gone(error: &anyhow::Error) -> bool {
    error.downcast_ref::<io::Error>().is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
}

/// The highlighting language for a file, by name or extension.
fn language(path: &Path) -> String {
    let name = path.file_name().map(|name| name.to_string_lossy().to_lowercase()).unwrap_or_default();
    let extension = path.extension().map(|ext| ext.to_string_lossy().to_lowercase()).unwrap_or_default();
    match (name.as_str(), extension.as_str()) {
        ("makefile" | "gnumakefile", _) | (_, "mk") => "make",
        (".zshrc" | ".zprofile" | ".zshenv" | ".bashrc" | ".bash_profile" | ".profile", _) => "bash",
        (_, "zsh" | "bash") => "bash",
        (_, "jsx" | "mjs" | "cjs") => "javascript",
        (_, "mts" | "cts") => "typescript",
        (_, "h") => "c",
        (_, "hpp" | "cc" | "cxx" | "hh") => "cpp",
        (_, extension) => extension,
    }
    .to_string()
}
