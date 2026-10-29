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

/// The 16 terminal colors of Mu's dark and light themes, which the theme registry doesn't read.
fn ansi_colors(json: &str) -> AnsiColors {
    let file: serde_json::Value = serde_json::from_str(json).expect("theme.json parses");
    let themes = file["themes"].as_array().expect("a list of themes");
    let ansi = |mode: &str| {
        let theme = themes.iter().find(|theme| theme["mode"] == mode).expect("the theme exists");
        let colors = theme["terminal"]["ansi"].as_array().expect("a list of colors").iter();
        let colors: Vec<Hsla> =
            colors.map(|hex| Rgba::try_from(hex.as_str().expect("a color")).expect("a valid color").into()).collect();
        colors.try_into().expect("16 colors")
    };
    AnsiColors { dark: ansi("dark"), light: ansi("light") }
}
