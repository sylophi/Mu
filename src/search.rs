//! Find in folder: file names and contents under a folder, skipping what `.gitignore` ignores,
//! hidden files and binaries. Results stream in as they're found.

use std::{
    ops::Range,
    os::macos::fs::MetadataExt as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use regex::{Regex, RegexBuilder};

use crate::{
    fs::{self, shown_path},
    icons,
    workspace::TabEvent,
};

/// How many matching lines a search shows before it stops.
const MAX_MATCHES: usize = 5000;
const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024;
const MAX_LINE_LENGTH: usize = 400;
const ROW_HEIGHT: Pixels = px(24.);

#[derive(Clone)]
enum Row {
    NamesHeading,
    Name { path: PathBuf, relative: SharedString, is_dir: bool, ranges: Vec<Range<usize>> },
    File { path: PathBuf, relative: SharedString },
    Line { path: PathBuf, line: u32, text: SharedString, ranges: Vec<Range<usize>> },
}

/// What one file yields.
struct Found {
    path: PathBuf,
    relative: SharedString,
    is_dir: bool,
    name_ranges: Vec<Range<usize>>,
    lines: Vec<(u32, SharedString, Vec<Range<usize>>)>,
}

pub struct Search {
    root: PathBuf,
    query: Entity<InputState>,
    case_sensitive: bool,
    regex: bool,
    names: Vec<Row>,
    contents: Vec<Row>,
    files: usize,
    matches: usize,
    searching: bool,
    truncated: bool,
    error: Option<SharedString>,
    scroll: UniformListScrollHandle,
    cancel: Arc<AtomicBool>,
    _subscription: Subscription,
    _search: Task<()>,
}

impl EventEmitter<TabEvent> for Search {}

impl Drop for Search {
    /// Stops the walk, which otherwise runs until its next batch finds nobody listening.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Search {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search file names and contents"));
        let subscription = cx.subscribe(&query, |this, _, event, cx| {
            if let InputEvent::Change = event {
                this.search(cx);
                cx.emit(TabEvent::Changed);
            }
        });
        Self {
            root,
            query,
            case_sensitive: false,
            regex: false,
            names: Vec::new(),
            contents: Vec::new(),
            files: 0,
            matches: 0,
            searching: false,
            truncated: false,
            error: None,
            scroll: UniformListScrollHandle::new(),
            cancel: Arc::default(),
            _subscription: subscription,
            _search: Task::ready(()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn title(&self, cx: &App) -> SharedString {
        let query = self.query.read(cx).value();
        if query.is_empty() { "Search".into() } else { format!("Search: {query}").into() }
    }

    pub fn focus_query(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |query, cx| {
            query.focus(window, cx);
            query.select_all(window, cx);
        });
    }

    fn pattern(&self, cx: &App) -> Option<Result<Regex, regex::Error>> {
        let query = self.query.read(cx).value();
        if query.trim().is_empty() {
            return None;
        }
        let source = if self.regex { query.to_string() } else { regex::escape(&query) };
        // Smart case: lowercase searches ignore case unless asked not to.
        let ignore_case = !self.case_sensitive && !query.chars().any(char::is_uppercase);
        Some(RegexBuilder::new(&source).case_insensitive(ignore_case).build())
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        self.names.clear();
        self.contents.clear();
        self.files = 0;
        self.matches = 0;
        self.truncated = false;
        self.error = None;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.searching = false;
        self._search = Task::ready(());
        let pattern = match self.pattern(cx) {
            None => return cx.notify(),
            Some(Err(error)) => {
                self.error = Some(error.to_string().lines().last().unwrap_or_default().to_string().into());
                return cx.notify();
            }
            Some(Ok(pattern)) => pattern,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        self.searching = true;
        let root = self.root.clone();
        let (tx, rx) = smol::channel::bounded::<Vec<Found>>(16);
        self._search = cx.spawn(async move |this, cx| {
            // Typing fast shouldn't start a walk per keystroke.
            cx.background_executor().timer(Duration::from_millis(120)).await;
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            std::thread::spawn(move || walk(&root, &pattern, &cancel, &tx));
            while let Ok(batch) = rx.recv().await {
                if this.update(cx, |this, cx| this.add(batch, cx)).is_err() {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.searching = false;
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn add(&mut self, batch: Vec<Found>, cx: &mut Context<Self>) {
        for mut found in batch {
            if self.truncated {
                break;
            }
            if !found.name_ranges.is_empty() {
                if self.names.is_empty() {
                    self.names.push(Row::NamesHeading);
                }
                self.names.push(Row::Name {
                    path: found.path.clone(),
                    relative: found.relative.clone(),
                    is_dir: found.is_dir,
                    ranges: found.name_ranges,
                });
            }
            if found.lines.is_empty() {
                continue;
            }
            found.lines.truncate(MAX_MATCHES - self.matches);
            self.files += 1;
            self.matches += found.lines.len();
            self.contents.push(Row::File { path: found.path.clone(), relative: found.relative });
            self.contents.extend(found.lines.into_iter().map(|(line, text, ranges)| Row::Line {
                path: found.path.clone(),
                line,
                text,
                ranges,
            }));
            if self.matches == MAX_MATCHES {
                self.truncated = true;
                self.cancel.store(true, Ordering::Relaxed);
            }
        }
        cx.notify();
    }

    fn row(&self, ix: usize) -> Option<&Row> {
        self.names.get(ix).or_else(|| self.contents.get(ix - self.names.len()))
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(row) = self.row(ix).cloned() else { return div().into_any_element() };
        let highlight = HighlightStyle {
            background_color: Some(theme.yellow.opacity(0.25)),
            color: Some(theme.foreground),
            ..Default::default()
        };
        let highlighted = |text: SharedString, ranges: Vec<Range<usize>>| {
            StyledText::new(text).with_highlights(ranges.into_iter().map(|range| (range, highlight)))
        };
        let row_base =
            |id: ElementId| h_flex().id(id).h(ROW_HEIGHT).w_full().px_3().gap_2().text_sm().whitespace_nowrap();
        match row {
            Row::NamesHeading => row_base(("names", ix).into())
                .text_xs()
                .font_medium()
                .text_color(theme.muted_foreground)
                .child("File names")
                .into_any_element(),
            Row::Name { path, relative, is_dir, ranges } => row_base(("name", ix).into())
                .cursor_pointer()
                .hover(|s| s.bg(theme.list_hover))
                .child(icons::item(&path, is_dir, px(14.)))
                .child(div().min_w_0().truncate().text_ellipsis_middle().child(highlighted(relative, ranges)))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(TabEvent::Reveal(vec![path.clone()]))))
                .into_any_element(),
            Row::File { path, relative } => {
                let name = fs::display_name(&path);
                let folder = relative.rsplit_once('/').map(|(folder, _)| folder.to_string()).unwrap_or_default();
                row_base(("file", ix).into())
                    .pt_1()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .child(icons::item(&path, false, px(14.)))
                    .child(div().font_medium().child(name))
                    .child(div().min_w_0().truncate().text_xs().text_color(theme.muted_foreground).child(folder))
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(TabEvent::Edit { path: path.clone(), line: None })),
                    )
                    .into_any_element()
            }
            Row::Line { path, line, text, ranges } => row_base(("line", ix).into())
                .pl(px(34.))
                .cursor_pointer()
                .hover(|s| s.bg(theme.list_hover))
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .child(
                    div()
                        .w(px(44.))
                        .flex_none()
                        .text_right()
                        .text_color(theme.muted_foreground)
                        .child((line + 1).to_string()),
                )
                .child(div().min_w_0().truncate().text_color(theme.muted_foreground).child(highlighted(text, ranges)))
                .on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(TabEvent::Edit { path: path.clone(), line: Some(line) })),
                )
                .into_any_element(),
        }
    }
}

impl Focusable for Search {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl Render for Search {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let root = shown_path(&self.root);
        let has_query = !self.query.read(cx).value().trim().is_empty();
        let summary = match (&self.error, has_query) {
            (Some(error), _) => error.to_string(),
            (None, false) => format!("in {root}"),
            (None, true) => {
                let mut summary = format!(
                    "{} in {} under {root}",
                    fs::count(self.matches, "match", "matches"),
                    fs::count(self.files, "file", "files")
                );
                if self.truncated {
                    summary.push_str(&format!(" (stopped at the first {MAX_MATCHES})"));
                }
                summary
            }
        };
        let count = self.names.len() + self.contents.len();
        let toggle = |id: &'static str, label: &'static str, tooltip: &'static str, on: bool| {
            Button::new(id).ghost().xsmall().label(label).selected(on).tooltip(tooltip)
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                v_flex()
                    .flex_none()
                    .p_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Input::new(&self.query)
                            .prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground))
                            .suffix(
                                h_flex()
                                    .gap_0p5()
                                    .child(toggle("case", "Aa", "Match Case", self.case_sensitive).on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.case_sensitive = !this.case_sensitive;
                                            this.search(cx);
                                        }),
                                    ))
                                    .child(toggle("regex", ".*", "Regular Expression", self.regex).on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.regex = !this.regex;
                                            this.search(cx);
                                        }),
                                    )),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(if self.error.is_some() { theme.danger } else { theme.muted_foreground })
                            .when(self.searching, |el| el.child(Spinner::new().xsmall()))
                            .child(div().flex_1().min_w_0().child(summary)),
                    ),
            )
            .child(
                uniform_list(
                    "search-results",
                    count,
                    cx.processor(|this, range: Range<usize>, _, cx| range.map(|ix| this.render_row(ix, cx)).collect()),
                )
                .flex_1()
                .py_1()
                .track_scroll(&self.scroll),
            )
    }
}

/// Walks `root` on a plain thread, sending what it finds in batches until done or cancelled.
fn walk(root: &Path, pattern: &Regex, cancel: &AtomicBool, tx: &smol::channel::Sender<Vec<Found>>) {
    let mut batch = Vec::new();
    let mut sent_at = std::time::Instant::now();
    for entry in ignore::WalkBuilder::new(root).hidden(true).follow_links(false).build().flatten() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let path = entry.path();
        if path == root {
            continue;
        }
        let relative: SharedString = path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned().into();
        let name_start = relative.rfind('/').map_or(0, |slash| slash + 1);
        let name_ranges: Vec<_> =
            pattern.find_iter(&relative[name_start..]).map(|m| m.start() + name_start..m.end() + name_start).collect();
        let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        let is_file = entry.file_type().is_some_and(|kind| kind.is_file());
        let lines = if is_file { search_file(path, pattern) } else { Vec::new() };
        if !name_ranges.is_empty() || !lines.is_empty() {
            batch.push(Found { path: path.to_path_buf(), relative, is_dir, name_ranges, lines });
        }
        if !batch.is_empty() && sent_at.elapsed() > Duration::from_millis(50) {
            if tx.send_blocking(std::mem::take(&mut batch)).is_err() {
                return;
            }
            sent_at = std::time::Instant::now();
        }
    }
    tx.send_blocking(batch).ok();
}

fn search_file(path: &Path, pattern: &Regex) -> Vec<(u32, SharedString, Vec<Range<usize>>)> {
    // Reading what's only in iCloud would download it.
    let skip =
        |metadata: std::fs::Metadata| metadata.len() > MAX_FILE_SIZE || metadata.st_flags() & fs::SF_DATALESS != 0;
    if std::fs::metadata(path).map_or(true, skip) {
        return Vec::new();
    }
    let Ok(bytes) = fs::read_text(path, MAX_FILE_SIZE) else { return Vec::new() };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let ranges: Vec<_> = pattern.find_iter(line).map(|m| m.range()).collect();
        if ranges.is_empty() {
            continue;
        }
        // Show a long line from a little before its first match.
        let start = line.floor_char_boundary(ranges[0].start.saturating_sub(40));
        let end = line.floor_char_boundary(start + MAX_LINE_LENGTH);
        let shown = line[start..end].trim_end();
        let indent = shown.len() - shown.trim_start().len();
        let (start, shown) = (start + indent, shown.trim_start());
        let ranges = ranges
            .into_iter()
            .filter(|range| range.start >= start && range.end <= start + shown.len())
            .map(|range| range.start - start..range.end - start)
            .collect();
        lines.push((number as u32, shown.to_string().into(), ranges));
    }
    lines
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::{MAX_LINE_LENGTH, search_file};
    use crate::fs::Scratch;

    #[test]
    fn matching_lines() {
        let scratch = Scratch::new("search");
        let dir = &scratch.0;
        let pattern = Regex::new("needle").unwrap();
        // Three-byte characters around the match put both ends of the shown window mid-character.
        let long = format!("{}needle{}", "€".repeat(200), "€".repeat(200));
        std::fs::write(dir.join("text"), format!("  αβγ needle\nnothing\n{long}\n")).unwrap();
        std::fs::write(dir.join("binary"), b"needle\0").unwrap();

        let lines = search_file(&dir.join("text"), &pattern);
        assert_eq!(lines.iter().map(|line| line.0).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(lines[0].1.as_ref(), "αβγ needle");
        assert!(lines[1].1.len() <= MAX_LINE_LENGTH);
        for (_, text, ranges) in &lines {
            assert_eq!(ranges.len(), 1);
            assert_eq!(&text[ranges[0].clone()], "needle");
        }
        assert!(search_file(&dir.join("binary"), &pattern).is_empty());
    }
}
