//! Pieces the sidebar, the path bar, the tab strip and the bars share.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    Theme,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::PopupMenuItem,
};
use gpui_kit::*;

use super::Workspace;

/// A row in a list, like the sidebar's places or the path bar's suggestions.
pub(super) fn row(id: impl Into<ElementId>, theme: &Theme) -> Stateful<Div> {
    h_flex().id(id).h(px(26.)).px_2().gap_2().rounded(theme.radius).text_sm().hover(|s| s.bg(theme.list_hover))
}

/// `text` on one line, cut short with "…" if it doesn't fit.
pub(super) fn truncated(text: impl Into<SharedString>) -> Div {
    div().min_w_0().truncate().child(text.into())
}

/// An item in a context menu that does `f` to the workspace when it's chosen.
pub(super) fn menu_item(
    label: &'static str,
    workspace: &WeakEntity<Workspace>,
    f: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> PopupMenuItem {
    let workspace = workspace.clone();
    PopupMenuItem::new(label).on_click(move |_, window, cx| {
        workspace.update(cx, |this, cx| f(this, window, cx)).ok();
    })
}

/// A button with just an icon that does `action`, whose keys its tooltip shows.
pub(super) fn action_button(id: &'static str, icon: IconName, tooltip: &'static str, action: impl Action) -> Button {
    let action: Box<dyn Action> = Box::new(action);
    Button::new(id)
        .ghost()
        .icon(icon)
        .tooltip_with_action(tooltip, &*action, Some("Workspace"))
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
}

/// The button at the end of a tab or a shelf item that closes it or takes it off, without the click
/// reaching what it's on.
pub(super) fn close_button(
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id("close")
        .flex_none()
        .size(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .hover(|s| s.bg(theme.secondary_active))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
}

/// Has `element` show the folder at `path` when it's clicked, or open it in a tab behind the active
/// one with ⌘ or a middle click, as places in the sidebar and the path bar do.
pub(super) fn opens_folder(element: Stateful<Div>, path: PathBuf, cx: &Context<Workspace>) -> Stateful<Div> {
    let clicked = path.clone();
    element
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            cx.stop_propagation();
            match event.modifiers().platform {
                true => this.open_behind(clicked.clone(), window, cx),
                false => this.show_folder(clicked.clone(), window, cx),
            }
        }))
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.open_behind(path.clone(), window, cx);
            }),
        )
}
