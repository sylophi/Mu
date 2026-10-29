//! The docks either side of the pane, the sidebar and the preview: showing and hiding them,
//! dragging their edges, and zooming the pane past them.

use std::ops::Range;

use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::Workspace;
use crate::settings::{DOCK_WIDTHS, DockWidths, Settings};

/// The docks in a window: whether each shows, and how wide.
pub(super) struct Docks {
    pub(super) sidebar: bool,
    /// Whether the preview shows, beside a folder with room for it.
    pub(super) preview: bool,
    widths: DockWidths,
    /// The widths while an edge is dragged, kept once it's let go.
    dragged: Option<DockWidths>,
    /// Whether the pane fills the window, without the docks.
    zoomed: bool,
}

impl Docks {
    /// The docks as the last window to change them had them.
    pub(super) fn new(settings: &Settings) -> Self {
        Self {
            sidebar: settings.show_sidebar,
            preview: settings.show_preview,
            widths: settings.dock_widths,
            dragged: None,
            zoomed: false,
        }
    }

    fn widths(&self) -> DockWidths {
        self.dragged.unwrap_or(self.widths)
    }
}

/// A dock's edge, being dragged.
#[derive(Clone, Copy)]
enum Dock {
    Sidebar,
    Preview,
}

/// The least the pane can have with the preview beside it. Narrower, the preview hides.
const MIN_PANE_WIDTH: Pixels = px(400.);
/// How far the docks can be dragged.
const SIDEBAR_WIDTHS: Range<Pixels> = px(140.)..px(420.);
const PREVIEW_WIDTHS: Range<Pixels> = px(200.)..px(800.);

impl Workspace {
    /// The pane between the docks, or by itself while zoomed.
    pub(super) fn render_main(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.docks.zoomed {
            return self.render_center(window, cx);
        }
        let show_sidebar = self.docks.sidebar;
        let widths = self.docks.widths();
        // The preview is for folders, and steps aside while showing it would leave the pane too narrow
        // to use.
        let sidebar = if show_sidebar { widths.sidebar } else { px(0.) };
        let show_preview = self.docks.preview
            && self.active_folder().is_some()
            && window.viewport_size().width - sidebar - widths.preview >= MIN_PANE_WIDTH;
        let border = cx.theme().border;
        // The docks keep their widths as the window's resized, and the pane takes up the rest.
        // Only when the window's too narrow for that do they give way, down to their least.
        h_flex()
            .size_full()
            .when(show_sidebar, |el| {
                el.child(
                    div()
                        .relative()
                        .h_full()
                        .w(widths.sidebar)
                        .min_w(SIDEBAR_WIDTHS.start)
                        .border_r_1()
                        .border_color(border)
                        .child(self.render_sidebar(cx))
                        .child(self.render_dock_edge(Dock::Sidebar, cx)),
                )
            })
            .child(div().flex_1().min_w(px(240.)).h_full().child(self.render_center(window, cx)))
            .when(show_preview, |el| {
                // Cached: it only changes when the selection does, not on every hover elsewhere.
                let preview = self.preview.clone().cached(StyleRefinement::default().size_full());
                el.child(
                    div()
                        .relative()
                        .h_full()
                        .w(widths.preview)
                        .min_w(PREVIEW_WIDTHS.start)
                        .border_l_1()
                        .border_color(border)
                        .child(preview)
                        .child(self.render_dock_edge(Dock::Preview, cx)),
                )
            })
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<Dock>, _, cx| {
                let (bounds, x) = (event.bounds, event.event.position.x);
                let mut widths = this.docks.widths();
                match event.drag(cx) {
                    Dock::Sidebar => {
                        widths.sidebar = (x - bounds.left()).clamp(SIDEBAR_WIDTHS.start, SIDEBAR_WIDTHS.end)
                    }
                    Dock::Preview => {
                        widths.preview = (bounds.right() - x).clamp(PREVIEW_WIDTHS.start, PREVIEW_WIDTHS.end)
                    }
                }
                this.docks.dragged = Some(widths);
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::finish_dock_drag))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::finish_dock_drag))
            .into_any_element()
    }

    pub(super) fn toggle_zoom(&mut self, cx: &mut Context<Self>) {
        self.docks.zoomed = !self.docks.zoomed;
        cx.notify();
    }

    /// The edge of a dock that's dragged to resize it, or double-clicked to put it back.
    fn render_dock_edge(&self, dock: Dock, cx: &Context<Self>) -> impl IntoElement {
        let edge = div()
            .id(match dock {
                Dock::Sidebar => "sidebar-edge",
                Dock::Preview => "preview-edge",
            })
            .absolute()
            .top_0()
            .h_full()
            .w(px(6.))
            .cursor_col_resize()
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    let mut widths = this.docks.widths;
                    match dock {
                        Dock::Sidebar => widths.sidebar = DOCK_WIDTHS.sidebar,
                        Dock::Preview => widths.preview = DOCK_WIDTHS.preview,
                    }
                    this.resize_docks(widths, cx);
                }
            }))
            .on_drag(dock, |_, _, _, cx| cx.new(|_| EmptyView));
        // Deferred, so it's over the pane it straddles, like Zed's.
        deferred(match dock {
            Dock::Sidebar => edge.right(px(-3.)),
            Dock::Preview => edge.left(px(-3.)),
        })
    }

    fn finish_dock_drag(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(widths) = self.docks.dragged.take() {
            self.resize_docks(widths, cx);
        }
    }

    /// Resizes the docks in this window, and has new windows start out the same.
    fn resize_docks(&mut self, widths: DockWidths, cx: &mut Context<Self>) {
        self.docks.widths = widths;
        Settings::update(cx, |settings| settings.dock_widths = widths);
    }

    /// Shows or hides the sidebar in this window, and has new windows start out the same.
    pub(super) fn show_sidebar(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.docks.sidebar = shown;
        Settings::update(cx, |settings| settings.show_sidebar = shown);
    }

    /// Shows or hides the preview in this window, and has new windows start out the same.
    pub(super) fn show_preview(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.docks.preview = shown;
        self.preview.update(cx, |preview, cx| preview.set_shown(shown, cx));
        Settings::update(cx, |settings| settings.show_preview = shown);
    }
}
