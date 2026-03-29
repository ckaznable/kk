use enclose::enclose;
use itertools::Itertools;
use kr::db::IndexedMovieData;
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use fltk::{
    draw,
    enums::{Align, Color, Font, FrameType},
    group::Group,
    image::SharedImage,
    prelude::{GroupExt, ImageExt, WidgetBase, WidgetExt},
};

use crate::ui::reflow_widgets;

const CONTAINER_MARGIN: i32 = 10;

const MENU_ITEM_HEIGHT: i32 = 260;
const MENU_ITEM_WIDTH: i32 = 350;

const MENU_IMG_HEIGHT: i32 = 208;
const MENU_IMG_WIDTH: i32 = 312;

const ITEM_GAP: i32 = 8;

#[derive(Default, Clone)]
pub enum MenuMode {
    #[default]
    AddedTime,
    Random,
    Fav,
    Marked,
    Actor(String),
    WebDav,
}

impl MenuMode {
    pub fn display_name(&self) -> String {
        match self {
            MenuMode::AddedTime => "Recent".to_string(),
            MenuMode::Random => "Random".to_string(),
            MenuMode::Fav => "Favorites".to_string(),
            MenuMode::Marked => "Marked".to_string(),
            MenuMode::Actor(name) => format!("Actor: {}", name),
            MenuMode::WebDav => "WebDAV".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct RenderItem {
    pub path: String, // NFO path or WebDAV relative path
    pub img_path: PathBuf,
    pub title: String,
    pub num: Option<String>, // Movie number / ID for fallback display
    pub index: u32,
    pub fav: bool,
    pub is_webdav: bool,
}

impl TryFrom<IndexedMovieData<'_>> for RenderItem {
    type Error = ();

    fn try_from(value: IndexedMovieData<'_>) -> Result<Self, Self::Error> {
        let nfo_path = value.movie.abs_path();
        let img_path = value.movie.abs_thumb_path().ok_or(())?;

        Ok(Self {
            path: nfo_path.to_string_lossy().to_string(),
            img_path,
            title: value.movie.movie.title.clone(),
            num: value.movie.movie.num.clone(),
            index: value.index,
            fav: value.movie.fav,
            is_webdav: false,
        })
    }
}

impl From<(usize, &kr::db::WebDavMovieData)> for RenderItem {
    fn from(value: (usize, &kr::db::WebDavMovieData)) -> Self {
        let (index, data) = value;
        let img_path = data.abs_thumb_path().unwrap_or_default();
        Self {
            path: data.url_path.clone(),
            img_path,
            title: data.movie.title.clone(),
            num: data.movie.num.clone(),
            index: index as u32,
            fav: data.fav,
            is_webdav: true,
        }
    }
}

pub struct MenuItem;

impl MenuItem {
    pub fn new(item: RenderItem, symbol: String) {
        let img_result = SharedImage::load(&item.img_path);

        let full_txt = item.title.clone();
        let is_fav = item.fav;
        // Fallback label: prefer movie num, otherwise use title
        let fallback_label = item.num.clone().unwrap_or_else(|| item.title.clone());

        let mut g = Group::default().with_size(MENU_ITEM_WIDTH, MENU_ITEM_HEIGHT);
        g.set_frame(FrameType::NoBox);

        match img_result {
            Ok(mut img) => {
                img.scale(MENU_IMG_WIDTH, MENU_IMG_HEIGHT, true, true);
                let mut draw_img = img.clone();
                g.draw(move |w| {
                    let img_y_fix = (MENU_IMG_HEIGHT - draw_img.height()) / 2;
                    let img_x = w.x() + (MENU_ITEM_WIDTH - MENU_IMG_WIDTH) / 2;
                    let img_y = w.y() + img_y_fix;

                    draw_img.draw(img_x, img_y, MENU_IMG_WIDTH, MENU_IMG_HEIGHT);

                    // Draw favorite heart icon only when favorited
                    if is_fav {
                        let heart_size = 20;
                        let heart_x = img_x + MENU_IMG_WIDTH - heart_size - 5;
                        let heart_y = img_y + 5;
                        draw::set_draw_color(Color::from_rgb(220, 53, 69));
                        draw::set_font(Font::Helvetica, heart_size);
                        draw::draw_text2(
                            "♥",
                            heart_x,
                            heart_y,
                            heart_size,
                            heart_size,
                            Align::Center,
                        );
                    }

                    draw::set_draw_color(Color::White);
                    draw::set_font(Font::Helvetica, 14);
                    let txt_y = img_y + MENU_IMG_HEIGHT + 5 - img_y_fix;
                    Self::draw_title_lines(&full_txt, w.x(), txt_y, MENU_ITEM_WIDTH);

                    let line_height = 18;
                    draw::draw_text2(
                        &format!("({symbol})"),
                        w.x(),
                        txt_y + line_height * 2,
                        MENU_ITEM_WIDTH,
                        line_height,
                        Align::Center,
                    );
                });
            }
            Err(_) => {
                // Fallback: render a dark placeholder with the movie num as large text
                g.draw(move |w| {
                    let img_x = w.x() + (MENU_ITEM_WIDTH - MENU_IMG_WIDTH) / 2;
                    let img_y = w.y();

                    // Dark gray background for the image area
                    draw::draw_rect_fill(
                        img_x,
                        img_y,
                        MENU_IMG_WIDTH,
                        MENU_IMG_HEIGHT,
                        Color::from_rgb(40, 40, 40),
                    );

                    // Draw movie num / title as large centered text
                    let label_font_size = 22;
                    draw::set_draw_color(Color::from_rgb(200, 200, 200));
                    draw::set_font(Font::HelveticaBold, label_font_size);
                    draw::draw_text2(
                        &fallback_label,
                        img_x,
                        img_y,
                        MENU_IMG_WIDTH,
                        MENU_IMG_HEIGHT,
                        Align::Center,
                    );

                    // Draw favorite heart if needed
                    if is_fav {
                        let heart_size = 20;
                        let heart_x = img_x + MENU_IMG_WIDTH - heart_size - 5;
                        let heart_y = img_y + 5;
                        draw::set_draw_color(Color::from_rgb(220, 53, 69));
                        draw::set_font(Font::Helvetica, heart_size);
                        draw::draw_text2(
                            "♥",
                            heart_x,
                            heart_y,
                            heart_size,
                            heart_size,
                            Align::Center,
                        );
                    }

                    draw::set_draw_color(Color::White);
                    draw::set_font(Font::Helvetica, 14);
                    let txt_y = img_y + MENU_IMG_HEIGHT + 5;
                    Self::draw_title_lines(&full_txt, w.x(), txt_y, MENU_ITEM_WIDTH);

                    let line_height = 18;
                    draw::draw_text2(
                        &format!("({symbol})"),
                        w.x(),
                        txt_y + line_height * 2,
                        MENU_ITEM_WIDTH,
                        line_height,
                        Align::Center,
                    );
                });
            }
        }

        g.end();
    }

    /// Render up to two lines of title text, truncating with "..." if needed.
    fn draw_title_lines(full_txt: &str, x: i32, y: i32, width: i32) {
        let max_w = (width - 4) as f64;
        let line_height = 18;

        let mut line1 = String::new();
        let mut line2 = String::new();
        let mut remaining_text = full_txt;

        for (i, c) in full_txt.char_indices() {
            let end_idx = i + c.len_utf8();
            let current_slice = &full_txt[0..end_idx];
            if draw::width(current_slice) > max_w {
                line1 = full_txt[0..i].to_string();
                remaining_text = &full_txt[i..];
                break;
            }
            if end_idx == full_txt.len() {
                line1 = full_txt.to_string();
                remaining_text = "";
            }
        }

        if !remaining_text.is_empty() {
            if draw::width(remaining_text) > max_w {
                let mut temp = remaining_text.to_string();
                while !temp.is_empty() && draw::width(&format!("{}...", temp)) > max_w {
                    temp.pop();
                }
                line2 = format!("{}...", temp);
            } else {
                line2 = remaining_text.to_string();
            }
        }

        if !line1.is_empty() {
            draw::draw_text2(&line1, x, y, width, line_height, Align::Left);
        }
        if !line2.is_empty() {
            draw::draw_text2(&line2, x, y + line_height, width, line_height, Align::Left);
        }
    }
}

#[derive(Clone)]
pub struct BrowseMenu {
    pub g: Group,
    items: Rc<RefCell<Vec<RenderItem>>>,
    page: Rc<Cell<usize>>,
    last_page: Rc<Cell<usize>>,
    last_mode: Rc<RefCell<MenuMode>>,
    symbols: Rc<Vec<String>>,
    symbol: Rc<RefCell<String>>,
    page_index_list: Rc<RefCell<Vec<u32>>>,
    mode: Rc<RefCell<MenuMode>>,
}

impl BrowseMenu {
    pub fn new(width: i32, height: i32) -> Self {
        let items = Rc::new(RefCell::new(vec![]));
        let page = Rc::new(Cell::new(1));
        let last_page = Rc::new(Cell::new(1));
        let last_mode = Rc::new(RefCell::new(MenuMode::default()));

        let symbols_chars = "uiop";
        let n = symbols_chars.len();
        let symbols: Vec<String> = (1..=n)
            .flat_map(|len| {
                symbols_chars
                    .chars()
                    .permutations(len)
                    .map(|chars| chars.into_iter().collect::<String>())
            })
            .collect();
        let symbols = Rc::new(symbols);
        let symbol = Rc::new(RefCell::new(String::from("")));
        let page_path_list = Rc::new(RefCell::new(vec![]));

        let mut g = Group::default().with_size(width, height).with_pos(0, 0);

        g.end();
        g.set_frame(FrameType::NoBox);

        g.draw(|w| {
            draw::draw_rect_fill(w.x(), w.y(), w.w(), w.h(), Color::Black);
            w.draw_children();
        });

        let mode = Rc::new(RefCell::new(MenuMode::default()));

        g.resize_callback(enclose!((items, page, symbols, symbol, page_path_list, mode) move |w, _x, _y, _width, _height| {
            let page_size = Self::page_size(w);
            let total_items = items.borrow().len();
            let total_pages = if total_items == 0 { 1 } else { (total_items + page_size - 1) / page_size };
            *page_path_list.borrow_mut() = Self::draw_items(w, &items.borrow(), page.get(), &symbols, &symbol.borrow(), mode.borrow().clone(), total_pages);
        }));

        Self {
            g,
            items,
            page,
            last_page,
            last_mode,
            symbols,
            symbol,
            page_index_list: page_path_list,
            mode,
        }
    }

    pub fn draw(&mut self) {
        let mode = self.mode.borrow().clone();
        let page = self.page.get();
        let page_size = Self::page_size(&self.g);
        let total_items = self.items.borrow().len();
        let total_pages = if total_items == 0 {
            1
        } else {
            (total_items + page_size - 1) / page_size
        };

        *self.page_index_list.borrow_mut() = Self::draw_items(
            &mut self.g,
            &self.items.borrow(),
            page,
            &self.symbols,
            &self.symbol.borrow(),
            mode,
            total_pages,
        );
    }

    pub fn draw_items(
        g: &mut Group,
        items: &[RenderItem],
        page: usize,
        symbols: &[String],
        s: &str,
        mode: MenuMode,
        total_pages: usize,
    ) -> Vec<u32> {
        let page_size = Self::page_size(g);
        let page = page.min(items.len() / page_size + 1);

        g.clear();
        g.begin();

        let plist: Vec<u32> = items
            .iter()
            .skip(page_size * (page.saturating_sub(1)))
            .take(page_size)
            .enumerate()
            .filter_map(|(i, item)| {
                let symbol = if i > symbols.len() {
                    &symbols[symbols.len() % i]
                } else {
                    &symbols[i]
                };

                if !s.is_empty() && !symbol.starts_with(s) {
                    return None;
                }

                Some((item, symbol.clone()))
            })
            .map(|(item, s)| {
                MenuItem::new(item.clone(), s);
                item.index
            })
            .collect();

        g.end();

        reflow_widgets(
            g,
            MENU_ITEM_WIDTH,
            MENU_ITEM_HEIGHT,
            CONTAINER_MARGIN,
            ITEM_GAP,
        );

        // Draw status bar at the bottom
        let status_bar_height = 30;
        let status_text = format!(
            "Page {}/{} | Mode: {}",
            page,
            total_pages,
            mode.display_name()
        );

        g.begin();
        let mut status_frame = fltk::frame::Frame::default()
            .with_size(g.w(), status_bar_height)
            .with_pos(g.x(), g.y() + g.h() - status_bar_height);
        status_frame.set_label(&status_text);
        status_frame.set_label_color(Color::White);
        status_frame.set_label_size(14);
        g.end();

        g.redraw();

        plist
    }

    pub fn next_mode(&self) -> MenuMode {
        use MenuMode::*;
        let mode = match *self.mode.borrow() {
            AddedTime => Random,
            Random => Fav,
            Fav => Marked,
            Marked => AddedTime,
            WebDav => AddedTime, // If current is WebDav, go back to start
            Actor(_) => AddedTime,
        };

        *self.mode.borrow_mut() = mode.clone();
        mode
    }

    pub fn current_mode(&self) -> MenuMode {
        self.mode.borrow().clone()
    }

    pub fn set_mode(&self, mode: MenuMode) {
        if matches!(mode, MenuMode::Actor(_)) {
            *self.last_mode.borrow_mut() = self.mode.borrow().clone();
            self.last_page.set(self.page.get());
            self.page.set(1);
        }
        *self.mode.borrow_mut() = mode;
    }

    pub fn last_mode(&self) -> MenuMode {
        self.last_mode.borrow().clone()
    }

    pub fn restore_last_page(&self) {
        self.page.set(self.last_page.get());
    }

    pub fn prev_mode(&self) -> MenuMode {
        use MenuMode::*;
        let mode = match *self.mode.borrow() {
            AddedTime => Marked,
            Random => AddedTime,
            Fav => Random,
            Marked => Fav,
            WebDav => Marked,
            Actor(_) => AddedTime,
        };

        *self.mode.borrow_mut() = mode.clone();
        mode
    }

    pub fn set_item(&mut self, items: Vec<RenderItem>) {
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

    pub fn current_page(&self) -> usize {
        self.page.get()
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

    pub fn page_first_item_path(&self) -> Option<u32> {
        self.page_index_list.borrow().first().cloned()
    }

    /// Get the item index at the given position (x, y)
    pub fn get_item_at_pos(&self, x: i32, y: i32) -> Option<u32> {
        // Check if the click is within the menu group
        if x < self.g.x()
            || x > self.g.x() + self.g.w()
            || y < self.g.y()
            || y > self.g.y() + self.g.h()
        {
            return None;
        }

        // Calculate the grid dimensions
        let w = self.g.w();
        let clamp_w = w - CONTAINER_MARGIN * 2;
        let max_w_item_len = clamp_w / MENU_ITEM_WIDTH;

        // Calculate relative position from the menu group origin
        let rel_x = x - self.g.x() - CONTAINER_MARGIN;
        let rel_y = y - self.g.y() - CONTAINER_MARGIN;

        if rel_x < 0 || rel_y < 0 {
            return None;
        }

        // Calculate which column and row was clicked
        let col = rel_x / MENU_ITEM_WIDTH;
        let row = rel_y / MENU_ITEM_HEIGHT;

        // Calculate the index in the current page
        let idx = (row * max_w_item_len + col) as usize;

        // Get the item from page_index_list
        self.page_index_list.borrow().get(idx).cloned()
    }

    /// Check if the click is on the status bar
    pub fn is_status_bar_click(&self, x: i32, y: i32) -> bool {
        let status_bar_height = 30;
        let status_bar_y = self.g.y() + self.g.h() - status_bar_height;

        x >= self.g.x()
            && x <= self.g.x() + self.g.w()
            && y >= status_bar_y
            && y <= self.g.y() + self.g.h()
    }
}
