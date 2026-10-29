//! The command palette (⇧⌘P): every menu command that applies where you are, by name.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _,
    command::{Command, CommandItem, CommandState},
    h_flex,
    kbd::Kbd,
};
use gpui_kit::*;

pub enum PaletteEvent {
    Dismissed,
    Confirmed(Box<dyn Action>),
}

/// A command from the menus: the menu it's in, its name there and what it does.
struct MenuCommand {
    menu: SharedString,
    name: SharedString,
    action: Rc<dyn Action>,
}

pub struct Palette {
    state: Entity<CommandState>,
    commands: Rc<Vec<MenuCommand>>,
    /// Where focus was, which decides both what's available and where the command goes.
    previous: FocusHandle,
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
                    commands.push(MenuCommand { menu: menu.name.clone(), name, action: action.boxed_clone().into() });
                }
            }
        }
        let state = cx.new(|cx| CommandState::new(window, cx));
        state.update(cx, |state, cx| state.focus(window, cx));
        Self { state, commands: Rc::new(commands), previous }
    }

    pub fn previous_focus(&self) -> &FocusHandle {
        &self.previous
    }
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.weak_entity();
        let cancel = palette.clone();
        let items = self.commands.iter().map(|command| {
            let (menu, name, action, previous) =
                (command.menu.clone(), command.name.clone(), command.action.clone(), self.previous.clone());
            CommandItem::new().label(format!("{menu}: {name}")).child(move |window, cx| {
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(div().text_color(cx.theme().muted_foreground).child(format!("{menu}:")))
                    .child(name.clone())
                    .child(div().flex_1())
                    .children(Kbd::binding_for_action_in(&*action, &previous, window))
            })
        });
        let commands = self.commands.clone();
        div().w(px(560.)).rounded(cx.theme().radius_lg).shadow_lg().bg(cx.theme().popover).child(
            Command::new(&self.state)
                .placeholder("Execute a command…")
                .max_h(px(420.))
                .items(items)
                .on_confirm(move |index, _, cx| {
                    let action = commands[index.row].action.boxed_clone();
                    palette.update(cx, |_, cx| cx.emit(PaletteEvent::Confirmed(action))).ok();
                })
                .on_cancel(move |_, cx| {
                    cancel.update(cx, |_, cx| cx.emit(PaletteEvent::Dismissed)).ok();
                }),
        )
    }
}
