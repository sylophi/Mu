//! A window, laid out like Zed: the sidebar dock beside a pane of tabs (folders, terminals, editors,
//! searches, Activity and Storage), with the preview dock beside a folder in it.

pub mod actions;
mod docks;
mod go;
mod item;
mod pane;
mod parts;
mod path_bar;
mod sidebar;
mod status_bar;
mod tab_list;
mod tabs;
mod toolbar;
pub mod windows;

use std::{cell::Cell, collections::HashMap, path::PathBuf, rc::Rc};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Theme, TitleBar,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{InputEvent, InputState},
    v_flex,
};
use gpui_kit::*;

use crate::{
    activity::monitor::Monitor,
    browser::Browser,
    drag::DraggedFiles,
    network, operations,
    palette::Palette,
    permissions::{self, Permissions},
    places::Places,
    preview::Preview,
    session::Tab,
    settings::Settings,
    shelf,
    update::{RestartToUpdate, Updater},
};
use actions::ShowSettings;
use docks::Docks;
use item::Item;
use path_bar::PathEditor;
use sidebar::{DraggedFavorite, Sidebar};
use status_bar::Meters;
use tab_list::TabList;
use tabs::{DraggedTab, TabDrag};

pub fn init(cx: &mut App) {
    go::init(cx);
    actions::init(cx);
    windows::init(cx);
}

/// What a tab asks of the window it's in.
#[derive(Clone)]
pub enum TabEvent {
    /// A folder tab went to this folder.
    Navigated(PathBuf),
    /// What the window shows of it changed: its title, as a search's does with its query, or an
    /// editor's mark for unsaved changes and its cursor in the status bar.
    Changed,
    /// Opens a folder in a tab behind this one.
    OpenTab(PathBuf),
    /// Opens a folder in a window of its own.
    OpenWindow(PathBuf),
    OpenTerminal(PathBuf),
    Edit {
        path: PathBuf,
        line: Option<u32>,
    },
    /// Shows these items, all in one folder, selected.
    Reveal(Vec<PathBuf>),
    ShowFolder(PathBuf),
}

/// What its buttons and meters need, which toasts stay above.
pub const STATUS_BAR_HEIGHT: Pixels = px(29.);

pub struct Workspace {
    items: TabList<Item>,
    /// What each tab asks of the window, listened to while it's in this one.
    tab_subscriptions: HashMap<EntityId, Subscription>,
    tabs_scroll: ScrollHandle,
    /// Where the tab strip was last drawn, to tell when what's dragged is over it.
    strip_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Where among the tabs letting go of what's dragged over them would put it.
    tab_drop: Option<usize>,
    /// A tab dragged from here, and where it would go.
    tab_drag: Option<TabDrag>,
    /// The tabs closed here, the last one last, and where each was, for Reopen Closed Tab.
    closed: Vec<(usize, Tab)>,
    /// The folder tab shown last, which the sidebar and preview follow.
    last_folder: Option<Entity<Browser>>,
    docks: Docks,
    preview: Entity<Preview>,
    sidebar: Entity<Sidebar>,
    meters: Entity<Meters>,
    palette: Option<(Entity<Palette>, Subscription)>,
    /// The permissions sheet shown at launch, polling while it's up.
    onboarding: Option<permissions::Watch>,
    path_editor: Option<PathEditor>,
    /// The path bar's width as of the last frame, which decides how much of the path fits.
    path_bar_width: Pixels,
    filter: Entity<InputState>,
    /// Where in Favorites letting go of what's dragged over them would put it.
    favorite_drop: Option<usize>,
    /// Whether files are being dragged, which the shelf comes out for.
    dragging_files: bool,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let subscriptions = vec![
            cx.subscribe_in(&filter, window, |this, filter, event, window, cx| match event {
                InputEvent::Change => {
                    let value = filter.read(cx).value();
                    if let Some(browser) = this.active_folder() {
                        browser.update(cx, |browser, cx| browser.set_filter(value, cx));
                    }
                }
                InputEvent::PressEnter { .. } => this.focus_active(window, cx),
                _ => {}
            }),
            cx.observe_window_appearance(window, |_, window, cx| Theme::sync_system_appearance(Some(window), cx)),
            cx.observe_global::<Settings>(|_, cx| cx.notify()),
            cx.observe_global::<operations::Transfers>(|_, cx| cx.notify()),
            cx.observe_global::<shelf::Shelf>(|_, cx| cx.notify()),
            cx.observe_global::<network::Servers>(|_, cx| cx.notify()),
            cx.observe_global::<Places>(|_, cx| cx.notify()),
            cx.observe(&Updater::global(cx), |_, _, cx| cx.notify()),
            cx.observe(&Permissions::global(cx), |this, _, cx| {
                this.onboard(cx);
                cx.notify();
            }),
            // Back from System Settings, maybe with something switched on.
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    Permissions::global(cx).update(cx, |permissions, cx| permissions.refresh(cx));
                }
            }),
        ];
        let docks = Docks::new(Settings::get(cx));
        let mut this = Self {
            items: TabList::default(),
            tab_subscriptions: HashMap::new(),
            tabs_scroll: ScrollHandle::new(),
            strip_bounds: Rc::default(),
            tab_drop: None,
            tab_drag: None,
            closed: Vec::new(),
            last_folder: None,
            preview: cx.new(|cx| Preview::new(docks.preview, cx)),
            docks,
            sidebar: cx.new({
                let workspace = cx.entity();
                move |cx| Sidebar::new(&workspace, cx)
            }),
            meters: cx.new(Meters::new),
            palette: None,
            onboarding: None,
            path_editor: None,
            path_bar_width: px(0.),
            filter,
            favorite_drop: None,
            dragging_files: false,
            _subscriptions: subscriptions,
        };
        this.onboard(cx);
        let workspace = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            workspace
                .update(cx, |this, cx| {
                    let close = this.unsaved(cx).is_empty();
                    match close {
                        true => this.save_if_last(window, cx),
                        false => this.close_window(window, cx),
                    }
                    close
                })
                .unwrap_or(true)
        });
        this
    }

    /// Brings up the sheet that asks for the permissions, if it's this window's turn.
    fn onboard(&mut self, cx: &mut Context<Self>) {
        if let Some(watch) = Permissions::global(cx).update(cx, |permissions, _| permissions.onboard()) {
            self.onboarding = Some(watch);
        }
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let host = Monitor::global(cx).read(cx).host.clone();
        let permissions = Permissions::global(cx).read(cx);
        // Settings lists them itself.
        let on_settings = matches!(self.active_item(), Some(Item::Settings(_)));
        let missing = (permissions.checked() && permissions.missing() > 0 && !on_settings).then(|| {
            Button::new("permissions")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::ShieldAlert).text_color(theme.warning))
                .label("Permissions")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowSettings), cx))
        });
        let update = Updater::global(cx).read(cx).is_ready().then(|| {
            Button::new("update")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::Download).text_color(theme.success))
                .label("Restart to Update")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(RestartToUpdate), cx))
        });
        TitleBar::new().child(
            h_flex()
                .flex_1()
                .h_full()
                .gap_1()
                .text_sm()
                .child(
                    h_flex()
                        .px_1()
                        .gap_1()
                        .child(Icon::new(IconName::Server).xsmall().text_color(theme.accent_foreground))
                        .child(host),
                )
                .child(div().flex_1())
                .children(update)
                .children(missing)
                .child(div().w_2()),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Nothing says when a drag ends without a drop here, over another window or somewhere that
        // takes nothing, so what it showed is cleared once no drag is going on.
        if !cx.has_active_drag() {
            self.dragging_files = false;
            self.tab_drop = None;
            self.tab_drag = None;
        }
        let main = self.render_main(window, cx);
        let palette = self.palette.as_ref().map(|(palette, _)| {
            deferred(
                div().absolute().top(px(48.)).left_0().right_0().flex().justify_center().child(
                    div()
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
                        .child(palette.clone()),
                ),
            )
            .with_priority(1)
        });
        let onboarding = self.onboarding.is_some().then(|| {
            let close = cx.listener(|this, _, _, cx| {
                this.onboarding = None;
                cx.notify();
            });
            permissions::render_onboarding(close, cx)
        });
        self.register_actions(v_flex(), cx)
            .size_full()
            .relative()
            .key_context("Workspace")
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<DraggedTab>, window, cx| {
                let (id, grabbed_at) = (event.drag(cx).id, event.drag(cx).grabbed_at.get());
                this.tab_dragged(id, grabbed_at, window, cx);
            }))
            // Let go anywhere, even outside the window.
            .capture_any_mouse_up(cx.listener(|this, _: &MouseUpEvent, window, cx| this.tab_dropped(window, cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, window, cx| this.tab_dropped(window, cx)))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<DraggedFiles>, _, cx| {
                this.files_dragged(cx);
                let drop = this.folder_drop(event.event.position, &event.drag(cx).paths);
                this.set_tab_drop(drop, cx);
            }))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                this.files_dragged(cx);
                let drop = this.folder_drop(event.event.position, event.drag(cx).paths());
                this.set_tab_drop(drop, cx);
            }))
            // A favorite let go of anywhere but the sidebar comes out of it.
            .on_drop(|favorite: &DraggedFavorite, _, cx| {
                Settings::update(cx, |settings| settings.favorites.retain(|path| path != &favorite.path))
            })
            .child(self.render_title_bar(cx))
            .child(div().flex_1().min_h_0().child(main))
            .child(self.render_status_bar(cx))
            .children(palette)
            .children(onboarding)
    }
}
