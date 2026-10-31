//! The preview pane: the selected item's contents and its details. Images and text show as they
//! are, Quick Look pictures most other files, like a PDF's first page or a frame of a video, and
//! folders and apps show their icons. Below go its details.

mod details;

use std::{path::PathBuf, sync::Arc};

use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::*;

use crate::{
    browser::Browser,
    fs::{self, Category, Entry},
    icons,
    thumbnails::Cache,
};
use details::Details;

/// Enough text to fill the pane without reading a whole log file.
const TEXT_LIMIT: u64 = 48 * 1024;

/// The extensions of the images GPUI decodes itself, which show as they are. Quick Look pictures
/// other kinds, like HEIC.
const GPUI_DECODES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "ico", "svg"];

/// The largest picture Quick Look makes for the pane.
const PICTURE: Pixels = px(512.);
/// The size of the icon shown for what there's no picture of.
const ICON: Pixels = px(96.);

/// What the pane shows above the item's details.
enum Content {
    None,
    /// The image at this path, which GPUI decodes, in a cache of its own. GPUI's would keep it
    /// for as long as Mu runs and show it as it was after it changes, while this one lets go of
    /// it once the pane moves on.
    Image(PathBuf, Entity<RetainAllImageCache>),
    /// A text file's lines, and which of them is the widest.
    Text(Arc<[SharedString]>, usize),
    /// A folder's icon, and how many items it holds.
    Folder(usize),
    /// The icon macOS draws for an app or another package.
    Icon,
    /// Quick Look's picture of it, or its icon until there is one, or if there's none.
    Picture,
}

pub struct Preview {
    browser: Option<(WeakEntity<Browser>, Subscription)>,
    target: Option<Entry>,
    content: Content,
    details: Details,
    pictures: Cache,
    /// Where the text is scrolled to. Each item starts at its top.
    scroll: UniformListScrollHandle,
    /// Whether the pane is shown. While it's hidden, there's no reading files.
    shown: bool,
    _load: Task<()>,
    _details: Task<()>,
}

impl Preview {
    pub fn new(shown: bool, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| this.pictures.clear(None, cx)).detach();
        Self {
            browser: None,
            target: None,
            content: Content::None,
            details: Details::default(),
            pictures: Cache::new(PICTURE),
            scroll: UniformListScrollHandle::new(),
            shown,
            _load: Task::ready(()),
            _details: Task::ready(()),
        }
    }

    /// Shows whatever is selected in `browser` from now on.
    pub fn follow(&mut self, browser: &Entity<Browser>, cx: &mut Context<Self>) {
        self.browser = Some((browser.downgrade(), cx.observe(browser, |this, _, cx| this.refresh(cx))));
        self.refresh(cx);
    }

    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        self.refresh(cx);
    }

    /// Catches up with the selection, while the pane is shown.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(browser) = self.browser.as_ref().and_then(|(browser, _)| browser.upgrade())
            && self.shown
        {
            let target = browser.read(cx).preview_target();
            self.show(target, cx);
        }
    }

    fn show(&mut self, entry: Option<Entry>, cx: &mut Context<Self>) {
        let key = |e: &Entry| (e.path.clone(), e.modified, e.size);
        if self.target.as_ref().map(key) == entry.as_ref().map(key) {
            return;
        }
        self.target = entry;
        self.content = Content::None;
        self.details = Details::default();
        // What's still loading for the item before would land on this one.
        (self._load, self._details) = (Task::ready(()), Task::ready(()));
        self.scroll = UniformListScrollHandle::new();
        self.pictures.trim(None, None, cx);
        cx.notify();
        let Some(entry) = self.target.clone() else { return };
        // Its details come in on their own, as some take longer to read than what it holds.
        let read = cx.background_spawn({
            let entry = entry.clone();
            async move { objc2::rc::autoreleasepool(|_| details::read(&entry)) }
        });
        self._details = cx.spawn(async move |this, cx| {
            let details = read.await;
            this.update(cx, |this, cx| {
                this.details = details;
                cx.notify();
            })
            .ok();
        });
        // An alias shows what it leads to.
        let path = entry.target().to_path_buf();
        let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
        // Reading what's only in iCloud would download it, so Quick Look pictures it instead.
        let image =
            entry.category == Category::Image && extension.is_some_and(|ext| GPUI_DECODES.contains(&ext.as_str()));
        if image && !entry.cloud_only {
            self.content = Content::Image(path, RetainAllImageCache::new(cx));
            return;
        }
        let load = cx.background_spawn(async move {
            if entry.folder {
                let count = std::fs::read_dir(&path).map(|dir| dir.count());
                return count.map(Content::Folder).unwrap_or(Content::None);
            }
            if entry.package {
                return Content::Icon;
            }
            text(&entry).unwrap_or(Content::Picture)
        });
        self._load = cx.spawn(async move |this, cx| {
            let content = load.await;
            this.update(cx, |this, cx| {
                this.content = content;
                cx.notify();
            })
            .ok();
        });
    }
}

/// The start of a text file's text.
fn text(entry: &Entry) -> Option<Content> {
    if entry.cloud_only
        || !matches!(entry.category, Category::Code | Category::Text | Category::Other | Category::Executable)
    {
        return None;
    }
    let bytes = fs::read_text(entry.target(), TEXT_LIMIT).ok().filter(|bytes| !bytes.is_empty())?;
    let lines: Arc<[SharedString]> =
        String::from_utf8_lossy(&bytes).lines().map(|line| line.to_owned().into()).collect();
    let widest = (0..lines.len()).max_by_key(|&ix| lines[ix].chars().count()).unwrap_or(0);
    Some(Content::Text(lines, widest))
}

impl Render for Preview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picture = match (&self.content, &self.target) {
            (Content::Picture, Some(entry)) => self.pictures.get(entry, window, cx, |this| &mut this.pictures),
            _ => None,
        };
        let theme = cx.theme();
        let pane = v_flex().size_full().bg(theme.sidebar).text_sm();
        let Some(entry) = &self.target else {
            return pane.items_center().justify_center().text_color(theme.muted_foreground).child("No selection");
        };
        // Filling what's left, rather than as tall as its width would make it, so it fits above the
        // details when they take up most of the pane.
        let fit = |image: ImageSource| {
            v_flex().size_full().p_4().child(img(image).flex_1().min_h_0().w_full().object_fit(ObjectFit::Contain))
        };
        let icon = || {
            let icon = icons::entry(entry, ICON);
            div().size_full().flex().items_center().justify_center().child(icon).into_any_element()
        };
        let content = match &self.content {
            Content::Image(path, images) => {
                image_cache(images.clone()).size_full().child(fit(path.clone().into())).into_any_element()
            }
            Content::Text(lines, widest) => {
                let (lines, count) = (lines.clone(), lines.len());
                let list = uniform_list("preview-text", count, move |range, _, _| {
                    range.map(|ix| div().child(lines[ix].clone())).collect()
                })
                // Lines take the width they need, so the widest one decides how far it scrolls sideways.
                .with_width_from_item(Some(*widest))
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .size_full()
                .p_3()
                .track_scroll(&self.scroll);
                div()
                    .size_full()
                    .bg(theme.background)
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .whitespace_nowrap()
                    .child(list)
                    .into_any_element()
            }
            Content::Picture => match picture {
                Some(picture) => fit(picture.into()).into_any_element(),
                // What it has no picture of, or not yet, shows its icon.
                None => icon(),
            },
            Content::Folder(_) | Content::Icon => icon(),
            Content::None => div().size_full().into_any_element(),
        };
        // Values wrap, so a long one, like a lens's model, shows in full.
        let detail = |label: &'static str, value: String| {
            h_flex()
                .items_start()
                .gap_2()
                .child(div().w(px(80.)).flex_none().text_color(theme.muted_foreground).child(label))
                .child(div().min_w_0().child(value))
        };
        let size = match &self.content {
            Content::Folder(count) => Some(fs::count(*count, "item", "items")),
            _ if entry.folder || entry.package => None,
            _ => Some(fs::format_size(entry.size)),
        };
        pane.child(div().flex_1().min_h_0().child(content)).child(
            // Scrolling when there's more than fits under what it holds.
            v_flex()
                .id("details")
                .flex_none()
                .max_h(relative(0.6))
                .overflow_y_scroll()
                .gap_1()
                .p_3()
                .border_t_1()
                .border_color(theme.border)
                .child(div().pb_1().font_medium().child(entry.name.clone()))
                .child(detail("Kind", entry.kind.to_string()))
                .children(size.map(|size| detail("Size", size)))
                .children(self.details.about.iter().map(|(label, value)| detail(label, value.clone())))
                .children(self.details.created.map(|time| detail("Created", fs::format_date(time))))
                .children(entry.modified.map(|time| detail("Modified", fs::format_date(time))))
                .children(self.details.source.as_ref().map(|source| detail("Where from", source.clone())))
                .child(detail("Where", entry.path.parent().map(|p| p.display().to_string()).unwrap_or_default())),
        )
    }
}
