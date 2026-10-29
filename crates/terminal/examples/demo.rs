//! A window with one terminal, for trying the crate on its own, in Mu's dark theme:
//! `cargo run -p mu-terminal --example demo`. With `MU_DEMO_COMMAND` set, it types that command into
//! the shell once it starts. It quits when the shell exits.

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::*;
use mu_terminal::{AnsiColors, TerminalView};

const THEMES: &str = include_str!("../../../assets/theme.json");

fn main() {
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(|cx| {
        gpui_kit::init(cx);
        mu_terminal::init(cx);

        ThemeRegistry::global_mut(cx).load_themes_from_str(THEMES).expect("theme.json parses");
        cx.set_global(ansi_colors(THEMES));
        if let Some(dark) = ThemeRegistry::global(cx).themes().get("Zedokai Darker").cloned() {
            Theme::update(cx, |theme| theme.apply_config(&dark));
        }
        Theme::change(ThemeMode::Dark, None, cx);

        let cwd = std::env::var_os("HOME").map(Into::into).unwrap_or_else(|| "/".into());
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(860.), px(540.)), cx))),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            let terminal = cx.new(|cx| TerminalView::new(cwd, cx));
            window.focus(&terminal.focus_handle(cx), cx);
            if let Ok(command) = std::env::var("MU_DEMO_COMMAND") {
                terminal.update(cx, |terminal, cx| terminal.send_text(&format!("{command}\r"), cx));
            }
            cx.subscribe(&terminal, |_, event, cx| {
                if let mu_terminal::TerminalEvent::Exited = event {
                    cx.quit();
                }
            })
            .detach();
            terminal
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}

/// The 16 colors in each "ansi" list of Mu's themes, the dark theme's first. The demo has no JSON
/// parser, and these lists are all it needs that the theme registry doesn't read.
fn ansi_colors(themes: &str) -> AnsiColors {
    let mut lists = themes.split("\"ansi\"").skip(1).map(|rest| {
        let list = &rest[..rest.find(']').expect("the list ends")];
        let colors: Vec<Hsla> = list.split('"').filter_map(|text| Rgba::try_from(text).ok()).map(Into::into).collect();
        colors.try_into().expect("16 colors")
    });
    let mut next = || lists.next().expect("a dark and a light theme");
    AnsiColors { dark: next(), light: next() }
}
