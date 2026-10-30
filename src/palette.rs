//! The command palette (⇧⌘P): every menu command that applies where you are, by name.

use std::ops::Range;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, h_flex,
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    scroll::Scrollbar,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

actions!(palette, [SelectNext, SelectPrevious, RunCommand, DismissPalette]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        // Over the input's own, which do nothing on one line.
        KeyBinding::new("down", SelectNext, Some("Palette > Input")),
        KeyBinding::new("up", SelectPrevious, Some("Palette > Input")),
        KeyBinding::new("enter", RunCommand, Some("Palette > Input")),
        KeyBinding::new("escape", DismissPalette, Some("Palette")),
    ]);
}

pub enum PaletteEvent {
    Dismissed,
    Confirmed(Box<dyn Action>),
}

/// A command from the menus: the menu it's in, its name there and what it does.
struct MenuCommand {
    menu: SharedString,
    name: SharedString,
    action: Box<dyn Action>,
}

pub struct Palette {
    query: Entity<InputState>,
    commands: Vec<MenuCommand>,
    /// The commands the query matches, in menu order.
    matches: Vec<usize>,
    /// Which of the matches Return runs.
    selected: usize,
    scroll: UniformListScrollHandle,
    /// Where focus was, which decides both what's available and where the command goes.
    previous: FocusHandle,
    _query: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    /// Lists the commands available from `previous`. Call before moving focus to the palette.
    pub fn new(previous: FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut commands = Vec::new();
        for menu in crate::workspace::actions::menus(cx) {
            for item in menu.items {
                if let MenuItem::Action { name, action, .. } = item
                    && (window.is_action_available(&*action, cx) || cx.is_action_available(&*action))
                {
                    commands.push(MenuCommand { menu: menu.name.clone(), name, action });
                }
            }
        }
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Execute a command…"));
        query.update(cx, |query, cx| query.focus(window, cx));
        let subscription = cx.subscribe(&query, |this, _, event, cx| {
            if let InputEvent::Change = event {
                this.filter(cx);
            }
        });
        Self {
            query,
            matches: (0..commands.len()).collect(),
            commands,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            previous,
            _query: subscription,
        }
    }

    pub fn previous_focus(&self) -> &FocusHandle {
        &self.previous
    }

    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_lowercase();
        self.matches = (0..self.commands.len())
            .filter(|&ix| {
                let command = &self.commands[ix];
                format!("{}: {}", command.menu, command.name).to_lowercase().contains(&query)
            })
            .collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.selected != ix {
            self.selected = ix;
            cx.notify();
        }
    }

    /// Moves the selection `step` matches along, round from one end to the other.
    fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.matches.len() as isize;
        if count > 0 {
            self.select((self.selected as isize + step).rem_euclid(count) as usize, cx);
            self.scroll.scroll_to_item(self.selected, ScrollStrategy::Nearest);
        }
    }

    fn run(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(&command) = self.matches.get(ix) {
            cx.emit(PaletteEvent::Confirmed(self.commands[command].action.boxed_clone()));
        }
    }

    /// Clears the query, or once it's clear, closes the palette.
    fn dismiss(&mut self, _: &DismissPalette, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.read(cx).value().is_empty() {
            return cx.emit(PaletteEvent::Dismissed);
        }
        self.query.update(cx, |query, cx| query.set_value("", window, cx));
        self.filter(cx);
    }

    fn render_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        // Concentric with the palette's corners, inside its border and the list's padding.
        let radius = (theme.radius_lg - window.rem_size() * 0.25 - px(1.)).max(px(0.));
        range
            .map(|ix| {
                let command = &self.commands[self.matches[ix]];
                h_flex()
                    .id(("command", ix))
                    .role(Role::ListBoxOption)
                    .aria_selected(ix == self.selected)
                    .w_full()
                    .gap_1()
                    .px_2()
                    .py_1p5()
                    .text_sm()
                    .rounded(radius)
                    .cursor_default()
                    .when(ix == self.selected, |el| el.bg(theme.accent).text_color(theme.accent_foreground))
                    .child(div().text_color(theme.muted_foreground).child(format!("{}:", command.menu)))
                    .child(command.name.clone())
                    .child(div().flex_1())
                    .children(Kbd::binding_for_action_in(&*command.action, &self.previous, window))
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        if *hovered {
                            this.select(ix, cx);
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.run(ix, cx)))
                    .into_any_element()
            })
            .collect()
    }
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let list = match self.matches.len() {
            0 => div()
                .py_7()
                .text_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No results found.")
                .into_any_element(),
            count => div()
                .id("command-list")
                .role(Role::ListBox)
                .relative()
                .child(
                    uniform_list("commands", count, cx.processor(Self::render_rows))
                        .with_sizing_behavior(ListSizingBehavior::Infer)
                        .max_h(px(420.))
                        .p_1()
                        .track_scroll(&self.scroll),
                )
                .child(Scrollbar::vertical(&self.scroll))
                .into_any_element(),
        };
        v_flex()
            .key_context("Palette")
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_by(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.select_by(-1, cx)))
            .on_action(cx.listener(|this, _: &RunCommand, _, cx| this.run(this.selected, cx)))
            .on_action(cx.listener(Self::dismiss))
            .w(px(560.))
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .child(
                div().flex_none().px_3().border_b_1().border_color(theme.border).child(
                    Input::new(&self.query)
                        .prefix(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                        .appearance(false)
                        .p_0(),
                ),
            )
            .child(list)
    }
}
