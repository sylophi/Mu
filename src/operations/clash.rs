//! Asking what to do when items copied or moved into a folder find their names taken there, the way
//! Windows does: folders merge without asking, and for the rest, replace them all, skip them all,
//! or decide for each, seeing both side by side.

use std::{ops::Range, path::PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use smol::channel::Sender;

use crate::{
    fs::{self, Choice, Choices, Clash},
    history::Op,
    icons,
};

/// How tall each file's row is when deciding for each, and how many show before they scroll.
const ROW: f32 = 72.;
const ROWS: usize = 4;

/// Asks what to do about what copying or moving `paths` into `dir` would clash with, if anything.
/// Resolves to the choices made, or None when it's cancelled.
pub fn choose(op: Op, paths: Vec<PathBuf>, dir: PathBuf, window: &mut Window, cx: &mut App) -> Task<Option<Choices>> {
    let scan = cx.background_spawn({
        let (paths, dir) = (paths.clone(), dir.clone());
        async move { fs::clashes(&paths, &dir).into_iter().map(Both::new).collect::<Vec<_>>() }
    });
    window.spawn(cx, async move |cx| {
        let clashes = scan.await;
        if clashes.is_empty() {
            return Some(Choices::new());
        }
        let (answer, chosen) = smol::channel::bounded(1);
        cx.update(|window, cx| {
            let from = paths.first().and_then(|path| path.parent()).map(fs::display_name).unwrap_or_default();
            let to = fs::display_name(&dir);
            let going = format!("{} {} from “{from}” to “{to}”", op.verb(), fs::count(paths.len(), "item", "items"));
            let keep = vec![[true, false]; clashes.len()];
            let ask = cx.new(|_| Ask { going, from, to, clashes, deciding: false, keep, answer });
            window.open_dialog(cx, move |dialog, _, _| {
                dialog.title("Replace or Skip Files").w(px(560.)).child(ask.clone())
            });
        })
        .ok()?;
        // Closing the dialog lets go of the sender, which cancels.
        chosen.recv().await.ok()
    })
}

/// A clash: the name that's taken, and the one coming in and the one already there, side by side.
struct Both {
    to: PathBuf,
    name: String,
    sides: [Side; 2],
}

/// One of the two in a clash, and what there is to compare of it.
struct Side {
    path: PathBuf,
    is_dir: bool,
    modified: String,
    /// How big it is, unless it's a folder.
    details: String,
}

impl Both {
    fn new(clash: Clash) -> Self {
        Self {
            name: fs::display_name(&clash.to),
            sides: [Side::new(clash.from), Side::new(clash.to.clone())],
            to: clash.to,
        }
    }
}

impl Side {
    fn new(path: PathBuf) -> Self {
        let meta = std::fs::metadata(&path).ok();
        let is_dir = meta.as_ref().is_some_and(|meta| meta.is_dir());
        Self {
            details: if is_dir { "Folder".into() } else { fs::format_size(meta.as_ref().map_or(0, |meta| meta.len())) },
            modified: meta.and_then(|meta| meta.modified().ok()).map(fs::format_date).unwrap_or_default(),
            is_dir,
            path,
        }
    }
}

struct Ask {
    /// What's going where, and the names of the two folders.
    going: String,
    from: String,
    to: String,
    clashes: Vec<Both>,
    /// Whether it's deciding for each.
    deciding: bool,
    /// For each, whether to keep the one coming in, and the one already there.
    keep: Vec<[bool; 2]>,
    answer: Sender<Choices>,
}

impl Ask {
    fn answer(&self, choices: Choices, window: &mut Window, cx: &mut App) {
        self.answer.try_send(choices).ok();
        window.close_dialog(cx);
    }

    fn all(&self, choice: Choice) -> Choices {
        self.clashes.iter().map(|both| (both.to.clone(), choice)).collect()
    }

    /// What's been decided for each: keeping both, replacing what's there, or keeping only it.
    fn decided(&self) -> Choices {
        let choice = |keep: [bool; 2]| match keep {
            [true, true] => Choice::KeepBoth,
            [true, false] => Choice::Replace,
            [false, _] => Choice::Skip,
        };
        self.clashes.iter().zip(&self.keep).map(|(both, &keep)| (both.to.clone(), choice(keep))).collect()
    }

    fn render_choices(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let one = self.clashes.len() == 1;
        let choice = |id: &'static str, icon: IconName, label: &'static str| {
            h_flex()
                .id(id)
                .px_3()
                .py_2()
                .gap_3()
                .rounded(theme.radius)
                .cursor_pointer()
                .hover(|s| s.bg(theme.list_hover))
                .child(Icon::new(icon).text_color(theme.muted_foreground))
                .child(label)
        };
        v_flex()
            .child(
                choice(
                    "replace",
                    IconName::Replace,
                    if one { "Replace the file in the destination" } else { "Replace the files in the destination" },
                )
                .on_click(cx.listener(|this, _, window, cx| this.answer(this.all(Choice::Replace), window, cx))),
            )
            .child(
                choice("skip", IconName::Redo2, if one { "Skip this file" } else { "Skip these files" })
                    .on_click(cx.listener(|this, _, window, cx| this.answer(this.all(Choice::Skip), window, cx))),
            )
            .child(
                choice(
                    "decide",
                    IconName::Columns2,
                    if one { "Compare both files" } else { "Let me decide for each file" },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.deciding = true;
                    cx.notify();
                })),
            )
    }

    fn render_decide(&self, cx: &mut Context<Self>) -> Div {
        let column = |side: usize, label: String| {
            let checked = self.keep.iter().all(|keep| keep[side]);
            div().flex_1().min_w_0().child(Checkbox::new(("all", side)).checked(checked).label(label).on_click(
                cx.listener(move |this, checked: &bool, _, cx| {
                    for keep in &mut this.keep {
                        keep[side] = *checked;
                    }
                    cx.notify();
                }),
            ))
        };
        let rows = uniform_list(
            "clashes",
            self.clashes.len(),
            cx.processor(|this, range: Range<usize>, _, cx| range.map(|ix| this.render_row(ix, cx)).collect()),
        )
        .w_full()
        .h(px(ROW * self.clashes.len().min(ROWS) as f32));
        v_flex()
            .child(
                h_flex()
                    .gap_4()
                    .pb_3()
                    .text_sm()
                    .child(column(0, format!("From “{}”", self.from)))
                    .child(column(1, format!("Already in “{}”", self.to))),
            )
            .child(rows)
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let both = &self.clashes[ix];
        let side = |side: usize| {
            let version = &both.sides[side];
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(Checkbox::new(("keep", ix * 2 + side)).checked(self.keep[ix][side]).on_click(cx.listener(
                    move |this, checked: &bool, _, cx| {
                        this.keep[ix][side] = *checked;
                        cx.notify();
                    },
                )))
                .child(icons::item(&version.path, version.is_dir, px(32.)))
                .child(
                    v_flex()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(div().truncate().child(version.modified.clone()))
                        .child(version.details.clone()),
                )
        };
        v_flex()
            .w_full()
            .h(px(ROW))
            .justify_center()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .child(div().text_sm().font_medium().truncate().child(both.name.clone()))
            .child(h_flex().gap_4().child(side(0)).child(side(1)))
    }
}

impl Render for Ask {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let headline = match (self.deciding, self.clashes.as_slice()) {
            (true, _) => "Which files do you want to keep?".to_string(),
            (false, [both]) => format!("The destination already has “{}”.", both.name),
            (false, clashes) => {
                format!("The destination has {} with the same names.", fs::count(clashes.len(), "file", "files"))
            }
        };
        let header = v_flex()
            .gap_1()
            .child(div().text_sm().text_color(theme.muted_foreground).child(self.going.clone()))
            .child(div().font_medium().child(headline))
            .when(self.deciding, |el| {
                el.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Keeping both adds a number to the name of the one coming in."),
                )
            });
        let body = if self.deciding { self.render_decide(cx) } else { self.render_choices(cx) };
        v_flex().gap_4().child(header).child(body).child(
            h_flex()
                .justify_end()
                .gap_2()
                .child(Button::new("cancel").label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                .when(self.deciding, |el| {
                    el.child(
                        Button::new("continue")
                            .primary()
                            .label("Continue")
                            .on_click(cx.listener(|this, _, window, cx| this.answer(this.decided(), window, cx))),
                    )
                }),
        )
    }
}
