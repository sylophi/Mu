//! Removing an app with everything it left around the Mac, in a tab of its own that Uninstall…
//! on an app opens. It all goes to the Trash, so Undo puts it back.

mod app;
mod leftovers;
mod receipts;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    menu::{ContextMenuExt as _, PopupMenuItem},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::{group, list, loading, note, startup::launchd};
use crate::{
    activity::monitor,
    browser::{Redo, Refresh, Undo},
    drag::DropFiles as _,
    fs::{self, format_size, shown_path},
    history::{self, History},
    icons, operations,
    workspace::TabEvent,
};
use app::App;
use leftovers::{Leftover, Roots};

pub fn init(cx: &mut gpui_kit::App) {
    let context = Some("Uninstall");
    cx.bind_keys([
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
        KeyBinding::new("cmd-r", Refresh, context),
    ]);
}

pub struct Uninstall {
    path: PathBuf,
    /// None once it's gone.
    app: Option<App>,
    /// Till they've been found.
    leftovers: Option<Vec<Leftover>>,
    removing: bool,
    /// Whether it's not macOS's own, or Mu.
    removable: bool,
    /// How many of the history's undos and redos it has looked at.
    toggles_seen: usize,
    focus_handle: FocusHandle,
    _find: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TabEvent> for Uninstall {}

impl Uninstall {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            path,
            app: None,
            leftovers: None,
            removing: false,
            removable: false,
            toggles_seen: history::toggled(cx).len(),
            focus_handle: cx.focus_handle(),
            _find: Task::ready(()),
            // Undo can bring back what went, or take it away again.
            _subscriptions: vec![cx.observe_global::<History>(|this, cx| {
                let toggled = history::toggled(cx).len();
                if toggled != this.toggles_seen {
                    this.toggles_seen = toggled;
                    this.find(cx);
                }
            })],
        };
        this.find(cx);
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn title(&self) -> SharedString {
        let name = self.app.as_ref().map_or_else(|| fs::display_name(&self.path), |app| app.name.clone());
        format!("Uninstall {name}").into()
    }

    /// Reads the app again, and looks for what it left, keeping what was ticked or unticked.
    fn find(&mut self, cx: &mut Context<Self>) {
        self.app = App::new(&self.path);
        self.removable = self.app.as_ref().is_some_and(|app| !app.is_system() && !app.is_mu());
        let ticks: HashMap<PathBuf, bool> =
            self.leftovers.take().into_iter().flatten().map(|item| (item.path, item.checked)).collect();
        let Some(app) = self.app.clone() else { return cx.notify() };
        self._find = cx.spawn(async move |this, cx| {
            let mut found =
                cx.background_spawn(async move { leftovers::find(&app, &app::installed(), &Roots::mac()) }).await;
            for item in &mut found {
                item.checked = ticks.get(&item.path).copied().unwrap_or(item.checked);
            }
            this.update(cx, |this, cx| {
                this.leftovers = Some(found);
                cx.notify();
            })
            .ok();
        });
        cx.emit(TabEvent::Changed);
        cx.notify();
    }

    /// Takes in what's dropped: the first app among it, in place of this one.
    fn drop(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if let Some(path) = paths.iter().find(|path| path.extension().is_some_and(|ext| ext == "app")) {
            self.path = path.clone();
            self.leftovers = None;
            self.find(cx);
        }
    }

    fn check(&mut self, ix: usize, checked: bool, cx: &mut Context<Self>) {
        if let Some(item) = self.leftovers.as_mut().and_then(|items| items.get_mut(ix)) {
            item.checked = checked;
            cx.notify();
        }
    }

    /// Moves what's ticked to the Trash, stopping the jobs launchd runs for it, which would carry on
    /// without their files, and when the app goes too, quitting it first if it's open. The tab
    /// closes once the app's gone.
    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(app) = self.app.clone() else { return };
        let paths: Vec<PathBuf> =
            self.leftovers.iter().flatten().filter(|item| item.checked).map(|item| item.path.clone()).collect();
        let app_goes = paths.iter().any(|path| app.path.starts_with(path));
        let answer = (app_goes && !fs::running_in(&app.path).is_empty()).then(|| {
            let question = format!("“{}” is open. Quit it and move it to the Trash?", app.name);
            window.prompt(PromptLevel::Warning, &question, None, &["Quit and Move to Trash", "Cancel"], cx)
        });
        self.removing = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let go = match answer {
                Some(answer) => answer.await == Ok(0),
                None => true,
            };
            let mut done = None;
            if go {
                let mut plists = paths.clone();
                if app_goes {
                    // Those it registers itself, from inside it.
                    for dir in ["LaunchAgents", "LaunchDaemons"] {
                        let dir = std::fs::read_dir(app.path.join("Contents/Library").join(dir));
                        plists.extend(dir.into_iter().flatten().flatten().map(|entry| entry.path()));
                    }
                }
                // Its jobs go first, so launchd doesn't start what's quit again.
                let first = move || {
                    plists.iter().try_for_each(|plist| launchd::stop(plist))?;
                    if app_goes { quit(&app) } else { Ok(()) }
                };
                if let Ok(task) = cx.update(|window, cx| operations::trash_after(paths, first, window, cx)) {
                    done = task.await;
                }
            }
            this.update_in(cx, |this, window, cx| {
                this.removing = false;
                // The button that had it is gone, and ⌘Z undoes this.
                this.focus_handle.focus(window, cx);
                match done {
                    Some(_) if !this.path.exists() => cx.emit(TabEvent::Close),
                    Some(_) => this.find(cx),
                    None => cx.notify(),
                }
            })
            .ok();
        })
        .detach();
    }

    fn render_app(&self, app: &App, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let checked: Vec<&Leftover> = self.leftovers.iter().flatten().filter(|item| item.checked).collect();
        let size = checked.iter().map(|item| item.size).sum::<u64>();
        let remove = Button::new("remove")
            .danger()
            .label("Move to Trash")
            .loading(self.removing)
            .disabled(checked.is_empty() || self.removing || !self.removable)
            .on_click(cx.listener(|this, _, window, cx| this.remove(window, cx)));
        let header = h_flex()
            .gap_4()
            .child(div().flex_none().child(icons::item(&app.path, true, px(64.))))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_baseline()
                            .child(div().text_lg().font_semibold().truncate().child(app.name.clone()))
                            .children(app.version.clone().map(|version| {
                                div().flex_none().text_sm().text_color(theme.muted_foreground).child(version)
                            })),
                    )
                    .children(app.path.parent().map(|dir| {
                        div().text_sm().truncate().text_color(theme.muted_foreground).child(shown_path(dir))
                    })),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_3()
                    .when(self.leftovers.is_some(), |el| {
                        el.child(div().text_sm().text_color(theme.muted_foreground).child(format_size(size)))
                    })
                    .child(remove),
            );
        let groups = match &self.leftovers {
            None => vec![loading().into_any_element()],
            Some(items) => {
                let mut start = 0;
                items
                    .chunk_by(|a, b| a.place == b.place)
                    .map(|chunk| {
                        let rows = chunk.iter().zip(start..).map(|(item, ix)| self.render_leftover(ix, item, cx));
                        start += chunk.len();
                        group(chunk[0].place.name(), None, list(rows, cx), cx).into_any_element()
                    })
                    .collect()
            }
        };
        v_flex().gap_6().child(header).children(groups)
    }

    fn render_leftover(&self, ix: usize, item: &Leftover, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let is_dir = item.path.is_dir();
        let reveal = item.path.clone();
        let this = cx.weak_entity();
        h_flex()
            .id(("leftover", ix))
            .gap_3()
            .child(
                Checkbox::new(("check", ix))
                    .checked(item.checked)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| this.check(ix, *checked, cx))),
            )
            .child(div().flex_none().child(icons::item(&item.path, is_dir, px(20.))))
            .child(v_flex().flex_1().min_w_0().child(div().truncate().child(fs::display_name(&item.path))).children(
                item.path.parent().map(|dir| {
                    div()
                        .text_xs()
                        .truncate()
                        .text_ellipsis_middle()
                        .text_color(theme.muted_foreground)
                        .child(shown_path(dir))
                }),
            ))
            .child(div().flex_none().text_color(theme.muted_foreground).child(format_size(item.size)))
            .context_menu(move |menu, _, _| {
                let (this, reveal) = (this.clone(), reveal.clone());
                menu.item(PopupMenuItem::new("Show in Files").on_click(move |_, _, cx| {
                    this.update(cx, |_, cx| cx.emit(TabEvent::Reveal(vec![reveal.clone()]))).ok();
                }))
            })
    }
}

/// Quits what runs from `app`: the app itself first, so it can ask to save what's open while its
/// helpers still run, then what's left. Each gets ten seconds.
fn quit(app: &App) -> anyhow::Result<()> {
    let programs = app.path.canonicalize().unwrap_or_else(|_| app.path.clone()).join("Contents/MacOS");
    for main in [true, false] {
        let running = || {
            let holders = fs::running_in(&app.path).into_iter();
            holders.filter(|holder| !main || holder.exe.as_ref().is_some_and(|exe| exe.starts_with(&programs)))
        };
        monitor::quit(&running().collect::<Vec<_>>(), libc::SIGTERM)?;
        let mut tries = 0;
        while running().next().is_some() {
            tries += 1;
            if tries > 40 {
                anyhow::bail!("“{}” didn't quit", app.name);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(())
}

impl Focusable for Uninstall {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Uninstall {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.app {
            Some(app) => self.render_app(app, cx).into_any_element(),
            None => note(format!("“{}” isn't there anymore", fs::display_name(&self.path)), cx).into_any_element(),
        };
        div()
            .id("uninstall")
            .size_full()
            .overflow_y_scroll()
            .key_context("Uninstall")
            .track_focus(&self.focus_handle)
            .on_action(|_: &Undo, window, cx| history::step(false, window, cx).detach())
            .on_action(|_: &Redo, window, cx| history::step(true, window, cx).detach())
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.find(cx)))
            .drop_target()
            .on_drop_files({
                let this = cx.weak_entity();
                move |paths, _, cx| {
                    this.update(cx, |this, cx| this.drop(paths, cx)).ok();
                }
            })
            .bg(cx.theme().background)
            .child(v_flex().max_w(px(720.)).mx_auto().p_6().child(content))
    }
}
