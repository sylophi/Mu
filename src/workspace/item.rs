//! What a tab holds: a folder, a terminal, an editor, a search, an app to remove or one of Mu's own
//! pages.

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::*;
use mu_terminal::TerminalView;

use crate::{
    activity::Activity,
    browser::Browser,
    editor::FileEditor,
    history::page::HistoryPage,
    icons,
    mac::{ThisMac, Uninstall},
    search::Search,
    session::Tab,
    settings::page::SettingsPage,
};

/// Something in the center pane.
#[derive(Clone)]
pub(super) enum Item {
    Folder(Entity<Browser>),
    Terminal(Entity<TerminalView>),
    Editor(Entity<FileEditor>),
    Search(Entity<Search>),
    Activity(Entity<Activity>),
    Mac(Entity<ThisMac>),
    History(Entity<HistoryPage>),
    Settings(Entity<SettingsPage>),
    Uninstall(Entity<Uninstall>),
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
            Item::Mac($entity) => $body,
            Item::History($entity) => $body,
            Item::Settings($entity) => $body,
            Item::Uninstall($entity) => $body,
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
            Tab::Untitled(dir) => Item::Editor(cx.new(|cx| FileEditor::untitled(dir, window, cx))),
            Tab::Search(root) => Item::Search(cx.new(|cx| Search::new(root, window, cx))),
            Tab::Activity => Item::Activity(cx.new(|cx| Activity::new(window, cx))),
            Tab::ThisMac | Tab::Startup | Tab::Storage(_) => Item::Mac(cx.new(|cx| ThisMac::new(tab, cx))),
            Tab::History => Item::History(cx.new(HistoryPage::new)),
            Tab::Settings => Item::Settings(cx.new(SettingsPage::new)),
            Tab::Uninstall(app) => Item::Uninstall(cx.new(|cx| Uninstall::new(app, cx))),
        }
    }

    /// Whether it shows `tab` already, so showing that again shows this. Folders and terminals open
    /// anew each time, as do new files, and there's one of each of Mu's own pages.
    pub(super) fn shows(&self, tab: &Tab, cx: &App) -> bool {
        match (self, tab) {
            (Item::Editor(editor), Tab::Editor(path)) => editor.read(cx).path() == Some(path.as_path()),
            (Item::Search(search), Tab::Search(root)) => search.read(cx).root() == root,
            (Item::Uninstall(page), Tab::Uninstall(app)) => page.read(cx).path() == app,
            (Item::Activity(_), Tab::Activity)
            | (Item::Mac(_), Tab::ThisMac | Tab::Startup | Tab::Storage(_))
            | (Item::History(_), Tab::History)
            | (Item::Settings(_), Tab::Settings) => true,
            _ => false,
        }
    }

    /// Has it show `tab`, which it `shows`: This Mac goes to the section asked for.
    pub(super) fn show(&self, tab: Tab, cx: &mut App) {
        if let Item::Mac(page) = self {
            page.update(cx, |page, cx| page.show(tab, cx));
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
            Item::Mac(_) => "This Mac".into(),
            Item::History(_) => "Undo History".into(),
            Item::Settings(_) => "Settings".into(),
            Item::Uninstall(e) => e.read(cx).title(),
        }
    }

    /// What it shows, to open it again.
    pub(super) fn saved(&self, cx: &App) -> Tab {
        match self {
            Item::Folder(e) => Tab::Folder(e.read(cx).path().to_path_buf()),
            Item::Terminal(e) => Tab::Terminal(e.read(cx).cwd()),
            Item::Editor(e) => e.read(cx).saved(),
            Item::Search(e) => Tab::Search(e.read(cx).root().to_path_buf()),
            Item::Activity(_) => Tab::Activity,
            Item::Mac(e) => e.read(cx).saved(cx),
            Item::History(_) => Tab::History,
            Item::Settings(_) => Tab::Settings,
            Item::Uninstall(e) => Tab::Uninstall(e.read(cx).path().to_path_buf()),
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
            Item::Editor(e) => match e.read(cx).path() {
                Some(path) => icons::item(path, false, size).into_any_element(),
                None => lucide(IconName::SquarePen, cx),
            },
            Item::Search(_) => lucide(IconName::Search, cx),
            Item::Activity(_) => lucide(IconName::Activity, cx),
            Item::Mac(_) => lucide(IconName::Laptop, cx),
            Item::History(_) => lucide(IconName::RotateCcwClock, cx),
            Item::Settings(_) => lucide(IconName::Settings, cx),
            Item::Uninstall(e) => icons::item(e.read(cx).path(), true, size).into_any_element(),
        }
    }
}
