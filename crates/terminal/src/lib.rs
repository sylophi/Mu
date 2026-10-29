//! A terminal view: a shell in a PTY, emulated by alacritty_terminal and drawn with GPUI.

mod keys;
mod powerline;
mod render;

pub use render::AnsiColors;

use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::{CStr, OsStr},
    ops::Range,
    os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
    path::PathBuf,
    sync::Arc,
};

use alacritty_terminal::{
    event::{Event as AlacEvent, EventListener, Notify, WindowSize},
    event_loop::{EventLoop, Msg, Notifier},
    grid::Scroll,
    index::{Column, Line, Point as GridPoint, Side},
    selection::{Selection, SelectionType},
    sync::FairMutex,
    term::{Config, Term, TermMode, test::TermSize},
    tty,
};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::*;

// The app handles ShowInFiles, which the terminal only offers in its menu.
actions!(terminal, [Copy, Paste, Clear, SendTab, SendBacktab, ShowInFiles]);

const CONTEXT: &str = "Terminal";

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, Some(CONTEXT)),
        KeyBinding::new("cmd-v", Paste, Some(CONTEXT)),
        KeyBinding::new("cmd-k", Clear, Some(CONTEXT)),
        // Root binds tab to focus traversal, but inside a terminal it belongs to the shell.
        KeyBinding::new("tab", SendTab, Some(CONTEXT)),
        KeyBinding::new("shift-tab", SendBacktab, Some(CONTEXT)),
    ]);
}

/// What a terminal tells the app it's in.
pub enum TerminalEvent {
    /// What `title` gives changed, as the shell set its title or reset it.
    TitleChanged,
    /// The shell exited.
    Exited,
}

#[derive(Clone)]
struct Listener(smol::channel::Sender<AlacEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: AlacEvent) {
        self.0.try_send(event).ok();
    }
}

/// The grid size and cell metrics from the last layout.
#[derive(Clone, Copy)]
struct Geometry {
    origin: Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
    columns: usize,
    lines: usize,
}

impl Geometry {
    /// Where the cell at `row` and `column` on screen starts.
    fn cell_origin(&self, row: usize, column: usize) -> Point<Pixels> {
        point(self.origin.x + self.cell_width * column as f32, self.origin.y + self.line_height * row as f32)
    }

    /// Where `count` cells from `row` and `column` on are.
    fn cells(&self, row: usize, column: usize, count: usize) -> Bounds<Pixels> {
        Bounds::new(self.cell_origin(row, column), size(self.cell_width * count as f32, self.line_height))
    }
}

pub struct TerminalView {
    term: Arc<FairMutex<Term<Listener>>>,
    notifier: Notifier,
    focus_handle: FocusHandle,
    /// Our own handle on the PTY, to find what runs in the foreground and where. The event loop
    /// closes its handle when the shell exits, and that descriptor number can then be reused.
    pty_file: std::fs::File,
    /// Where the shell started.
    cwd: PathBuf,
    title: Option<String>,
    geometry: Geometry,
    /// Scroll delta not yet worth a whole line.
    scroll_remainder: Pixels,
    /// IME preedit text, drawn at the cursor until committed.
    marked_text: Option<String>,
    /// The mouse button being reported to an app in mouse mode.
    reported_button: Option<u8>,
    exited: bool,
    _events: Task<()>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl TerminalView {
    pub fn new(cwd: PathBuf, cx: &mut Context<Self>) -> Self {
        let (events_tx, events_rx) = smol::channel::unbounded();
        let listener = Listener(events_tx);
        let geometry =
            Geometry { origin: Point::default(), cell_width: px(8.), line_height: px(16.), columns: 80, lines: 24 };
        let term = Arc::new(FairMutex::new(Term::new(
            Config { kitty_keyboard: true, ..Config::default() },
            &TermSize::new(geometry.columns, geometry.lines),
            listener.clone(),
        )));

        let mut env = HashMap::from([
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
        ]);
        // Apps opened from Finder get no locale, which leaves the shell in ASCII.
        if std::env::var_os("LANG").is_none() {
            env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
        }
        // No shell given: alacritty runs $SHELL (or the passwd shell) as a login shell.
        let options = tty::Options { shell: None, working_directory: Some(cwd.clone()), drain_on_exit: true, env };
        let pty = tty::new(&options, window_size(&geometry), 0).expect("failed to open a PTY");
        let pty_file = pty.file().try_clone().expect("failed to duplicate the PTY");
        let event_loop =
            EventLoop::new(term.clone(), listener, pty, true, false).expect("failed to start the terminal event loop");
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();

        let events = cx.spawn(async move |this, cx| {
            while let Ok(event) = events_rx.recv().await {
                let mut batch = vec![event];
                while let Ok(event) = events_rx.try_recv() {
                    batch.push(event);
                }
                if this.update(cx, |this, cx| this.handle_events(batch, cx)).is_err() {
                    break;
                }
            }
        });

        Self {
            term,
            notifier,
            focus_handle: cx.focus_handle(),
            pty_file,
            cwd,
            title: None,
            geometry,
            scroll_remainder: Pixels::ZERO,
            marked_text: None,
            reported_button: None,
            exited: false,
            _events: events,
        }
    }

    pub fn title(&self) -> SharedString {
        match &self.title {
            Some(title) => title.clone().into(),
            None => {
                let cwd = self.cwd();
                cwd.file_name().unwrap_or(cwd.as_os_str()).to_string_lossy().to_string().into()
            }
        }
    }

    /// The current folder of the shell (or whatever runs in the foreground), asked of the kernel
    /// since not every shell reports it.
    pub fn cwd(&self) -> PathBuf {
        if self.exited {
            return self.cwd.clone();
        }
        let foreground = unsafe { libc::tcgetpgrp(self.pty_file.as_raw_fd()) };
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as i32;
        let read =
            unsafe { libc::proc_pidinfo(foreground, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
        if foreground <= 0 || read != size {
            return self.cwd.clone();
        }
        let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
        PathBuf::from(OsStr::from_bytes(path.to_bytes()))
    }

    /// Writes text to the shell as if typed.
    pub fn send_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.write(text.as_bytes().to_vec(), cx);
    }

    fn write(&mut self, bytes: impl Into<Cow<'static, [u8]>>, cx: &mut Context<Self>) {
        let mut term = self.term.lock();
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
            cx.notify();
        }
        drop(term);
        self.notifier.notify(bytes);
    }

    fn handle_events(&mut self, events: Vec<AlacEvent>, cx: &mut Context<Self>) {
        let mut dirty = false;
        for event in events {
            match event {
                AlacEvent::Wakeup | AlacEvent::MouseCursorDirty | AlacEvent::CursorBlinkingChange => {
                    dirty = true;
                }
                AlacEvent::Title(title) => {
                    self.title = Some(title);
                    cx.emit(TerminalEvent::TitleChanged);
                }
                AlacEvent::ResetTitle => {
                    self.title = None;
                    cx.emit(TerminalEvent::TitleChanged);
                }
                AlacEvent::PtyWrite(text) => self.notifier.notify(text.into_bytes()),
                AlacEvent::ClipboardStore(_, text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                AlacEvent::ClipboardLoad(_, format) => {
                    let text = cx.read_from_clipboard().and_then(|item| item.text());
                    self.notifier.notify(format(&text.unwrap_or_default()).into_bytes());
                }
                AlacEvent::ColorRequest(index, format) => {
                    let color = render::Palette::new(cx).request(index, self.term.lock().colors());
                    self.notifier.notify(format(color).into_bytes());
                }
                AlacEvent::TextAreaSizeRequest(format) => {
                    self.notifier.notify(format(window_size(&self.geometry)).into_bytes());
                }
                AlacEvent::Exit => {
                    if !self.exited {
                        self.exited = true;
                        cx.emit(TerminalEvent::Exited);
                    }
                }
                AlacEvent::ChildExit(_) | AlacEvent::Bell => {}
            }
        }
        if dirty {
            cx.notify();
        }
    }

    /// Resizes the PTY and grid when the element's size or font metrics change.
    fn set_geometry(&mut self, geometry: Geometry) {
        // Anything but where it is on screen.
        let size = |g: &Geometry| (g.columns, g.lines, g.cell_width, g.line_height);
        let resized = size(&geometry) != size(&self.geometry);
        self.geometry = geometry;
        if resized {
            self.term.lock().resize(TermSize::new(geometry.columns, geometry.lines));
            self.notifier.0.send(Msg::Resize(window_size(&geometry))).ok();
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let mode = *self.term.lock().mode();
        let keystroke = &event.keystroke;

        if keystroke.modifiers.shift && !keystroke.modifiers.control && !mode.contains(TermMode::ALT_SCREEN) {
            let scroll = match keystroke.key.as_str() {
                "pageup" => Some(Scroll::PageUp),
                "pagedown" => Some(Scroll::PageDown),
                "home" => Some(Scroll::Top),
                "end" => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                self.term.lock().scroll_display(scroll);
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }

        if let Some(bytes) = keys::to_esc(keystroke, mode) {
            self.write(bytes, cx);
            cx.stop_propagation();
        }
    }

    /// A key the app binds, sent as if it had come to the terminal.
    fn send_key(&mut self, keystroke: &str, cx: &mut Context<Self>) {
        let mode = *self.term.lock().mode();
        if let Some(bytes) = keys::to_esc(&Keystroke::parse(keystroke).expect("it's a keystroke"), mode) {
            self.write(bytes, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.term.lock().selection_to_string() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.term.lock().mode().contains(TermMode::BRACKETED_PASTE) {
            let text = text.replace('\x1b', "");
            self.write(format!("\x1b[200~{text}\x1b[201~").into_bytes(), cx);
        } else {
            self.write(text.into_bytes(), cx);
        }
    }

    fn clear(&mut self, _: &Clear, _: &mut Window, cx: &mut Context<Self>) {
        let mut term = self.term.lock();
        term.grid_mut().clear_history();
        term.selection = None;
        drop(term);
        // Form feed: the shell clears the screen and redraws its prompt at the top.
        self.write(b"\x0c".as_slice(), cx);
        cx.notify();
    }

    /// The grid cell under a window position, and which half of it.
    fn grid_point(&self, position: Point<Pixels>, display_offset: usize) -> (GridPoint, Side) {
        let g = &self.geometry;
        let x = (position.x - g.origin.x).max(Pixels::ZERO);
        let y = (position.y - g.origin.y).max(Pixels::ZERO);
        let column = ((x / g.cell_width) as usize).min(g.columns - 1);
        let line = ((y / g.line_height) as usize).min(g.lines - 1);
        let side = if x - g.cell_width * column as f32 > g.cell_width / 2. { Side::Right } else { Side::Left };
        let point = GridPoint::new(Line(line as i32 - display_offset as i32), Column(column));
        (point, side)
    }

    fn reports_mouse(&self, modifiers: &Modifiers) -> bool {
        // Shift bypasses the app so text can still be selected.
        !modifiers.shift && self.term.lock().mode().intersects(TermMode::MOUSE_MODE)
    }

    fn report_mouse(&self, button: u8, pressed: bool, position: Point<Pixels>, modifiers: &Modifiers) {
        let mode = *self.term.lock().mode();
        let (point, _) = self.grid_point(position, 0);
        let mut code = button;
        if modifiers.shift {
            code += 4;
        }
        if modifiers.alt {
            code += 8;
        }
        if modifiers.control {
            code += 16;
        }
        let (column, line) = (point.column.0 + 1, point.line.0 as usize + 1);
        let bytes = if mode.contains(TermMode::SGR_MOUSE) {
            let end = if pressed { 'M' } else { 'm' };
            format!("\x1b[<{code};{column};{line}{end}").into_bytes()
        } else {
            let code = if pressed { code } else { 3 + (code & !3) };
            if column > 222 || line > 222 {
                return;
            }
            vec![0x1b, b'[', b'M', 32 + code, 32 + column as u8, 32 + line as u8]
        };
        self.notifier.notify(bytes);
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let button = match event.button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return,
        };
        if self.reports_mouse(&event.modifiers) {
            self.reported_button = Some(button);
            self.report_mouse(button, true, event.position, &event.modifiers);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        let mut term = self.term.lock();
        let (point, side) = self.grid_point(event.position, term.grid().display_offset());
        let kind = match event.click_count {
            1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        if event.click_count == 1 && event.modifiers.shift {
            if let Some(selection) = term.selection.as_mut() {
                selection.update(point, side);
            }
        } else {
            let mut selection = Selection::new(kind, point, side);
            if kind != SelectionType::Simple {
                // Word and line selections cover the clicked cell right away.
                selection.include_all();
            }
            term.selection = Some(selection);
        }
        drop(term);
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self.reported_button {
            if self.term.lock().mode().intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                self.report_mouse(button + 32, true, event.position, &event.modifiers);
            }
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let mut term = self.term.lock();
        let (point, side) = self.grid_point(event.position, term.grid().display_offset());
        if let Some(selection) = term.selection.as_mut() {
            selection.update(point, side);
            drop(term);
            cx.notify();
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if let Some(button) = self.reported_button.take() {
            self.report_mouse(button, false, event.position, &event.modifiers);
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line_height = self.geometry.line_height;
        self.scroll_remainder += event.delta.pixel_delta(line_height).y;
        let lines = (self.scroll_remainder / line_height) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= line_height * lines as f32;

        let mode = *self.term.lock().mode();
        if self.reports_mouse(&event.modifiers) {
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.abs() {
                self.report_mouse(button, true, event.position, &event.modifiers);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            let arrow = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.notifier.notify(arrow.repeat(lines.unsigned_abs() as usize).into_bytes());
        } else {
            self.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }
}

fn window_size(geometry: &Geometry) -> WindowSize {
    WindowSize {
        num_lines: geometry.lines as u16,
        num_cols: geometry.columns as u16,
        cell_width: f32::from(geometry.cell_width) as u16,
        cell_height: f32::from(geometry.line_height) as u16,
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        // The event loop owns the PTY, so shutting it down hangs up the shell.
        self.notifier.0.send(Msg::Shutdown).ok();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let mouse_mode = self.term.lock().mode().intersects(TermMode::MOUSE_MODE);
        let focus_handle = self.focus_handle.clone();
        let view = div()
            .id("terminal")
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::clear))
            .on_action(cx.listener(|this, _: &SendTab, _, cx| this.send_key("tab", cx)))
            .on_action(cx.listener(|this, _: &SendBacktab, _, cx| this.send_key("shift-tab", cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            // A drag reported to the app can end anywhere, and it still has to hear the release.
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .child(render::terminal_canvas(entity));
        // Apps that use the mouse get right clicks too.
        if mouse_mode {
            return view.into_any_element();
        }
        view.context_menu(move |menu, _, _| {
            menu.action_context(focus_handle.clone())
                .menu("Copy", Box::new(Copy))
                .menu("Paste", Box::new(Paste))
                .separator()
                .menu("Clear", Box::new(Clear))
                .menu("Show in Files", Box::new(ShowInFiles))
        })
        .into_any_element()
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        None
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text.as_ref().map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        self.write(text.as_bytes().to_vec(), cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let term = self.term.lock();
        let cursor = term.grid().cursor.point;
        let line = cursor.line.0 + term.grid().display_offset() as i32;
        let g = &self.geometry;
        Some(Bounds::new(
            point(g.origin.x + g.cell_width * cursor.column.0 as f32, g.origin.y + g.line_height * line as f32),
            size(g.cell_width, g.line_height),
        ))
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}
