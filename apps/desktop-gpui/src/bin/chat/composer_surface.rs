use gpui::prelude::*;
use gpui::{canvas, point, px, Bounds, Path, PathBuilder, Pixels};

use super::Theme;

pub(super) fn surface(theme: &Theme, accepts_files: bool) -> impl IntoElement {
    let radius = theme.radii.panel;
    let palette = theme.palette;
    canvas(
        move |bounds, _, _| {
            // The canvas is inside the layout's transparent 1px border.
            let bounds = bounds.inset(px(-1.0));
            let fill = outline(PathBuilder::fill(), bounds, radius);
            let border = outline(PathBuilder::stroke(px(0.5)), bounds, radius);
            (fill, border)
        },
        move |bounds, (fill, border), window, cx| {
            if let Some(path) = fill {
                window.paint_path(path, palette.color(palette.input_major));
            }
            if let Some(path) = border {
                let dragging = accepts_files
                    && cx.has_active_drag()
                    && bounds.contains(&window.mouse_position());
                window.paint_path(
                    path,
                    palette.color(if dragging {
                        palette.button_info
                    } else {
                        palette.border_l2
                    }),
                );
            }
        },
    )
    .absolute()
    .inset_0()
}

fn outline(mut path: PathBuilder, bounds: Bounds<Pixels>, radius: Pixels) -> Option<Path<Pixels>> {
    let radius = radius.min(bounds.size.width.min(bounds.size.height) / 2.0);
    if radius <= px(0.0) {
        return None;
    }
    let centers = [
        point(bounds.right() - radius, bounds.top() + radius),
        point(bounds.right() - radius, bounds.bottom() - radius),
        point(bounds.left() + radius, bounds.bottom() - radius),
        point(bounds.left() + radius, bounds.top() + radius),
    ];
    let quarter_turn = std::f32::consts::FRAC_PI_2;
    // CSS superellipse(1.5) has an implicit exponent of 2^1.5.
    let exponent = 2.0 / 2.0_f32.powf(1.5);
    path.move_to(point(centers[0].x, bounds.top()));
    for (index, center) in centers.into_iter().enumerate() {
        for step in 0..=16 {
            let angle = (index as f32 - 1.0 + step as f32 / 16.0) * quarter_turn;
            let (sin, cos) = angle.sin_cos();
            path.line_to(
                center
                    + point(
                        radius * cos.abs().powf(exponent).copysign(cos),
                        radius * sin.abs().powf(exponent).copysign(sin),
                    ),
            );
        }
    }
    path.close();
    path.build().ok()
}
