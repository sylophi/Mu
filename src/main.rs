//! Mu, a file manager for macOS. Started as the `mu` command it opens tabs in the app, and started
//! as root it does what needs an administrator. Otherwise it's the app.

mod activity;
mod assets;
mod browser;
mod drag;
mod editor;
mod finder;
mod fs;
mod history;
mod icons;
mod link;
mod network;
mod operations;
mod palette;
mod pasteboard;
mod permissions;
mod places;
mod preview;
mod quicklook;
mod search;
mod session;
mod settings;
mod shelf;
mod storage;
mod theme;
mod thumbnails;
mod update;
mod workspace;

use std::process::ExitCode;

use crate::settings::Settings;

fn main() -> ExitCode {
    if std::env::args_os().nth(1).is_some_and(|argument| argument == fs::admin::ARGUMENT) {
        return fs::admin::serve();
    }
    if let Some(app) = link::command() {
        return link::run(&app);
    }
    // Tabs other apps ask for with a link, or the `mu` command does. What's asked at launch can come
    // before Mu gets going.
    let (asked, opened) = smol::channel::unbounded();
    let app = gpui_kit::application().with_assets(assets::Assets);
    app.on_open_urls(move |urls| {
        let tabs: Vec<_> = urls.iter().filter_map(|url| link::tab(url)).collect();
        if !tabs.is_empty() {
            _ = asked.try_send(tabs);
        }
    });
    app.run(move |cx| {
        gpui_kit::init(cx);
        Settings::load(cx);
        theme::init(cx);
        history::init(cx);
        operations::init(cx);
        shelf::init(cx);
        storage::init(cx);
        activity::monitor::init(cx);
        network::init(cx);
        permissions::init(cx);
        places::init(cx);
        mu_terminal::init(cx);
        browser::init(cx);
        editor::init(cx);
        palette::init(cx);
        workspace::init(cx);
        update::init(cx);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        // As it was when Mu last closed, and tabs asked for with links open in the window in front.
        workspace::windows::restore(cx);
        cx.spawn(async move |cx| {
            while let Ok(tabs) = opened.recv().await {
                cx.update(|cx| workspace::windows::open_asked(tabs, cx));
            }
        })
        .detach();
        cx.activate(true);
        // The menu bar comes after the window: setting it the first time has macOS load its text
        // input items for the Edit menu, which can take a while.
        cx.spawn(async |cx| cx.update(|cx| cx.set_menus(workspace::actions::menus(cx)))).detach();
    });
    ExitCode::SUCCESS
}
