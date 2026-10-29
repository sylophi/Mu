//! An editor tab for quick changes without leaving Mu: syntax highlighting, find (⌘F) and
//! save (⌘S).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui_kit::component::{
    ActiveTheme as _,
    input::{Editor, EditorState, InputEvent, Position, TabSize},
    v_flex,
};
use gpui_kit::*;

use crate::{fs, theme::Toasts as _, workspace::TabEvent};

gpui_kit::actions!(editor, [Save]);

const CONTEXT: &str = "FileEditor";
/// Bigger files are better off in a real editor.
const SIZE_LIMIT: u64 = 8 * 1024 * 1024;

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-s", Save, Some(CONTEXT))]);
}

pub struct FileEditor {
    path: PathBuf,
    state: Entity<EditorState>,
    error: Option<SharedString>,
    loading: bool,
    /// The line, 0-based, to put the cursor on once the file's loaded.
    line: Option<u32>,
    dirty: bool,
    _subscriptions: Vec<Subscription>,
    _task: Task<()>,
}

impl EventEmitter<TabEvent> for FileEditor {}

impl FileEditor {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language(&path))
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
            cx.observe(&state, |_, _, cx| {
                cx.notify();
                cx.emit(TabEvent::Changed);
            }),
        ];
        let load = cx.background_spawn({
            let path = path.clone();
            async move { read_text(&path) }
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let text = load.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match text {
                    Ok(text) => {
                        this.state.update(cx, |state, cx| state.set_value(text, window, cx));
                        if let Some(line) = this.line.take() {
                            this.move_to_line(line, window, cx);
                        }
                    }
                    Err(error) => this.error = Some(format!("{error:#}").into()),
                }
                cx.notify();
            })
            .ok();
        });
        Self {
            path,
            state,
            error: None,
            loading: true,
            line: None,
            dirty: false,
            _subscriptions: subscriptions,
            _task: task,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn title(&self) -> SharedString {
        fs::display_name(&self.path).into()
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
            false => self.move_to_line(line, window, cx),
        }
    }

    /// Moves the cursor without taking focus, which `set_cursor_position` does, so a file that
    /// finishes loading in a background tab leaves focus where it is.
    fn move_to_line(&self, line: u32, window: &mut Window, cx: &mut App) {
        let focused = window.focused(cx);
        self.state.update(cx, |state, cx| state.set_cursor_position(Position::new(line, 0), window, cx));
        if let Some(focused) = focused {
            focused.focus(window, cx);
        }
    }

    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        if self.loading || self.error.is_some() {
            return Task::ready(false);
        }
        let text = self.state.read(cx).value();
        let saved = text.clone();
        let path = self.path.clone();
        let write = cx.background_spawn(async move {
            std::fs::write(&path, text.as_bytes())
                .with_context(|| format!("Couldn't save “{}”", fs::display_name(&path)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = write.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    // Typing during the save makes changes the file doesn't have yet.
                    this.dirty = this.state.read(cx).value() != saved;
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
