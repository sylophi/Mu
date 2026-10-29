//! What a tab holds: a folder, a terminal, an editor, a search or one of Mu's own pages.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::*;
use mu_terminal::TerminalView;

use crate::{
    activity::Activity, browser::Browser, editor::FileEditor, history::page::HistoryPage, icons, search::Search,
    session::Tab, settings::page::SettingsPage, storage::Storage,
};

/// Something in the center pane.
#[derive(Clone)]
pub(super) enum Item {
    Folder(Entity<Browser>),
    Terminal(Entity<TerminalView>),
    Editor(Entity<FileEditor>),
    Search(Entity<Search>),
    Activity(Entity<Activity>),
    Storage(Entity<Storage>),
    History(Entity<HistoryPage>),
    Settings(Entity<SettingsPage>),
}

/// Does the same with whichever entity `item` holds, as `entity`, whatever kind of tab it is.
macro_rules! each {
    ($item:expr, $entity:ident => $body:expr) => {
        match $item {
            Item::Folder($entity) => $body,
            Item::Terminal($entity) => $body,
            Item::Editor($entity) => $body,
            Item::Search($entity) => $body,
            Item::Activity($entity) => $body,
            Item::Storage($entity) => $body,
            Item::History($entity) => $body,
            Item::Settings($entity) => $body,
        }
    };
}

impl Item {
    /// A new tab for `tab`.
    pub(super) fn new(tab: Tab, window: &mut Window, cx: &mut App) -> Self {
        match tab {
            Tab::Folder(path) => Item::Folder(cx.new(|cx| Browser::new(path, window, cx))),
            Tab::Terminal(dir) => Item::Terminal(cx.new(|cx| TerminalView::new(dir, cx))),
            Tab::Editor(path) => Item::Editor(cx.new(|cx| FileEditor::new(path, window, cx))),
            Tab::Search(root) => Item::Search(cx.new(|cx| Search::new(root, window, cx))),
            Tab::Activity => Item::Activity(cx.new(|cx| Activity::new(window, cx))),
            Tab::Storage(dir) => Item::Storage(cx.new(|cx| Storage::new(dir, cx))),
            Tab::History => Item::History(cx.new(HistoryPage::new)),
            Tab::Settings => Item::Settings(cx.new(SettingsPage::new)),
        }
    }

    /// Whether it shows `tab` already, so showing that again shows this. Folders and terminals open
    /// anew each time, and there's one of each of Mu's own pages.
    pub(super) fn shows(&self, tab: &Tab, cx: &App) -> bool {
        match (self, tab) {
            (Item::Editor(editor), Tab::Editor(path)) => editor.read(cx).path() == path,
            (Item::Search(search), Tab::Search(root)) => search.read(cx).root() == root,
            (Item::Activity(_), Tab::Activity)
            | (Item::Storage(_), Tab::Storage(_))
            | (Item::History(_), Tab::History)
            | (Item::Settings(_), Tab::Settings) => true,
            _ => false,
        }
    }

    pub(super) fn id(&self) -> EntityId {
        each!(self, entity => entity.entity_id())
    }

    pub(super) fn view(&self) -> AnyView {
        each!(self, entity => entity.clone().into())
    }

    pub(super) fn focus_handle(&self, cx: &App) -> FocusHandle {
        each!(self, entity => entity.focus_handle(cx))
    }

    pub(super) fn title(&self, cx: &App) -> SharedString {
        match self {
            Item::Folder(e) => e.read(cx).title(),
            Item::Terminal(e) => e.read(cx).title(),
            Item::Editor(e) => e.read(cx).title(),
            Item::Search(e) => e.read(cx).title(cx),
            Item::Activity(_) => "Activity".into(),
            Item::Storage(_) => "Storage".into(),
            Item::History(_) => "Undo History".into(),
            Item::Settings(_) => "Settings".into(),
        }
    }

    /// What it shows, to open it again.
    pub(super) fn saved(&self, cx: &App) -> Tab {
        match self {
            Item::Folder(e) => Tab::Folder(e.read(cx).path().to_path_buf()),
            Item::Terminal(e) => Tab::Terminal(e.read(cx).cwd()),
            Item::Editor(e) => Tab::Editor(e.read(cx).path().to_path_buf()),
            Item::Search(e) => Tab::Search(e.read(cx).root().to_path_buf()),
            Item::Activity(_) => Tab::Activity,
            Item::Storage(e) => Tab::Storage(e.read(cx).dir()),
            Item::History(_) => Tab::History,
            Item::Settings(_) => Tab::Settings,
        }
    }

    pub(super) fn icon(&self, cx: &App) -> AnyElement {
        let size = px(14.);
        let lucide = |icon: IconName, cx: &App| {
            Icon::new(icon).size(size).flex_none().text_color(cx.theme().muted_foreground).into_any_element()
        };
        match self {
            Item::Folder(e) => icons::item(e.read(cx).path(), true, size).into_any_element(),
            Item::Terminal(_) => lucide(IconName::SquareTerminal, cx),
            Item::Editor(e) => icons::item(e.read(cx).path(), false, size).into_any_element(),
            Item::Search(_) => lucide(IconName::Search, cx),
            Item::Activity(_) => lucide(IconName::Activity, cx),
            Item::Storage(_) => lucide(IconName::HardDrive, cx),
            Item::History(_) => lucide(IconName::RotateCcwClock, cx),
            Item::Settings(_) => lucide(IconName::Settings, cx),
        }
    }
}
