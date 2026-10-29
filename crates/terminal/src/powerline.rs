//! Powerline's separators, which prompts put between their segments, drawn to fill the cell as
//! Ghostty draws them. A font's are sized to its own glyphs, so they'd stop short of the rows here,
//! which are taller, or sit off them.

use gpui_kit::*;

/// The separator `c` filling `cell`, if it's one.
pub fn separator(c: char, cell: Bounds<Pixels>) -> Option<Path<Pixels>> {
    // A point in the cell, as fractions of its width and height.
    let at = |(x, y): (f32, f32)| point(cell.left() + cell.size.width * x, cell.top() + cell.size.height * y);
    let builder = |filled: bool| if filled { PathBuilder::fill() } else { PathBuilder::stroke(px(1.)) };
    let lines = |filled: bool, points: &[(f32, f32)]| {
        let mut path = builder(filled);
        path.add_polygon(&points.iter().copied().map(at).collect::<Vec<_>>(), filled);
        path
    };
    // Half an ellipse from the cell's top to its bottom, bulging right from its left edge or left
    // from its right edge.
    let half_ellipse = |filled: bool, right: bool| {
        let x = if right { 0. } else { 1. };
        let mut path = builder(filled);
        path.move_to(at((x, 0.)));
        path.arc_to(point(cell.size.width, cell.size.height / 2.), px(0.), false, right, at((x, 1.)));
        if filled {
            path.close();
        }
        path
    };
    let path = match c {
        '\u{e0b0}' => lines(true, &[(0., 0.), (1., 0.5), (0., 1.)]),
        '\u{e0b1}' => lines(false, &[(0., 0.), (1., 0.5), (0., 1.)]),
        '\u{e0b2}' => lines(true, &[(1., 0.), (0., 0.5), (1., 1.)]),
        '\u{e0b3}' => lines(false, &[(1., 0.), (0., 0.5), (1., 1.)]),
        '\u{e0b4}' => half_ellipse(true, true),
        '\u{e0b5}' => half_ellipse(false, true),
        '\u{e0b6}' => half_ellipse(true, false),
        '\u{e0b7}' => half_ellipse(false, false),
        '\u{e0b8}' => lines(true, &[(0., 0.), (1., 1.), (0., 1.)]),
        '\u{e0b9}' | '\u{e0bf}' => lines(false, &[(0., 0.), (1., 1.)]),
        '\u{e0ba}' => lines(true, &[(1., 0.), (1., 1.), (0., 1.)]),
        '\u{e0bb}' | '\u{e0bd}' => lines(false, &[(1., 0.), (0., 1.)]),
        '\u{e0bc}' => lines(true, &[(0., 0.), (1., 0.), (0., 1.)]),
        '\u{e0be}' => lines(true, &[(0., 0.), (1., 0.), (1., 1.)]),
        _ => return None,
    };
    path.build().ok()
}
