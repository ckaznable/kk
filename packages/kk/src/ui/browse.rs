use enclose::enclose;
use itertools::Itertools;
use std::{cell::{Cell, RefCell}, path::{Path, PathBuf}, rc::Rc};

use fltk::{
    draw,
    enums::{Align, Color, Font, FrameType},
    frame::Frame,
    group::Group,
    image::SharedImage,
    prelude::{GroupExt, ImageExt, WidgetBase, WidgetExt},
};

use crate::ui::reflow_widgets;

const CONTAINER_MARGIN: i32 = 10;

const MENU_ITEM_HEIGHT: i32 = 300;
const MENU_ITEM_WIDTH: i32 = 350;

const MENU_IMG_HEIGHT: i32 = 220;
const MENU_IMG_WIDTH: i32 = 330;

const ITEM_GAP: i32 = 8;

type RenderItem = (PathBuf, String);

pub struct MenuItem;

impl MenuItem {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(img_path: &Path, content: &str, symbol: String) -> anyhow::Result<Group> {
        let mut img = SharedImage::load(img_path)?;
        img.scale(MENU_IMG_WIDTH, MENU_IMG_HEIGHT, true, true);

        let full_txt = content.to_string();
        let mut draw_img = img.clone();

        let mut item = Group::default().with_size(MENU_ITEM_WIDTH, MENU_ITEM_HEIGHT);
        item.set_frame(FrameType::NoBox);
        item.draw(move |w| {
            let img_y_fix = (MENU_IMG_HEIGHT - draw_img.height()) / 2;
            let img_x = w.x() + (MENU_ITEM_WIDTH - MENU_IMG_WIDTH) / 2;
            let img_y = w.y() + img_y_fix;

            draw_img.draw(img_x, img_y, MENU_IMG_WIDTH, MENU_IMG_HEIGHT);

            draw::set_draw_color(Color::White);
            draw::set_font(Font::Helvetica, 14);
            let txt_y = img_y + MENU_IMG_HEIGHT + 5 - img_y_fix;
            let max_w = (MENU_ITEM_WIDTH - 4) as f64;
            let line_height = 18;

            let mut line1 = String::new();
            let mut line2 = String::new();
            let mut remaining_text = full_txt.as_str();

            for (i, c) in full_txt.char_indices() {
                let end_idx = i + c.len_utf8();

                let current_slice = &full_txt[0..end_idx];

                if draw::width(current_slice) > max_w {
                    line1 = full_txt[0..i].to_string();
                    remaining_text = &full_txt[i..];
                    break;
                }

                if end_idx == full_txt.len() {
                    line1 = full_txt.clone();
                    remaining_text = "";
                }
            }

            if !remaining_text.is_empty() {
                if draw::width(remaining_text) > max_w {
                    let mut temp_line2 = remaining_text.to_string();
                    while !temp_line2.is_empty()
                        && draw::width(&format!("{}...", temp_line2)) > max_w
                    {
                        temp_line2.pop();
                    }
                    line2 = format!("{}...", temp_line2);
                } else {
                    line2 = remaining_text.to_string();
                }
            }

            if !line1.is_empty() {
                draw::draw_text2(
                    &line1,
                    w.x(),
                    txt_y,
                    MENU_ITEM_WIDTH,
                    line_height,
                    Align::Left,
                );
            }

            if !line2.is_empty() {
                draw::draw_text2(
                    &line2,
                    w.x(),
                    txt_y + line_height,
                    MENU_ITEM_WIDTH,
                    line_height,
                    Align::Left,
                );
            }

            draw::draw_text2(
                &format!("({symbol})"),
                w.x(),
                txt_y + line_height * 2,
                MENU_ITEM_WIDTH,
                line_height,
                Align::Center,
            );
        });
        item.end();

        Ok(item)
    }
}

#[derive(Clone)]
pub struct BrowseMenu {
    pub g: Group,
    items: Rc<RefCell<Vec<RenderItem>>>,
    page: Rc<Cell<usize>>,
    symbols: Rc<Vec<String>>,
    symbol: Rc<RefCell<String>>,
}

impl BrowseMenu {
    pub fn new(width: i32, height: i32) -> Self {
        let items = Rc::new(RefCell::new(vec![]));
        let page = Rc::new(Cell::new(1));

        let symbols_chars = "uiop";
        let n = symbols_chars.len();
        let symbols: Vec<String> = (1..=n)
            .flat_map(|len| {
                symbols_chars.chars()
                    .permutations(len)
                    .map(|chars| chars.into_iter().collect::<String>())
            })
            .collect();
        let symbols = Rc::new(symbols);
        let symbol = Rc::new(RefCell::new(String::from("")));

        let mut g = Group::default().with_size(width, height).with_pos(0, 0);

        let mut dummy = Frame::default().with_size(0, 0);
        dummy.hide();

        g.end();
        g.set_frame(FrameType::NoBox);
        g.resizable(&dummy);

        g.draw(|w| {
            draw::draw_rect_fill(w.x(), w.y(), w.w(), w.h(), Color::Black);
            w.draw_children();
        });

        g.resize_callback(enclose!((items, page, symbols, symbol) move |w, _x, _y, _width, _height| {
            Self::draw_items(w, &items.borrow(), page.get(), &symbols, &symbol.borrow());
        }));

        Self { g, items, page, symbols, symbol }
    }

    pub fn draw(&mut self) {
        Self::draw_items(&mut self.g, &self.items.borrow(), self.page.get(), &self.symbols,&self.symbol.borrow());
    }

    pub fn draw_items(g: &mut Group, items: &[RenderItem], page: usize, symbols: &[String], s: &str) {
        let page_size = Self::page_size(g);
        let page = page.min(items.len() / page_size + 1);

        g.clear();

        g.begin();
        items
            .iter()
            .skip(page_size * (page.saturating_sub(1)))
            .take(page_size)
            .enumerate()
            .filter_map(|(i, (p, c))| {
                let symbol = if i > symbols.len() {
                    &symbols[symbols.len() % i]
                } else {
                    &symbols[i]
                };

                if !s.is_empty() && !symbol.starts_with(s) {
                    return None;
                }

                Some((p, c, symbol.clone()))
            })
            .for_each(|(p, c, s)| {
                MenuItem::new(p, c, s).ok();
            });
        g.end();

        reflow_widgets(
            g,
            MENU_ITEM_WIDTH,
            MENU_ITEM_HEIGHT,
            CONTAINER_MARGIN,
            ITEM_GAP,
        );
        g.redraw();
    }

    pub fn set_item(&mut self, items: Vec<(PathBuf, String)>) {
        *self.items.borrow_mut() = items;
    }

    pub fn set_page(&mut self, page: usize) {
        let page_size = Self::page_size(&self.g);
        let page = page.min(self.items.borrow().len() / page_size + 1).max(1);
        self.page.set(page);
    }

    pub fn push_symbol(&self, ch: char) {
        self.symbol.borrow_mut().push(ch);
    }

    pub fn pop_symbol(&self) {
        self.symbol.borrow_mut().pop();
    }

    pub fn reset_symbol(&self) {
        self.symbol.borrow_mut().clear();
    }

    pub fn next_page(&mut self) {
        self.set_page(self.page.get() + 1);
    }

    pub fn prev_page(&mut self) {
        self.set_page(self.page.get().saturating_sub(1).max(1));
    }

    pub fn page_size(g: &Group) -> usize {
        let h = g.h();
        let clamp_h = h - CONTAINER_MARGIN * 2;
        let max_h_item_len = clamp_h / MENU_ITEM_HEIGHT;

        let w = g.w();
        let clamp_w = w - CONTAINER_MARGIN * 2;
        let max_w_item_len = clamp_w / MENU_ITEM_WIDTH;

        (max_w_item_len * max_h_item_len) as usize
    }
}
