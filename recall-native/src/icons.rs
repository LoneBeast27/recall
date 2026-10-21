//! Compact transparent canvas icons for the Recall native editor.
use gpui::{AnyElement, Hsla, PathBuilder, Window, canvas, div, point, prelude::*, px};

#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Play,
    Pause,
    SkipBack,
    SkipForward,
    Scissors,
    Trash,
    Undo,
    Redo,
    Save,
    Export,
    Volume,
    Mic,
    Filmstrip,
    Folder,
}

/// Returns an 18px neutral stroke icon. The caller supplies the theme colour.
pub fn icon(kind: Icon, color: Hsla) -> AnyElement {
    div()
        .w(px(18.))
        .h(px(18.))
        .flex_none()
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    paint(
                        kind,
                        color,
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y),
                        window,
                    )
                },
            )
            .size_full(),
        )
        .into_any_element()
}

fn line(window: &mut Window, color: Hsla, x: f32, y: f32, points: &[(f32, f32)]) {
    let mut path = PathBuilder::stroke(px(1.5));
    if let Some((first_x, first_y)) = points.first() {
        path.move_to(point(px(x + first_x), px(y + first_y)));
        for (next_x, next_y) in &points[1..] {
            path.line_to(point(px(x + next_x), px(y + next_y)));
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}

fn fill(window: &mut Window, color: Hsla, x: f32, y: f32, points: &[(f32, f32)]) {
    let mut path = PathBuilder::fill();
    for (index, (point_x, point_y)) in points.iter().enumerate() {
        if index == 0 {
            path.move_to(point(px(x + point_x), px(y + point_y)));
        } else {
            path.line_to(point(px(x + point_x), px(y + point_y)));
        }
    }
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn paint(kind: Icon, color: Hsla, x: f32, y: f32, window: &mut Window) {
    match kind {
        Icon::Play => fill(window, color, x, y, &[(6., 3.), (15., 9.), (6., 15.)]),
        Icon::Pause => {
            line(window, color, x, y, &[(6., 3.), (6., 15.)]);
            line(window, color, x, y, &[(12., 3.), (12., 15.)]);
        }
        Icon::SkipBack => {
            fill(window, color, x, y, &[(5., 9.), (12., 3.), (12., 15.)]);
            line(window, color, x, y, &[(4., 3.), (4., 15.)]);
        }
        Icon::SkipForward => {
            fill(window, color, x, y, &[(13., 9.), (6., 3.), (6., 15.)]);
            line(window, color, x, y, &[(14., 3.), (14., 15.)]);
        }
        Icon::Scissors => {
            line(window, color, x, y, &[(4., 4.), (14., 14.)]);
            line(window, color, x, y, &[(14., 4.), (4., 14.)]);
            line(window, color, x, y, &[(3., 4.), (5., 4.)]);
            line(window, color, x, y, &[(13., 14.), (15., 14.)]);
        }
        Icon::Trash => {
            line(
                window,
                color,
                x,
                y,
                &[(5., 5.), (13., 5.), (12., 15.), (6., 15.), (5., 5.)],
            );
            line(window, color, x, y, &[(4., 3.), (14., 3.)]);
            line(
                window,
                color,
                x,
                y,
                &[(8., 3.), (8., 2.), (10., 2.), (10., 3.)],
            );
        }
        Icon::Undo => {
            line(window, color, x, y, &[(7., 5.), (3., 9.), (7., 13.)]);
            line(window, color, x, y, &[(4., 9.), (12., 9.), (15., 12.)]);
        }
        Icon::Redo => {
            line(window, color, x, y, &[(11., 5.), (15., 9.), (11., 13.)]);
            line(window, color, x, y, &[(14., 9.), (6., 9.), (3., 12.)]);
        }
        Icon::Save => {
            line(
                window,
                color,
                x,
                y,
                &[
                    (4., 3.),
                    (14., 3.),
                    (15., 4.),
                    (15., 15.),
                    (3., 15.),
                    (3., 4.),
                    (4., 3.),
                ],
            );
            line(
                window,
                color,
                x,
                y,
                &[(6., 3.), (6., 7.), (12., 7.), (12., 3.)],
            );
            line(
                window,
                color,
                x,
                y,
                &[(6., 15.), (6., 10.), (12., 10.), (12., 15.)],
            );
        }
        Icon::Export => {
            line(window, color, x, y, &[(9., 3.), (9., 12.)]);
            line(window, color, x, y, &[(5., 7.), (9., 3.), (13., 7.)]);
            line(
                window,
                color,
                x,
                y,
                &[(4., 11.), (4., 15.), (14., 15.), (14., 11.)],
            );
        }
        Icon::Volume => {
            fill(
                window,
                color,
                x,
                y,
                &[
                    (3., 7.),
                    (6., 7.),
                    (10., 4.),
                    (10., 14.),
                    (6., 11.),
                    (3., 11.),
                ],
            );
            line(window, color, x, y, &[(12., 7.), (14., 9.), (12., 11.)]);
        }
        Icon::Mic => {
            line(
                window,
                color,
                x,
                y,
                &[
                    (7., 3.),
                    (11., 3.),
                    (11., 10.),
                    (9., 12.),
                    (7., 10.),
                    (7., 3.),
                ],
            );
            line(
                window,
                color,
                x,
                y,
                &[(5., 9.), (5., 11.), (9., 14.), (13., 11.), (13., 9.)],
            );
            line(window, color, x, y, &[(9., 14.), (9., 16.)]);
        }
        Icon::Filmstrip => {
            line(
                window,
                color,
                x,
                y,
                &[(3., 3.), (15., 3.), (15., 15.), (3., 15.), (3., 3.)],
            );
            line(window, color, x, y, &[(6., 3.), (6., 15.)]);
            line(window, color, x, y, &[(12., 3.), (12., 15.)]);
        }
        Icon::Folder => {
            line(
                window,
                color,
                x,
                y,
                &[
                    (3., 5.),
                    (8., 5.),
                    (10., 7.),
                    (15., 7.),
                    (14., 15.),
                    (3., 15.),
                    (3., 5.),
                ],
            );
        }
    }
}
