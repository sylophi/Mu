//! The This Mac tab, where you look after the Mac itself, a section at a time.

mod devices;
mod overview;
pub mod storage;
mod updates;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    tab::{self, TabBar},
    v_flex,
};
use gpui_kit::*;

use crate::{browser::Refresh, fs::home_dir, session::Tab, workspace::TabEvent};
use overview::Overview;
use storage::Storage;

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-r", Refresh, Some("ThisMac"))]);
    storage::init(cx);
}

/// In the order their tabs are.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    Overview,
    Storage,
}

impl Section {
    const ALL: [Section; 2] = [Section::Overview, Section::Storage];

    fn name(self) -> &'static str {
        match self {
            Section::Overview => "Overview",
            Section::Storage => "Storage",
        }
    }
}

pub struct ThisMac {
    section: Section,
    /// Each section's page, made once it's first shown, as each starts reading what it shows.
    overview: Option<Entity<Overview>>,
    storage: Option<Entity<Storage>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TabEvent> for ThisMac {}

impl ThisMac {
    pub fn new(tab: Tab, cx: &mut Context<Self>) -> Self {
        let mut this = Self { section: Section::Overview, overview: None, storage: None, _subscriptions: Vec::new() };
        this.show(tab, cx);
        this
    }

    /// Shows the section `tab` is for.
    pub fn show(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.section = match tab {
            Tab::Storage(dir) => {
                match &self.storage {
                    Some(storage) => storage.update(cx, |storage, cx| storage.show(dir, cx)),
                    None => self.storage = Some(self.page(|cx| Storage::new(dir, cx), cx)),
                }
                Section::Storage
            }
            _ => {
                if self.overview.is_none() {
                    self.overview = Some(cx.new(Overview::new));
                }
                Section::Overview
            }
        };
        cx.notify();
    }

    /// A section's page, whose asks of the window, like showing a folder in Files, go to it.
    fn page<T: EventEmitter<TabEvent>>(
        &mut self,
        new: impl FnOnce(&mut Context<T>) -> T,
        cx: &mut Context<Self>,
    ) -> Entity<T> {
        let page = cx.new(new);
        self._subscriptions.push(cx.subscribe(&page, |_, _, event: &TabEvent, cx| cx.emit(event.clone())));
        page
    }

    /// What it shows, to open it again.
    pub fn saved(&self, cx: &App) -> Tab {
        self.tab(self.section, cx)
    }

    /// The tab for `section` as it was last shown.
    fn tab(&self, section: Section, cx: &App) -> Tab {
        match section {
            Section::Overview => Tab::ThisMac,
            Section::Storage => Tab::Storage(self.storage.as_ref().map_or_else(home_dir, |s| s.read(cx).dir())),
        }
    }

    /// The page shown, and what takes focus in it.
    fn shown(&self, cx: &App) -> (AnyView, FocusHandle) {
        const MADE: &str = "a section's page is made when it's shown";
        match self.section {
            Section::Overview => {
                let page = self.overview.as_ref().expect(MADE);
                (page.clone().into(), page.focus_handle(cx))
            }
            Section::Storage => {
                let page = self.storage.as_ref().expect(MADE);
                (page.clone().into(), page.focus_handle(cx))
            }
        }
    }

    fn render_sections(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex_none().px_4().pt_2().border_b_1().border_color(cx.theme().border).child(
            TabBar::new("this-mac-sections")
                .underline()
                .small()
                .selected_index(self.section as usize)
                .on_click(cx.listener(|this, &ix: &usize, window, cx| {
                    let tab = this.tab(Section::ALL[ix], cx);
                    this.show(tab, cx);
                    this.focus_handle(cx).focus(window, cx);
                }))
                .children(Section::ALL.map(|section| tab::Tab::new().label(section.name()))),
        )
    }
}

impl Focusable for ThisMac {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.shown(cx).1
    }
}

impl Render for ThisMac {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ThisMac")
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_sections(cx))
            .child(div().flex_1().min_h_0().child(self.shown(cx).0))
    }
}
