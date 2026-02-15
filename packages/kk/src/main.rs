use dirs::SEARCH_PATH;
use enclose::enclose;
use fltk::{
    app,
    enums::{Color, Cursor, Event, Key},
    group::{Group, Wizard},
    menu,
    prelude::{GroupExt, WidgetBase, WidgetExt, WindowExt},
    window::{GlWindow, Window},
};
use libmpv2::Mpv;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use crate::ui::browse::{BrowseMenu, MenuMode};

mod ui;

const INIT_WIN_WIDTH: i32 = 1280;
const INIT_WIN_HEIGHT: i32 = 720;

#[derive(Clone, Debug)]
enum AppHandleEvent {
    TimePosUpdated(f64),
    GoToVideo(String, usize, Vec<f64>, bool), // path, index, markers, is_webdav
    GoToMenu,
    FullScreen(Option<bool>),
    SetCusor(Cursor),
    AddMarker(f64),
    End,
}

#[derive(Clone, Debug)]
enum MpvEvent {
    LoadFile(String),
    SetMarker(Vec<f64>),
    Stop,
    JumpNextMarker,
    TogglePause,
    TriggerMarkerSend,
    MouseMove(i32, i32),
    MouseClick(i32, i32),
    SeekRelative(i32),
    VolumeAdjust(i32),
}

fn main() {
    let mut db = kr::init();
    db.load_config(&SEARCH_PATH).ok();
    let db = Rc::new(RefCell::new(db));

    let wd_db = kr::db::WebDavDatabase::new().expect("Failed to load WebDAV database");
    let wd_db = Rc::new(RefCell::new(wd_db));

    let (app_tx, app_rx) = app::channel::<AppHandleEvent>();
    let (mpv_tx, mpv_rx) = std::sync::mpsc::channel::<MpvEvent>();

    let app = app::App::default();

    let mut win = Window::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("KK");
    win.make_resizable(true);

    let mut wizard = Wizard::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .center_of_parent();

    let menu = BrowseMenu::new(INIT_WIN_WIDTH, INIT_WIN_HEIGHT);
    draw_menu_with_mode(menu.clone(), db.clone(), wd_db.clone(), MenuMode::AddedTime);

    let video_group = Group::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_pos(0, 0);
    let video_layer = mpv_window();
    video_group.end();

    wizard.end();
    wizard.set_current_widget(&video_group);

    win.end();
    win.show();

    let wid = video_layer.raw_handle() as i64;

    let mut mpv = Mpv::new().expect("Main MPV init failed");
    mpv.set_property("wid", wid).unwrap();
    mpv_property(&mpv);

    // load lua script
    let temp_lua = tempfile::Builder::new()
        .suffix(".lua")
        .tempfile()
        .expect("can't create tmpfile");
    std::fs::write(temp_lua.path(), include_str!("../lua/marker.lua"))
        .expect("write lua script failed");
    let lua_path = temp_lua.path().to_str().unwrap();
    mpv.command("load-script", &[lua_path])
        .expect("load script failed");

    let _mpv_handle = std::thread::spawn(enclose!((app_tx) move || {
        let mut total_dur: f64 = 0.;
        loop {
            if let Some(Ok(event)) = mpv.wait_event(0.1) {
                use libmpv2::events::Event::*;
                use libmpv2::events::PropertyData;
                match event {
                    ClientMessage(args) => {
                        if args.is_empty() {
                            return;
                        }

                        let event_name = args[0];
                        match event_name {
                            "ui_visibility_changed" => {
                                let visible = args[1] == "visible";
                                let cursor = if visible { Cursor::Default } else { Cursor::None };
                                app_tx.send(AppHandleEvent::SetCusor(cursor));
                            }
                            "rust_add_marker" => {
                                if args.len() > 1 {
                                    if let Ok(time) = args[1].parse::<f64>() {
                                        // Send add_marker event back so it can be handled on the main thread
                                        app_tx.send(AppHandleEvent::AddMarker(time));
                                    }
                                }
                            }
                            _ => {}
                        }
                        println!("{args:?}");
                    },
                    PropertyChange {
                        name: event_name,
                        change: PropertyData::Double(val),
                        ..
                    } => {
                        match event_name {
                            "time-pos" => {
                                if total_dur > 0. {
                                    app_tx.send(AppHandleEvent::TimePosUpdated(val / total_dur));
                                }
                            }
                            "duration" => {
                                total_dur = val;
                            }
                            _ => ()
                        }
                    }
                    _ => ()
                }
            }

            if let Ok(evt) = mpv_rx.try_recv() {
                use MpvEvent::*;
                match evt {
                    LoadFile(path) => {
                        mpv.command("loadfile", &[&path]).ok();
                    }
                    Stop => {
                        mpv.command("stop", &[]).ok();
                    }
                    SetMarker(m) => {
                        let json_data = json!(m).to_string();
                        mpv.command("script-message", &["update_markers", &json_data]).unwrap();
                    }
                    JumpNextMarker => {
                        mpv.command("script-message", &["jump_next_marker"]).ok();
                    }
                    TogglePause => {
                        mpv.command("cycle", &["pause"]).ok();
                    }
                    TriggerMarkerSend => {
                        mpv.command("script-message", &["trigger_marker_send"]).ok();
                    }
                    #[allow(unused)]
                    MouseMove(x, y) => {
                        #[cfg(target_os = "windows")]
                        mpv.command("mouse", &[&x.to_string(), &y.to_string()]).ok();
                    }
                    #[allow(unused)]
                    MouseClick(x, y) => {
                        #[cfg(target_os = "windows")]
                        mpv.command("mouse", &[&x.to_string(), &y.to_string(), "0", "single"]).ok();
                    }
                    SeekRelative(s) => {
                        mpv.command("seek", &[&s.to_string(), "relative"]).ok();
                    }
                    VolumeAdjust(delta) => {
                        mpv.command("add", &["volume", &delta.to_string()]).ok();
                        if let Ok(vol) = mpv.get_property::<i64>("volume") {
                            mpv.command("show-text", &[&format!("Volume: {}%", vol), "1000"]).ok();
                        }
                    }
                }
            }
        }
    }));

    let in_video = Rc::new(Cell::new(false));
    let mut mouse_event_throttle = 0u8;
    win.handle(enclose!((app_tx, mpv_tx, in_video, mut menu, db, wd_db) move |win, ev| {
        match ev {
            Event::Move => {
                mouse_event_throttle = if mouse_event_throttle > 3 {
                    0
                } else {
                    mouse_event_throttle + 1
                };

                if in_video.get() && mouse_event_throttle.is_multiple_of(3) {
                    let (x, y) = app::event_coords();
                    mpv_tx.send(MpvEvent::MouseMove(x, y)).ok();
                    win.set_cursor(Cursor::Default);
                    return true;
                }

                false
            },
            Event::Push => {
                let mouse_button = app::event_button();

                if in_video.get(){
                    if mouse_button == 3 {
                        // Right-click in video mode: exit to menu
                        app_tx.send(AppHandleEvent::GoToMenu);
                        return true;
                    } else if mouse_button == 2 {
                        // Middle-click: jump to next marker
                        mpv_tx.send(MpvEvent::JumpNextMarker).ok();
                        return true;
                    } else {
                        // Left-click: send to MPV
                        let (x, y) = app::event_coords();
                        mpv_tx.send(MpvEvent::MouseClick(x, y)).ok();
                        return true;
                    }
                } else {
                    // Handle mouse click in menu mode
                    let (x, y) = app::event_coords();

                    if mouse_button == 3 {
                        // Right-click on item: show context menu
                        if let Some(item_index) = menu.get_item_at_pos(x, y) {
                            let is_webdav = matches!(menu.current_mode(), MenuMode::WebDav);

                            if is_webdav {
                                let new_fav_status = wd_db.borrow_mut().toggle_fav(item_index as usize);
                                wd_db.borrow().flush();
                                redraw_menu_keep_page(menu.clone(), db.clone(), wd_db.clone(), menu.current_mode());
                                println!("WebDAV Item {} favorite status toggled: {}", item_index, new_fav_status);
                            } else {
                                let is_fav = db.borrow().get_movie(item_index as usize).map(|m| m.fav).unwrap_or(false);
                                let actors = db.borrow().get_actors(item_index as usize);

                                // Step 1: Show main context menu with static labels
                                let main_items = if is_fav {
                                    menu::MenuItem::new(&["Unfavorite", "Actors"])
                                } else {
                                    menu::MenuItem::new(&["Favorite", "Actors"])
                                };

                                if let Some(val) = main_items.popup(x, y) {
                                    let label = val.label().unwrap_or_default();
                                    if label == "Favorite" || label == "Unfavorite" {
                                        let new_fav_status = db.borrow_mut().toggle_fav(item_index as usize);
                                        db.borrow().flush();
                                        redraw_menu_keep_page(menu.clone(), db.clone(), wd_db.clone(), menu.current_mode());
                                        println!("Item {} favorite status toggled: {}", item_index, new_fav_status);
                                    } else if label == "Actors" {
                                        if actors.len() == 1 {
                                            // Single actor: directly filter
                                            let actor_mode = MenuMode::Actor(actors[0].clone());
                                            draw_menu_with_mode(menu.clone(), db.clone(), wd_db.clone(), actor_mode);
                                        } else if actors.len() > 1 {
                                            // Leak strings to get 'static refs for MenuItem::new
                                            let static_refs: Vec<&'static str> = actors.iter()
                                                .map(|s| &*Box::leak(s.clone().into_boxed_str()))
                                                .collect();
                                            let actor_menu = menu::MenuItem::new(&static_refs);
                                            if let Some(val) = actor_menu.popup(x, y) {
                                                if let Some(name) = val.label() {
                                                    let actor_mode = MenuMode::Actor(name);
                                                    draw_menu_with_mode(menu.clone(), db.clone(), wd_db.clone(), actor_mode);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            return true;
                        }
                    } else {
                        // Left-click handler
                        // First check if clicking on status bar to toggle mode
                        if menu.is_status_bar_click(x, y) {
                            draw_menu_with_mode(menu.clone(), db.clone(), wd_db.clone(), menu.next_mode());
                            return true;
                        }

                        // Otherwise, check if clicking on item to play video
                        if let Some(item_index) = menu.get_item_at_pos(x, y) {
                            if try_play_video(&db, &wd_db, item_index as usize, matches!(menu.current_mode(), MenuMode::WebDav), &app_tx) {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            Event::MouseWheel => {
                use fltk::app::MouseWheel;
                if in_video.get() {
                    // Scroll in video mode: seek forward/backward
                    match app::event_dy() {
                        MouseWheel::Up => {
                            mpv_tx.send(MpvEvent::SeekRelative(-5)).ok();
                        }
                        MouseWheel::Down => {
                            mpv_tx.send(MpvEvent::SeekRelative(5)).ok();
                        }
                        _ => {}
                    }
                    return true;
                } else {
                    // Scroll in menu mode: page navigation
                    match app::event_dy() {
                        MouseWheel::Up => {
                            menu.prev_page();
                            menu.draw();
                        }
                        MouseWheel::Down => {
                            menu.next_page();
                            menu.draw();
                        }
                        _ => {}
                    }
                    return true;
                }
            }
            Event::KeyDown|Event::Shortcut => {
                let key = app::event_key();
                return match key {
                    Key::Enter => {
                        if let Some(i) = menu.page_first_item_path() {
                            if try_play_video(&db, &wd_db, i as usize, matches!(menu.current_mode(), MenuMode::WebDav), &app_tx) {
                                return true;
                            }
                        }
                        false
                    }
                    Key::Escape => {
                        app_tx.send(AppHandleEvent::End);
                        true
                    }
                    Key::BackSpace => {
                        if matches!(menu.current_mode(), MenuMode::Actor(_)) {
                            draw_menu_with_mode(menu.clone(), db.clone(), wd_db.clone(), MenuMode::AddedTime);
                        } else {
                            menu.pop_symbol();
                            menu.draw();
                        }
                        true
                    }
                    k if k == Key::from_char('s') && !in_video.get() => {
                        // Toggle favorite status for the first item on the current page
                        if let Some(i) = menu.page_first_item_path() {
                            let is_webdav = matches!(menu.current_mode(), MenuMode::WebDav);
                            if is_webdav {
                                wd_db.borrow_mut().toggle_fav(i as usize);
                                wd_db.borrow().flush();
                            } else {
                                db.borrow_mut().toggle_fav(i as usize);
                                db.borrow().flush();
                            }
                            // Reload menu data to update the heart icon
                            redraw_menu_keep_page(menu.clone(), db.clone(), wd_db.clone(), menu.current_mode());
                        }
                        true
                    }
                    k if k == Key::from_char('q')  => {
                        app_tx.send(AppHandleEvent::GoToMenu);
                        true
                    }
                    k if k == Key::from_char('f') => {
                        app_tx.send(AppHandleEvent::FullScreen(None));
                        true
                    }
                    k if k == Key::from_char('n') => {
                        if in_video.get() {
                            mpv_tx.send(MpvEvent::JumpNextMarker).ok();
                        } else {
                            draw_menu_with_mode(menu.clone(), db.clone(), wd_db.clone(), menu.next_mode());
                        }
                        true
                    }
                    k if k == Key::from_char('h') => {
                        if in_video.get() {
                            mpv_tx.send(MpvEvent::SeekRelative(-5)).ok();
                        } else {
                            menu.prev_page();
                            menu.draw();
                        }
                        true
                    }
                    k if k == Key::from_char('l') => {
                        if in_video.get() {
                            mpv_tx.send(MpvEvent::SeekRelative(5)).ok();
                        } else {
                            menu.next_page();
                            menu.draw();
                        }

                        true
                    }
                    k if k == Key::from_char('m') => {
                        if in_video.get() {
                            mpv_tx.send(MpvEvent::TriggerMarkerSend).ok();
                        } else {
                            // Toggle to Favorites mode in menu
                            draw_menu_with_mode(menu.clone(), db.clone(), wd_db.clone(), MenuMode::Fav);
                        }
                        true
                    }
                    k if k == Key::from_char('u') && !in_video.get() => {
                        menu.push_symbol('u');
                        menu.draw();
                        true
                    }
                    k if k == Key::from_char('i') => {
                        if in_video.get() {
                            app_tx.send(AppHandleEvent::FullScreen(None));
                        } else {
                            menu.push_symbol('i');
                            menu.draw();
                        }
                        true
                    }
                    k if k == Key::from_char('o') => {
                        if in_video.get() {
                            app_tx.send(AppHandleEvent::GoToMenu);
                            menu.reset_symbol();
                            menu.draw();
                        } else {
                            menu.push_symbol('o');
                            menu.draw();
                        }

                        true
                    }
                    k if k == Key::from_char('p') && !in_video.get() => {
                        menu.push_symbol('p');
                        menu.draw();
                        true
                    }
                    k if k == Key::from_char('0') && !in_video.get() => {
                        menu.reset_symbol();
                        menu.draw();
                        true
                    }
                    k if k == Key::from_char(' ') && in_video.get() => {
                        mpv_tx.send(MpvEvent::TogglePause).ok();
                        true
                    }
                    Key::Up if in_video.get() => {
                        mpv_tx.send(MpvEvent::VolumeAdjust(5)).ok();
                        true
                    }
                    Key::Down if in_video.get() => {
                        mpv_tx.send(MpvEvent::VolumeAdjust(-5)).ok();
                        true
                    }
                    k if k == Key::from_char('b') && !in_video.get() => {
                        draw_menu_with_mode(menu.clone(), db.clone(), wd_db.clone(), menu.prev_mode());
                        true
                    }
                    _ => false
                };
            }
            _ => false
        }
    }));

    let current_movie_index: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));

    while app.wait() {
        let Some(ev) = app_rx.recv() else {
            continue;
        };

        use AppHandleEvent::*;
        match ev {
            TimePosUpdated(_new_time) => {}
            GoToVideo(p, movie_idx, markers, is_webdav) => {
                in_video.set(true);
                current_movie_index.set(Some(movie_idx));
                wizard.set_current_widget(&video_group);

                let stream_path = if is_webdav {
                    let wd_ref = wd_db.borrow();
                    let base_url = if !wd_ref.config.base_url.is_empty() {
                         wd_ref.config.base_url.clone()
                    } else {
                         dirs::WEBDAV_URL.clone().expect("WebDAV URL not configured")
                    };
                    let user = wd_ref.config.user.clone().or_else(|| dirs::WEBDAV_USER.clone());
                    let pass = wd_ref.config.pass.clone().or_else(|| dirs::WEBDAV_PASS.clone());
                    
                    let client = kwa::WebDavClient::new(&base_url, user.zip(pass)).expect("Failed to create WebDAV client");
                    client.get_stream_url(&p).expect("Failed to get stream URL")
                } else {
                    p
                };

                mpv_tx.send(MpvEvent::LoadFile(stream_path)).ok();
                mpv_tx.send(MpvEvent::SetMarker(markers)).ok();
            }
            GoToMenu => {
                wizard.set_current_widget(&menu.g);
                in_video.set(false);
                current_movie_index.set(None);
                mpv_tx.send(MpvEvent::Stop).ok();
                // Redraw menu to pick up any marker/fav changes made during video playback
                redraw_menu_keep_page(menu.clone(), db.clone(), wd_db.clone(), menu.current_mode());
            }
            FullScreen(v) => {
                let is_fullscreen = v.unwrap_or(!win.fullscreen_active());
                win.fullscreen(is_fullscreen);
            }
            SetCusor(cursor) => {
                if in_video.get() {
                    win.set_cursor(cursor);
                }
            }
            AddMarker(time) => {
                if let Some(idx) = current_movie_index.get() {
                    let is_webdav = matches!(menu.current_mode(), MenuMode::WebDav);
                    if is_webdav {
                         // TODO: implement markers for webdav if needed
                    } else {
                        let added = db.borrow_mut().add_marker(idx, time);
                        db.borrow().flush();
                        // Send updated markers to mpv for display
                        let markers = db.borrow().get_markers(idx);
                        mpv_tx.send(MpvEvent::SetMarker(markers)).ok();
                        if added {
                            println!("Marker added at {:.1}s for movie index {}", time, idx);
                        } else {
                            println!("Marker removed at {:.1}s for movie index {}", time, idx);
                        }
                    }
                }
            }
            End => break,
        }
    }
}

fn mpv_window() -> GlWindow {
    let mut video_layer = GlWindow::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("");
    video_layer.set_color(Color::Black);
    video_layer.make_resizable(true);
    video_layer.set_border(false);
    video_layer.end();

    video_layer
}

#[inline]
fn mpv_property(mpv: &Mpv) {
    use libmpv2::Format;

    mpv.set_property("hwdec", "auto").unwrap();

    mpv.observe_property("time-pos", Format::Double, 0).unwrap();
    mpv.observe_property("duration", Format::Double, 1).unwrap();
}

/// Try to find and play a video file for the movie at the given db index.
/// Returns true if a video file was found and the GoToVideo event was sent.
fn try_play_video(
    db: &Rc<RefCell<kr::db::SimpleJsonDatabase>>,
    wd_db: &Rc<RefCell<kr::db::WebDavDatabase>>,
    movie_index: usize,
    is_webdav: bool,
    app_tx: &app::Sender<AppHandleEvent>,
) -> bool {
    if is_webdav {
        let wd_ref = wd_db.borrow();
        if let Some(data) = wd_ref.get_movie(movie_index) {
            // Check if we have URL, if not check dirs
            let has_url = !wd_ref.config.base_url.is_empty() || dirs::WEBDAV_URL.is_some();
            if has_url {
                app_tx.send(AppHandleEvent::GoToVideo(
                    data.url_path.clone(),
                    movie_index,
                    data.markers.clone(),
                    true,
                ));
                return true;
            }
        }
        return false;
    }

    let db_ref = db.borrow();
    let Some(data) = db_ref.get_movie(movie_index) else {
        return false;
    };

    let parent = data.path.parent().unwrap();
    let filename = data.path.file_prefix().unwrap().to_str().unwrap();
    let markers = db_ref.get_markers(movie_index);

    for ext in [
        "mp4", "mkv", "avi", "rmvb", "wmv", "mov", "flv", "webm", "ts", "m4v", "3gp",
    ] {
        let p = parent.join(format!("{filename}.{ext}"));
        if p.exists() {
            println!("playing {:?}", &p);
            app_tx.send(AppHandleEvent::GoToVideo(
                p.to_string_lossy().to_string(),
                movie_index,
                markers,
                false,
            ));
            return true;
        }
    }

    println!("{parent:?} {filename} video file not found");
    false
}

/// Query menu items from the database based on the given mode.
fn query_menu_items(
    db: &Rc<RefCell<kr::db::SimpleJsonDatabase>>,
    wd_db: &Rc<RefCell<kr::db::WebDavDatabase>>,
    mode: &MenuMode,
) -> Vec<crate::ui::browse::RenderItem> {
    if matches!(mode, MenuMode::WebDav) {
        return wd_db.borrow().config.movies.iter().enumerate()
            .map(|(i, m)| (i, m).into())
            .collect();
    }

    let db_ref = db.borrow_mut();
    match mode {
        MenuMode::Actor(actor_name) => {
            let indices = db_ref.filter_by_actor(actor_name);
            indices
                .iter()
                .filter_map(|&i| {
                    db_ref.get_movie(i as usize).and_then(|movie| {
                        kr::db::IndexedMovieData { movie, index: i }.try_into().ok()
                    })
                })
                .collect()
        }
        _ => {
            drop(db_ref);
            let mut db_ref = db.borrow_mut();
            let iter = match mode {
                MenuMode::AddedTime => db_ref.order_by_added_time(),
                MenuMode::Random => db_ref.order_by_random(),
                MenuMode::Fav => db_ref.filter_by_fav(),
                MenuMode::Marked => db_ref.filter_by_marked(),
                MenuMode::Actor(_) => unreachable!(),
                MenuMode::WebDav => unreachable!(),
            };
            iter.flat_map(|item| item.try_into().ok()).collect()
        }
    }
}

fn draw_menu_with_mode(mut menu: BrowseMenu, db: Rc<RefCell<kr::db::SimpleJsonDatabase>>, wd_db: Rc<RefCell<kr::db::WebDavDatabase>>, mode: MenuMode) {
    menu.set_mode(mode.clone());
    let items = query_menu_items(&db, &wd_db, &mode);

    println!(
        "Mode: {} - Items count: {}",
        mode.display_name(),
        items.len()
    );

    menu.set_page(1);
    menu.set_item(items);
    menu.draw();
}

fn redraw_menu_keep_page(
    mut menu: BrowseMenu,
    db: Rc<RefCell<kr::db::SimpleJsonDatabase>>,
    wd_db: Rc<RefCell<kr::db::WebDavDatabase>>,
    mode: MenuMode,
) {
    let current_page = menu.current_page();
    let items = query_menu_items(&db, &wd_db, &mode);

    menu.set_page(current_page);
    menu.set_item(items);
    menu.draw();
}
