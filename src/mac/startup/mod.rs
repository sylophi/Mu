//! This Mac's Startup Items: what opens at login or runs in the background, each with the app it
//! belongs to. Those launchd starts from its folders can be switched off here. Those apps register
//! for themselves are switched in System Settings.

pub(super) mod launchd;
mod login_items;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, PopupMenuItem},
    switch::Switch,
    v_flex,
};
use gpui_kit::*;

use super::{group, list, loading, note};
use crate::{browser::Refresh, icons, theme::Toasts as _, workspace::TabEvent};
use launchd::{Job, Scope};
use login_items::LoginItem;

pub struct Startup {
    /// Till they've been read.
    login_items: Option<Vec<LoginItem>>,
    jobs: Option<Vec<Job>>,
    /// The jobs being switched, by plist, which can't be switched again till that's done.
    switching: HashSet<PathBuf>,
    focus_handle: FocusHandle,
    _load: Task<()>,
}

impl EventEmitter<TabEvent> for Startup {}

impl Startup {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            login_items: None,
            jobs: None,
            switching: HashSet::new(),
            focus_handle: cx.focus_handle(),
            _load: Task::ready(()),
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let read = cx.background_spawn(async { (login_items::login_items(), launchd::jobs()) });
        self._load = cx.spawn(async move |this, cx| {
            let (login_items, jobs) = read.await;
            this.update(cx, |this, cx| {
                (this.login_items, this.jobs) = (Some(login_items), Some(jobs));
                cx.notify();
            })
            .ok();
        });
    }

    /// Switches the job in `plist` on or off, showing it so till launchctl says how it went.
    fn switch(&mut self, plist: PathBuf, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job) = self.jobs.iter_mut().flatten().find(|job| job.plist == plist) else { return };
        job.on = on;
        let job = job.clone();
        self.switching.insert(plist.clone());
        let switched = cx.background_spawn(async move { launchd::switch(&job, on) });
        cx.spawn_in(window, async move |this, cx| {
            let switched = switched.await;
            this.update_in(cx, |this, window, cx| {
                this.switching.remove(&plist);
                if let Err(error) = switched {
                    window.toast_error(&error, cx);
                }
                this.load(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn render_login_items(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let content = match &self.login_items {
            None => loading(),
            Some(items) if items.is_empty() => note("None", cx),
            Some(items) => list(
                items.iter().enumerate().map(|(ix, item)| {
                    // A helper says which app it's in.
                    let app_name = item.app.as_deref().and_then(Path::file_stem).map(|n| n.to_string_lossy());
                    let detail = app_name.filter(|app| *app != item.name).map(|app| app.into_owned());
                    let on =
                        div().flex_none().text_color(theme.muted_foreground).child(if item.on { "On" } else { "Off" });
                    row(("login-item", ix), item.app.as_deref(), item.name.clone(), detail, item.app.clone(), on, cx)
                }),
                cx,
            ),
        };
        let open = Button::new("login-items-settings")
            .ghost()
            .small()
            .label("Login Items Settings")
            .on_click(|_, _, cx| cx.open_url(login_items::SETTINGS));
        group("Login Items", Some(open), content, cx)
    }

    fn render_jobs(&self, scope: Scope, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let jobs: Vec<_> = self.jobs.as_ref()?.iter().enumerate().filter(|(_, job)| job.scope == scope).collect();
        if jobs.is_empty() {
            return None;
        }
        let heading = match scope {
            Scope::Yours => "Your Agents",
            Scope::Everyone => "Agents for Every User",
            Scope::System => "Daemons",
        };
        let rows = jobs.into_iter().map(|(ix, job)| {
            let plist = job.plist.clone();
            let switch = Switch::new(("switch", ix))
                .checked(job.on)
                .disabled(self.switching.contains(&job.plist))
                .on_click(cx.listener(move |this, on: &bool, window, cx| this.switch(plist.clone(), *on, window, cx)));
            // Without a program, it goes by its label.
            let detail = (job.name != job.label).then(|| job.label.clone());
            row(("job", ix), job.app.as_deref(), job.name.clone(), detail, Some(job.plist.clone()), switch, cx)
        });
        Some(group(heading, None, list(rows, cx), cx))
    }
}

/// A row for something that starts: its app's icon, or a program's, its name with `detail` under
/// it, `end` at its end, and Show in Files for `file`.
fn row(
    id: impl Into<ElementId>,
    app: Option<&Path>,
    name: String,
    detail: Option<String>,
    file: Option<PathBuf>,
    end: impl IntoElement,
    cx: &Context<Startup>,
) -> impl IntoElement {
    let theme = cx.theme();
    let icon = match app {
        Some(app) => icons::item(app, true, px(20.)).into_any_element(),
        None => Icon::new(IconName::SquareTerminal).size(px(20.)).text_color(theme.muted_foreground).into_any_element(),
    };
    let row =
        h_flex()
            .id(id)
            .gap_3()
            .child(div().flex_none().child(icon))
            .child(v_flex().flex_1().min_w_0().child(div().truncate().child(name)).children(
                detail.map(|detail| div().text_xs().truncate().text_color(theme.muted_foreground).child(detail)),
            ))
            .child(end);
    let Some(file) = file else { return row.into_any_element() };
    let this = cx.weak_entity();
    row.context_menu(move |menu, _, _| {
        let (this, file) = (this.clone(), file.clone());
        menu.item(PopupMenuItem::new("Show in Files").on_click(move |_, _, cx| {
            this.update(cx, |_, cx| cx.emit(TabEvent::Reveal(vec![file.clone()]))).ok();
        }))
    })
    .into_any_element()
}

impl Focusable for Startup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Startup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("startup")
            .size_full()
            .overflow_y_scroll()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.load(cx)))
            .bg(theme.background)
            .child(
                v_flex()
                    .max_w(px(720.))
                    .mx_auto()
                    .p_6()
                    .gap_6()
                    .child(self.render_login_items(cx))
                    .children(Scope::ALL.into_iter().filter_map(|scope| self.render_jobs(scope, cx))),
            )
    }
}
