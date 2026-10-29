//! The Settings tab (⌘,).

use std::io;

use crate::{
    finder, fs, icons,
    link::{self, Installed},
    permissions::{self, Permissions, Watch},
    settings::Settings,
    theme::{self, Toasts as _},
    update::{self, CheckForUpdates},
    workspace::{TabEvent, actions::ImportFromFinder},
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    notification::Notification,
    slider::{Slider, SliderEvent, SliderState},
    switch::Switch,
    theme::oklch,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

pub struct SettingsPage {
    focus_handle: FocusHandle,
    /// The hue the greys lean toward.
    hue: Entity<SliderState>,
    _watch: Watch,
    _observe: Subscription,
    _hue: Subscription,
}

impl EventEmitter<TabEvent> for SettingsPage {}

impl SettingsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let permissions = Permissions::global(cx);
        let hue = cx.new(|cx| SliderState::new().min(0.).max(360.).step(1.).default_value(theme::background_hue(cx)));
        // Turned as it's dragged, and saved once it's let go.
        let _hue = cx.subscribe(&hue, |_, _, event, cx| match event {
            SliderEvent::Change(value) => theme::turn_tint(value.start(), false, cx),
            SliderEvent::Release(value) => theme::turn_tint(value.start(), true, cx),
        });
        Self {
            focus_handle: cx.focus_handle(),
            hue,
            _watch: permissions.read(cx).watch(),
            _observe: cx.observe(&permissions, |_, _, cx| cx.notify()),
            _hue,
        }
    }

    fn render_tint(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let settings = Settings::get(cx);
        let (tint, changed) = (settings.tint, settings.tint_turn != 0.);
        let theme = cx.theme();
        let hue = self.hue.clone();
        let controls = h_flex()
            .gap_4()
            .when(tint && changed, |row| {
                row.child(Button::new("monokai-hue").ghost().small().label("Reset").on_click(move |_, window, cx| {
                    Settings::update(cx, |settings| settings.tint_turn = 0.);
                    theme::apply(cx);
                    hue.update(cx, |hue, cx| hue.set_value(theme::background_hue(cx), window, cx));
                }))
            })
            .child(Switch::new("tint").checked(tint).on_click(|on, _, cx| {
                Settings::update(cx, |settings| settings.tint = *on);
                theme::apply(cx);
            }));
        let header = setting("Tint", about("Greys lean toward a hue.", cx), controls);
        // Every hue at one lightness and colorfulness, so none stands out.
        const STEPS: usize = 12;
        let swatch = |ix: usize| oklch(0.72, 0.1, (ix * 360 / STEPS) as f32);
        let track = h_flex().absolute().left_0().right_0().h_1p5().children((0..STEPS).map(|ix| {
            let gradient =
                linear_gradient(90., linear_color_stop(swatch(ix), 0.), linear_color_stop(swatch(ix + 1), 1.));
            div()
                .flex_1()
                .h_full()
                .bg(gradient.color_space(ColorSpace::Oklab))
                .when(ix == 0, |el| el.rounded_l_full())
                .when(ix == STEPS - 1, |el| el.rounded_r_full())
        }));
        let slider = div()
            .relative()
            .flex()
            .items_center()
            .child(track)
            .child(Slider::new(&self.hue).bg(theme.transparent).text_color(theme.foreground));
        v_flex().gap_3().child(header).when(tint, |section| section.child(slider))
    }

    fn render_file_viewer(&self, on: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let text = "It stops working in Safari, Mail, Preview and App Store apps.";
        let toggle = Switch::new("file-viewer").checked(on).on_click(cx.listener(|_, on: &bool, _, cx| {
            finder::set_shows_for_finder(*on);
            cx.notify();
        }));
        setting("Redirect “Show in Finder” to Mu", about(text, cx), toggle)
    }

    /// A setting that's on or off, which `set` changes.
    fn render_switch(
        &self,
        name: &'static str,
        text: &'static str,
        on: bool,
        set: fn(&mut Settings, bool),
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let toggle = Switch::new(name).checked(on).on_click(cx.listener(move |_, on: &bool, _, cx| {
            Settings::update(cx, |settings| set(settings, *on));
            cx.notify();
        }));
        setting(name, about(text, cx), toggle)
    }

    /// The `mu` command, for opening tabs here from a shell.
    fn render_command(&self, installed: Installed, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let change = |verb: &'static str, run: fn() -> io::Result<()>| {
            cx.listener(move |_, _: &ClickEvent, window, cx| {
                if let Err(error) = run() {
                    window.toast(Notification::error(format!("Couldn't {verb} mu: {error}")), cx);
                }
                cx.notify();
            })
        };
        let theme = cx.theme();
        let link = link::command_link();
        let action = match installed {
            Installed::Yes => h_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_1()
                        .text_sm()
                        .text_color(theme.success)
                        .child(Icon::new(IconName::CircleCheck).small())
                        .child("Installed"),
                )
                .child(
                    Button::new("uninstall-command")
                        .small()
                        .ghost()
                        .label("Uninstall")
                        .on_click(change("uninstall", link::uninstall)),
                )
                .into_any_element(),
            Installed::No | Installed::Other => Button::new("install-command")
                .outline()
                .label(if installed == Installed::Other { "Replace and Install" } else { "Install" })
                .on_click(change("install", link::install))
                .into_any_element(),
        };
        let text = format!(
            "Open any tab here from a shell, like mu ~/Downloads or mu terminal. Installs in {}.",
            fs::typed_path(link.parent().expect("it's in a folder"))
        );
        let details = v_flex().gap_2().child(about(text, cx)).when(installed == Installed::Other, |details| {
            details.child(
                div()
                    .text_sm()
                    .text_color(theme.warning)
                    .child(format!("{} is something else.", fs::typed_path(&link))),
            )
        });
        setting("Command line tool", details, action)
    }
}

/// A setting: its name and what it does, with its controls at the end, which go under the rest
/// when short of room.
fn setting(name: &'static str, about: impl IntoElement, controls: impl IntoElement) -> Div {
    h_flex()
        .flex_wrap()
        .gap_4()
        .child(v_flex().flex_1().min_w(px(160.)).gap_2().child(div().font_semibold().child(name)).child(about))
        .child(div().ml_auto().child(controls))
}

/// What a setting does, under its name.
fn about(text: impl Into<SharedString>, cx: &App) -> Div {
    div().text_sm().text_color(cx.theme().muted_foreground).child(text.into())
}

impl Focusable for SettingsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let missing = Permissions::global(cx).read(cx).missing() > 0;
        let settings = Settings::get(cx);
        let (edit_text, extract_archives) = (settings.edit_text, settings.extract_archives);
        let theme = cx.theme();
        div().id("settings").size_full().overflow_y_scroll().track_focus(&self.focus_handle).bg(theme.background).child(
            v_flex()
                .max_w(px(720.))
                .mx_auto()
                .p_6()
                .gap_6()
                .child(
                    h_flex()
                        .gap_4()
                        .child(icons::logo(px(48.), cx))
                        .child(v_flex().child(div().text_xl().font_semibold().child("Mu")).children(
                            update::this_version().map(|version| {
                                div().text_sm().text_color(theme.muted_foreground).child(format!("Version {version}"))
                            }),
                        ))
                        .child(
                            Button::new("check-for-updates")
                                .ml_auto()
                                .outline()
                                .label("Check for Updates")
                                .on_click(|_, window, cx| window.dispatch_action(Box::new(CheckForUpdates), cx)),
                        ),
                )
                .child(
                    v_flex()
                        .gap_2()
                        .child(setting(
                            "Permissions",
                            about(permissions::ALLOW_ONCE, cx),
                            h_flex().when(missing, |el| el.child(permissions::allow_all_button(cx))),
                        ))
                        .child(permissions::render_list(cx)),
                )
                .child(setting(
                    "Finder",
                    about("Bring over its sidebar favorites, hidden files, panes and view.", cx),
                    Button::new("import-finder")
                        .outline()
                        .label("Import")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ImportFromFinder), cx)),
                ))
                .children(finder::shows_for_finder().map(|on| self.render_file_viewer(on, cx)))
                .child(self.render_switch(
                    "Open text files in Mu's editor",
                    "Instead of TextEdit.",
                    edit_text,
                    |settings, on| settings.edit_text = on,
                    cx,
                ))
                .child(self.render_switch(
                    "Extract archives in Mu",
                    "Instead of Archive Utility.",
                    extract_archives,
                    |settings, on| settings.extract_archives = on,
                    cx,
                ))
                .children(link::installed().map(|installed| self.render_command(installed, cx)))
                .child(self.render_tint(cx)),
        )
    }
}
