use fltk::{
    group::Group,
    prelude::{GroupExt, WidgetExt},
};

pub mod browse;

pub fn reflow_widgets(group: &Group, w: i32, h: i32, margin: i32, gap: i32) {
    let mut x = group.x() + margin;
    let mut y = group.y() + margin;
    let max_w = group.w();
    let mut max_h_in_row = 0;

    for i in 0..group.children() {
        if let Some(mut widget) = group.child(i) {
            if x + w + margin > group.x() + max_w {
                x = group.x() + margin;
                y += max_h_in_row + gap;
                max_h_in_row = 0;
            }

            widget.resize(x, y, w, h);

            x += w + gap;

            if h > max_h_in_row {
                max_h_in_row = h;
            }
        }
    }
}
