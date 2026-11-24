//! Vector icons and procedural album cover artwork built with Rune's GPU `Shape` primitive.

use rune::{Color, El, LineCap, Path, Shape, canvas, oklch, oklcha};

/// Renders a heart icon inside a `size × size` element.
pub fn heart_icon(size: f32, color: Color, filled: bool) -> El {
    let path = Path::new()
        .move_to([12.0, 20.5])
        .cubic_to([4.0, 15.0], [2.5, 10.5], [2.5, 7.2])
        .cubic_to([2.5, 4.2], [4.8, 2.2], [7.5, 2.2])
        .cubic_to([9.4, 2.2], [11.1, 3.2], [12.0, 4.8])
        .cubic_to([12.9, 3.2], [14.6, 2.2], [16.5, 2.2])
        .cubic_to([19.2, 2.2], [21.5, 4.2], [21.5, 7.2])
        .cubic_to([21.5, 10.5], [20.0, 15.0], [12.0, 20.5])
        .close();

    let shape = if filled {
        Shape::new().view_box(24.0, 24.0).fill_path(&path, color)
    } else {
        Shape::new()
            .view_box(24.0, 24.0)
            .stroke_path(&path, 2.2, LineCap::Round, color)
    };
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a crisp `✕` icon for SKIP and Remove from selection.
pub fn x_icon(size: f32, stroke_width: f32, color: Color) -> El {
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .line([6.0, 6.0], [18.0, 18.0], stroke_width, LineCap::Round, color)
        .line([18.0, 6.0], [6.0, 18.0], stroke_width, LineCap::Round, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a `+` icon for ADD actions.
pub fn plus_icon(size: f32, stroke_width: f32, color: Color) -> El {
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .line([12.0, 5.0], [12.0, 19.0], stroke_width, LineCap::Round, color)
        .line([5.0, 12.0], [19.0, 12.0], stroke_width, LineCap::Round, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a checkmark `✓` icon.
pub fn check_icon(size: f32, stroke_width: f32, color: Color) -> El {
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .stroke(vec![[5.0, 12.5], [10.0, 17.5], [19.5, 6.5]], stroke_width, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders an Undo counter-clockwise arrow icon (`↺`).
pub fn undo_icon(size: f32, color: Color) -> El {
    let arc = Path::new()
        .move_to([4.5, 10.0])
        .cubic_to([6.5, 5.5], [11.5, 4.0], [16.0, 5.5])
        .cubic_to([20.0, 7.0], [21.5, 11.8], [19.8, 16.0])
        .cubic_to([18.2, 19.8], [13.5, 21.0], [9.0, 19.5]);
    let arrow = vec![[4.0, 5.0], [4.2, 10.2], [9.4, 9.8]];
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .stroke_path(&arc, 2.2, LineCap::Round, color)
        .stroke(arrow, 2.2, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders an External Link icon (`↗`) for "Open in Spotify".
pub fn external_link_icon(size: f32, color: Color) -> El {
    let box_path = vec![
        [11.0, 5.0],
        [5.0, 5.0],
        [5.0, 19.0],
        [19.0, 19.0],
        [19.0, 13.0],
    ];
    let arrow_head = vec![[14.0, 4.5], [19.5, 4.5], [19.5, 10.0]];
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .stroke(box_path, 2.0, color)
        .stroke(arrow_head, 2.0, color)
        .line([10.5, 13.5], [19.2, 4.8], 2.0, LineCap::Round, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a left chevron (`‹`) for back navigation.
pub fn chevron_left_icon(size: f32, color: Color) -> El {
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .stroke(vec![[15.0, 6.0], [9.0, 12.0], [15.0, 18.0]], 2.2, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a magnifying glass search icon.
pub fn search_icon(size: f32, color: Color) -> El {
    let circle = Path::new()
        .move_to([11.0, 4.0])
        .cubic_to([14.9, 4.0], [18.0, 7.1], [18.0, 11.0])
        .cubic_to([18.0, 14.9], [14.9, 18.0], [11.0, 18.0])
        .cubic_to([7.1, 18.0], [4.0, 14.9], [4.0, 11.0])
        .cubic_to([4.0, 7.1], [7.1, 4.0], [11.0, 4.0])
        .close();
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .stroke_path(&circle, 2.0, LineCap::Round, color)
        .line([16.2, 16.2], [20.5, 20.5], 2.2, LineCap::Round, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a music note icon.
pub fn music_icon(size: f32, color: Color) -> El {
    let stem = vec![[9.0, 18.0], [9.0, 5.5], [19.0, 3.5], [19.0, 16.0]];
    let note1 = Path::new()
        .move_to([6.5, 15.5])
        .cubic_to([8.0, 15.5], [9.0, 16.6], [9.0, 18.0])
        .cubic_to([9.0, 19.4], [8.0, 20.5], [6.5, 20.5])
        .cubic_to([5.0, 20.5], [4.0, 19.4], [4.0, 18.0])
        .cubic_to([4.0, 16.6], [5.0, 15.5], [6.5, 15.5])
        .close();
    let note2 = Path::new()
        .move_to([16.5, 13.5])
        .cubic_to([18.0, 13.5], [19.0, 14.6], [19.0, 16.0])
        .cubic_to([19.0, 17.4], [18.0, 18.5], [16.5, 18.5])
        .cubic_to([15.0, 18.5], [14.0, 17.4], [14.0, 16.0])
        .cubic_to([14.0, 14.6], [15.0, 13.5], [16.5, 13.5])
        .close();
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .stroke(stem, 2.0, color)
        .fill_path(&note1, color)
        .fill_path(&note2, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a photo/cover image icon.
pub fn image_icon(size: f32, color: Color) -> El {
    let frame = vec![
        [4.0, 4.0],
        [20.0, 4.0],
        [20.0, 20.0],
        [4.0, 20.0],
        [4.0, 4.0],
    ];
    let mountain = vec![[4.5, 17.0], [9.5, 12.0], [13.5, 16.0], [16.5, 13.0], [19.5, 17.0]];
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .stroke(frame, 1.9, color)
        .stroke(mountain, 1.9, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a play triangle icon (`▶`).
pub fn play_icon(size: f32, color: Color) -> El {
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .fill(vec![[7.5, 4.5], [19.5, 12.0], [7.5, 19.5]], color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a pause bars icon (`⏸`).
pub fn pause_icon(size: f32, color: Color) -> El {
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .fill(
            vec![[6.5, 5.0], [10.2, 5.0], [10.2, 19.0], [6.5, 19.0]],
            color,
        )
        .fill(
            vec![[13.8, 5.0], [17.5, 5.0], [17.5, 19.0], [13.8, 19.0]],
            color,
        );
    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a Spotify-style speaker icon (high / low / muted).
pub fn volume_icon(size: f32, color: Color, level: f32, muted: bool) -> El {
    let speaker = vec![
        [3.0, 9.5],
        [7.2, 9.5],
        [12.0, 5.2],
        [12.0, 18.8],
        [7.2, 14.5],
        [3.0, 14.5],
    ];
    let mut shape = Shape::new().view_box(24.0, 24.0).fill(speaker, color);

    if muted || level <= 0.005 {
        shape = shape
            .line([15.5, 9.0], [21.0, 15.0], 2.0, LineCap::Round, color)
            .line([21.0, 9.0], [15.5, 15.0], 2.0, LineCap::Round, color);
    } else {
        let wave1 = Path::new()
            .move_to([15.2, 8.8])
            .cubic_to([17.2, 10.5], [17.2, 13.5], [15.2, 15.2]);
        shape = shape.stroke_path(&wave1, 1.9, LineCap::Round, color);

        if level >= 0.48 {
            let wave2 = Path::new()
                .move_to([18.0, 6.2])
                .cubic_to([21.2, 9.2], [21.2, 14.8], [18.0, 17.8]);
            shape = shape.stroke_path(&wave2, 1.9, LineCap::Round, color);
        }
    }

    canvas(shape).size(size, size).a11y_hidden()
}

/// Renders a shuffle icon.
pub fn shuffle_icon(size: f32, color: Color) -> El {
    let shape = Shape::new()
        .view_box(24.0, 24.0)
        .stroke(vec![[16.0, 3.0], [21.0, 3.0], [21.0, 8.0]], 2.0, color)
        .line([4.0, 20.0], [21.0, 3.0], 2.0, LineCap::Round, color)
        .stroke(vec![[21.0, 16.0], [21.0, 21.0], [16.0, 21.0]], 2.0, color)
        .line([15.0, 15.0], [21.0, 21.0], 2.0, LineCap::Round, color)
        .line([4.0, 4.0], [9.0, 9.0], 2.0, LineCap::Round, color);
    canvas(shape).size(size, size).a11y_hidden()
}

/// Generates a rich procedural vinyl/geometric artwork canvas for a given hue so cards look great
/// even before external artwork loads or in offline/headless runs.
pub fn vinyl_cover_art(hue: f32, width: f32, height: f32) -> El {
    let c_ring = oklcha(0.95, 0.04, hue, 0.14);
    let c_accent = oklch(0.72, 0.18, hue);
    let c_accent2 = oklch(0.60, 0.20, (hue + 48.0) % 360.0);
    let c_dark = oklcha(0.08, 0.02, hue, 0.78);

    let circle_pts = |cx: f32, cy: f32, r: f32, steps: usize| -> Vec<[f32; 2]> {
        (0..steps)
            .map(|i| {
                let a = (i as f32 / steps as f32) * std::f32::consts::TAU;
                [cx + r * a.cos(), cy + r * a.sin()]
            })
            .collect()
    };

    let cx = 190.0;
    let cy = 195.0;

    let mut shape = Shape::new().view_box(380.0, 510.0);
    // Decorative angled wave polygon
    shape = shape.fill(
        vec![[0.0, 310.0], [380.0, 180.0], [380.0, 510.0], [0.0, 510.0]],
        oklcha(0.22, 0.11, (hue + 35.0) % 360.0, 0.35),
    );
    // Outer vinyl disc
    shape = shape.fill(circle_pts(cx, cy, 124.0, 48), c_dark);
    shape = shape.stroke_with(circle_pts(cx, cy, 124.0, 48), true, 1.5, LineCap::Round, c_ring);
    shape = shape.stroke_with(circle_pts(cx, cy, 102.0, 40), true, 1.0, LineCap::Round, c_ring);
    shape = shape.stroke_with(circle_pts(cx, cy, 80.0, 36), true, 1.0, LineCap::Round, c_ring);
    // Inner vinyl label
    shape = shape.fill(circle_pts(cx, cy, 46.0, 32), c_accent);
    shape = shape.fill(circle_pts(cx, cy, 28.0, 24), c_accent2);
    // Spindle hole
    shape = shape.fill(circle_pts(cx, cy, 8.0, 16), oklch(0.10, 0.01, hue));

    canvas(shape).size(width, height).a11y_hidden()
}
