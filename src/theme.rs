//! The look: the bundled Zedokai themes with their greys tinted as set, their terminal colors, and the
//! fonts.

use std::{rc::Rc, sync::LazyLock};

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeConfig, WindowExt as _, notification::Notification};
use gpui_kit::*;
use mu_terminal::AnsiColors;
use serde::Deserialize;

use crate::{fs::Cancelled, settings::Settings};

const THEMES: &str = include_str!("../assets/theme.json");

const FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
];

/// Prompts like powerlevel10k draw icons from a Nerd Font, which the terminal falls back to.
const SYMBOLS: &[u8] = include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

#[derive(Deserialize)]
struct ThemeFile {
    themes: Vec<ThemeEntry>,
}

#[derive(Deserialize)]
struct ThemeEntry {
    #[serde(flatten)]
    config: ThemeConfig,
    terminal: TerminalColors,
}

#[derive(Deserialize)]
struct TerminalColors {
    ansi: [String; 16],
}

pub fn init(cx: &mut App) {
    cx.text_system().add_fonts(FONTS.iter().map(|font| (*font).into()).collect()).expect("the bundled fonts load");
    register_with_core_text(SYMBOLS);

    // Toasts go in the bottom right corner, above the status bar, no wider than a few words need. A
    // new one takes the place of the last rather than stacking on it, since a stack peeks out from
    // behind as half a toast.
    let toasts = &mut Theme::global_mut(cx).notification;
    toasts.placement = Anchor::BottomRight;
    toasts.margins.bottom += crate::workspace::STATUS_BAR_HEIGHT;
    toasts.width = px(300.);
    toasts.max_items = 1;

    apply(cx);
}

/// Puts the bundled themes in place with their greys tinted the way the settings say, and shows
/// them.
pub fn apply(cx: &mut App) {
    let settings = Settings::get(cx);
    let tint = if settings.tint { Tint::Turned(settings.tint_turn) } else { Tint::Neutral };
    let mut file: serde_json::Value = serde_json::from_str(THEMES).expect("the bundled theme is valid");
    recolor(&mut file, tint);
    let file: ThemeFile = serde_json::from_value(file).expect("the bundled theme is valid");
    let global = Theme::global_mut(cx);
    let mut ansi = AnsiColors { dark: Default::default(), light: Default::default() };
    for ThemeEntry { config, terminal } in file.themes {
        let colors = terminal.ansi.each_ref().map(|hex| Rgba::try_from(hex.as_str()).expect("a valid color").into());
        if config.mode.is_dark() {
            (global.dark_theme, ansi.dark) = (Rc::new(config), colors);
        } else {
            (global.light_theme, ansi.light) = (Rc::new(config), colors);
        }
    }
    cx.set_global(ansi);
    Theme::sync_system_appearance(None, cx);
}

/// A card, like Activity's graphs and Storage's disks: a rounded box a shade off the background.
pub fn card(theme: &Theme) -> Div {
    div().rounded(theme.radius_lg).border_1().border_color(theme.border).bg(theme.sidebar)
}

/// The hue the background leans toward with the tint on, in OKLCH degrees.
pub fn background_hue(cx: &App) -> f32 {
    (monokai_hue(cx.theme().is_dark()) + Settings::get(cx).tint_turn).rem_euclid(360.)
}

/// Turns the tint so the background leans toward `hue`, in OKLCH degrees, and shows it.
pub fn turn_tint(hue: f32, save: bool, cx: &mut App) {
    let turn = (hue - monokai_hue(cx.theme().is_dark())).rem_euclid(360.);
    if save {
        Settings::update(cx, |settings| settings.tint_turn = turn);
    } else {
        cx.update_global::<Settings, _>(|settings, _| settings.tint_turn = turn);
    }
    apply(cx);
}

/// The hue Monokai's background leans toward in its dark theme, or its light one.
fn monokai_hue(dark: bool) -> f32 {
    static HUES: LazyLock<[f32; 2]> = LazyLock::new(|| {
        let file: serde_json::Value = serde_json::from_str(THEMES).expect("the bundled theme is valid");
        let hue = |mode: &str| {
            let themes = file["themes"].as_array().expect("the bundled theme is valid");
            let theme = themes.iter().find(|theme| theme["mode"] == mode).expect("the bundled theme exists");
            let hex = theme["colors"]["background"].as_str().and_then(|color| color.strip_prefix('#'));
            let [_, a, b] = oklab(hex.and_then(|hex| u32::from_str_radix(hex, 16).ok()).expect("a valid color"));
            b.atan2(a).to_degrees().rem_euclid(360.) as f32
        };
        [hue("dark"), hue("light")]
    });
    HUES[usize::from(!dark)]
}

/// How the theme's greys lean.
#[derive(Clone, Copy)]
enum Tint {
    Neutral,
    /// The way Monokai's do, turned this many degrees around the hue circle.
    Turned(f32),
}

/// Colors less colorful than this, in OKLCH chroma, are the greys. Monokai's are at most 0.025, and
/// its other colors at least 0.095.
const GREY: f64 = 0.03;

/// Tints the greys in theme `value` the way `tint` says.
fn recolor(value: &mut serde_json::Value, tint: Tint) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(color) = tinted(text, tint) {
                *text = color;
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|item| recolor(item, tint)),
        serde_json::Value::Object(fields) => fields.values_mut().for_each(|field| recolor(field, tint)),
        _ => {}
    }
}

/// `color`, written like #rrggbb or #rrggbbaa, tinted the way `tint` says when it's a grey.
///
/// A grey is its colorless self, which is just how light it looks, with Monokai's tint for it on top.
/// Monokai blends its greys from its text and background colors, so their tints run in a gradient
/// from the text's hue to the background's. Turned, the gradient keeps its shape, and every grey
/// stays as light and as colorful as it was.
fn tinted(color: &str, tint: Tint) -> Option<String> {
    let hex = color.strip_prefix('#').filter(|hex| matches!(hex.len(), 6 | 8))?;
    let [l, a, b] = oklab(u32::from_str_radix(&hex[..6], 16).ok()?);
    if a.hypot(b) >= GREY {
        return None;
    }
    let (a, b) = match tint {
        Tint::Neutral => (0., 0.),
        Tint::Turned(degrees) => {
            let (sin, cos) = f64::from(degrees).to_radians().sin_cos();
            (a * cos - b * sin, a * sin + b * cos)
        }
    };
    Some(format!("#{:06x}{}", srgb([l, a, b]), &hex[6..]))
}

/// Color `rgb`, as 0xrrggbb, in Oklab: how light it looks, how red or green, and how yellow or blue.
fn oklab(rgb: u32) -> [f64; 3] {
    let linear = |shift: u32| {
        let c = f64::from((rgb >> shift) & 0xff) / 255.;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (linear(16), linear(8), linear(0));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

/// Oklab color `lab` as 0xrrggbb, or the nearest to it there is.
fn srgb([l, a, b]: [f64; 3]) -> u32 {
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    let encode = |c: f64| {
        let c = c.clamp(0., 1.);
        let c = if c <= 0.0031308 { 12.92 * c } else { 1.055 * c.powf(1. / 2.4) - 0.055 };
        (c * 255.).round() as u32
    };
    let r = encode(4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_);
    let g = encode(-1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_);
    let b = encode(-0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_);
    r << 16 | g << 8 | b
}

/// Shows toasts lifted off the window, like anything floating over it, rather than sunk into it.
pub trait Toasts {
    fn toast(&mut self, note: impl Into<Notification>, cx: &mut App);

    /// Says what went wrong and what caused it, unless it was only cancelled.
    fn toast_error(&mut self, error: &anyhow::Error, cx: &mut App) {
        if !error.is::<Cancelled>() {
            self.toast(Notification::error(format!("{error:#}")), cx);
        }
    }
}

impl Toasts for Window {
    fn toast(&mut self, note: impl Into<Notification>, cx: &mut App) {
        let lifted = cx.theme().background.blend(white().opacity(0.05));
        self.push_notification(note.into().bg(lifted), cx);
    }
}

/// GPUI keeps the fonts it's given to itself, but fallbacks are found by CoreText, so a fallback font
/// has to be registered there. The call for it is deprecated in favor of registering descriptors,
/// which reports back through a block as it goes, where this one is done when it returns.
#[allow(deprecated)]
fn register_with_core_text(font: &'static [u8]) {
    use objc2_core_foundation::CFData;
    use objc2_core_graphics::{CGDataProvider, CGFont};
    let data = CFData::from_static_bytes(font);
    let font = CGDataProvider::with_cf_data(Some(&data)).and_then(|provider| CGFont::with_data_provider(&provider));
    if let Some(font) = font {
        unsafe { objc2_core_text::CTFontManagerRegisterGraphicsFont(&font, std::ptr::null_mut()) };
    }
}

#[cfg(test)]
mod tests {
    use super::{THEMES, Tint, oklab, recolor, tinted};

    #[test]
    fn tints_only_the_greys() {
        assert_eq!(tinted("#2d2a2e", Tint::Neutral).as_deref(), Some("#2b2b2b"));
        assert_eq!(tinted("#fcfcfa29", Tint::Neutral).as_deref(), Some("#fcfcfc29"));
        assert_eq!(tinted("#ffd866", Tint::Neutral), None);
        assert_eq!(tinted("JetBrains Mono", Tint::Neutral), None);
        // Turned, a grey looks as light and as colorful as it did.
        let turned = tinted("#2d2a2e", Tint::Turned(-68.)).unwrap();
        let [l, a, b] = oklab(0x2d2a2e);
        let [turned_l, turned_a, turned_b] = oklab(u32::from_str_radix(&turned[1..], 16).unwrap());
        assert!((l - turned_l).abs() < 0.005);
        assert!((a.hypot(b) - turned_a.hypot(turned_b)).abs() < 0.003);
        assert!((turned_b.atan2(turned_a).to_degrees().rem_euclid(360.) - 250.).abs() < 20.);
    }

    #[test]
    fn unturned_tint_is_monokai_exactly() {
        let monokai: serde_json::Value = serde_json::from_str(THEMES).unwrap();
        let mut tinted = monokai.clone();
        recolor(&mut tinted, Tint::Turned(0.));
        assert_eq!(tinted, monokai);
    }
}
