use std::{cell::{Cell, RefCell}, rc::Rc};

use enclose::enclose;

use fltk::{
    app,
    draw,
    enums::{Color, Event, FrameType},
    frame::Frame,
    prelude::{WidgetBase, WidgetExt},
};

type CustomHandler = dyn FnMut(Frame, Event, ProgressRef) -> bool;

pub struct ProgressRef {
    pub val: Rc<Cell<f64>>,
    pub is_dragging: Rc<Cell<bool>>,
    pub marked: Rc<RefCell<Vec<f64>>>,
}

pub struct FlatProgressBar {
    wid: Frame,
    val: Rc<Cell<f64>>,
    marked: Rc<RefCell<Vec<f64>>>,
    is_dragging: Rc<Cell<bool>>,
    frame_handle: Rc<RefCell<Option<Box<CustomHandler>>>>,
}

impl FlatProgressBar {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        let mut wid = Frame::default().with_pos(x, y).with_size(w, h);
        wid.set_frame(FrameType::FlatBox);

        let val = Rc::new(Cell::new(0.0)); // 0.0 ~ 1.0
        let is_dragging =  Rc::new(Cell::new(false));
        let marked = Rc::new(RefCell::new(Vec::with_capacity(8)));
        let frame_handle: Rc<RefCell<Option<Box<CustomHandler>>>> = Rc::new(RefCell::new(None));

        wid.draw(enclose!((val, marked) {
            move |w| {
                draw::draw_rect_fill(w.x(), w.y(), w.w(), w.h(), Color::from_rgb(50, 50, 50));

                let progress = val.get(); // 0.0 ~ 1.0
                let fill_width = (w.w() as f64 * progress) as i32;

                if fill_width > 0 {
                    draw::draw_rect_fill(
                        w.x(),
                        w.y(),
                        fill_width,
                        w.h(),
                        Color::from_u32(0x3B8ED0),
                    );
                }

                marked
                    .borrow()
                    .iter()
                    .copied()
                    .for_each(|pos| {
                        draw::draw_rect_fill(
                            w.x() + (w.w() as f64 * pos) as i32,
                            w.y(),
                            1,
                            w.h(),
                            Color::from_rgb(255, 255, 0)
                        );
                    });
            }
        }));

        wid.handle(enclose!((val, is_dragging, frame_handle, marked) {
            move |w, ev| {
                let inner_handle = match ev {
                    Event::Push | Event::Drag => {
                        is_dragging.set(true);

                        let mouse_x = app::event_x() - w.x();
                        let width = w.w();

                        let pct = mouse_x as f64 / width as f64;
                        val.set(pct.clamp(0., 1.));

                        w.redraw();

                        true
                    }
                    Event::Released | Event::Leave => {
                        is_dragging.set(false);
                        true
                    }
                    _ => false,
                };

                if let Some(ref mut handler) = *frame_handle.borrow_mut() {
                    let custom_handle = handler(w.clone(), ev, ProgressRef {
                        val: val.clone(),
                        is_dragging: is_dragging.clone(),
                        marked: marked.clone(),
                    });

                    inner_handle || custom_handle
                } else {
                    inner_handle
                }
            }
        }));

        Self { wid, val, is_dragging, marked, frame_handle }
    }

    pub fn set_marked(&mut self, marked: &[f64]) {
        let mut m = self.marked.borrow_mut();
        m.clear();
        m.extend_from_slice(marked);
    }

    pub fn get_marked(&self) -> Rc<RefCell<Vec<f64>>> {
        self.marked.clone()
    }

    pub fn set_value(&mut self, value: f64) {
        if !self.is_dragging.get() {
            self.val.set(value.clamp(0.0, 1.0));
            self.wid.redraw();
        }
    }

    pub fn handle<F>(&mut self, f: F)
    where
        F: FnMut(Frame, Event, ProgressRef) -> bool + 'static
    {
        *self.frame_handle.borrow_mut() = Some(Box::new(f));
    }
}
