use enclose::enclose;
use fltk::{
    app,
    enums::{Color, Cursor, Event, Key},
    group::{Group, Wizard},
    prelude::{GroupExt, WidgetBase, WidgetExt, WindowExt},
    window::{GlWindow, Window},
};
use kr::db::SimpleJsonDatabase;
use libmpv2::Mpv;
use serde_json::json;
use std::{
    env,
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use crate::ui::browse::{BrowseMenu, MenuMode};

mod ui;

const INIT_WIN_WIDTH: i32 = 1280;
const INIT_WIN_HEIGHT: i32 = 720;

#[derive(Clone, Debug)]
enum AppHandleEvent {
    TimePosUpdated(f64),
    GoToVideo(String, Option<Vec<f64>>),
    GoToMenu,
    FullScreen(Option<bool>),
    SetCusor(Cursor),
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
}

fn main() {
    let search_path = env::var("KK_SEARCH_PATH").expect("KK_SEARCH_PATH env variable is required");
    let search_path = PathBuf::from(search_path);

    let mut db = kr::init();
    db.load_config(&search_path).ok();
    let db = Rc::new(RefCell::new(db));

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
    draw_menu_with_mode(menu.clone(), db.clone(), MenuMode::AddedTime);

    let video_group = Group::default().with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT).with_pos(0, 0);
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
                                // todo
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
                }
            }
        }
    }));

    let in_video = Rc::new(Cell::new(false));
    let mut mouse_event_throttle = 0u8;
    win.handle(enclose!((app_tx, mpv_tx, in_video, mut menu) move |win, ev| {
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
                if in_video.get(){
                    let (x, y) = app::event_coords();
                    mpv_tx.send(MpvEvent::MouseClick(x, y)).ok();
                    return true;
                }
                false
            }
            Event::KeyDown|Event::Shortcut => {
                let key = app::event_key();
                return match key {
                    Key::Enter => {
                        if let Some(p) = menu.page_first_item_path() {
                            let parent = p.parent().unwrap();
                            let filename = parent.file_name().unwrap().to_str().unwrap();
                            let filepath = std::fs::read_dir(parent)
                                .unwrap()
                                .filter_map(|e| e.ok())
                                .find(|e| {
                                    let name = e.file_name().to_string_lossy().to_string();
                                    if !name.starts_with(filename) {
                                        return false;
                                    }

                                    if let Some(ext) = e.path().extension() {
                                        ext == "mp4" || ext == "mkv" || ext == "avi" || ext == "rmvb"
                                    } else {
                                        false
                                    }
                                });

                            if let Some(filepath) = filepath {
                                let target_path = filepath.path().to_string_lossy().to_string();
                                println!("playing {}", &target_path);
                                app_tx.send(AppHandleEvent::GoToVideo(target_path, None));
                            }
                        }
                        true
                    }
                    Key::Escape => {
                        app_tx.send(AppHandleEvent::End);
                        true
                    }
                    Key::BackSpace => {
                        menu.pop_symbol();
                        menu.draw();
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
                            draw_menu_with_mode(menu.clone(), db.clone(), menu.next_mode());
                        }
                        true
                    }
                    k if k == Key::from_char('h') && !in_video.get() => {
                        menu.prev_page();
                        menu.draw();
                        true
                    }
                    k if k == Key::from_char('l') && !in_video.get() => {
                        menu.next_page();
                        menu.draw();
                        true
                    }
                    k if k == Key::from_char('m') && in_video.get() => {
                        mpv_tx.send(MpvEvent::TriggerMarkerSend).ok();
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
                    k if k == Key::from_char('b') && !in_video.get() => {
                        draw_menu_with_mode(menu.clone(), db.clone(), menu.prev_mode());
                        true
                    }
                    _ => false
                };
            }
            _ => false
        }
    }));

    while app.wait() {
        let Some(ev) = app_rx.recv() else {
            continue;
        };

        use AppHandleEvent::*;
        match ev {
            TimePosUpdated(_new_time) => {}
            GoToVideo(p, m) => {
                in_video.set(true);
                wizard.set_current_widget(&video_group);
                mpv_tx.send(MpvEvent::LoadFile(p)).ok();
                if let Some(m) = m {
                    mpv_tx.send(MpvEvent::SetMarker(m)).ok();
                }
            }
            GoToMenu => {
                wizard.set_current_widget(&menu.g);
                in_video.set(false);
                mpv_tx.send(MpvEvent::Stop).ok();
            }
            FullScreen(v) => {
                let is_fullscreen = v.unwrap_or(!win.fullscreen_active());
                win.fullscreen(is_fullscreen);

                if is_fullscreen {
                    win.set_cursor(Cursor::None);
                } else {
                    win.set_cursor(Cursor::Default);
                }
            }
            SetCusor(cursor) => {
                if in_video.get() {
                    win.set_cursor(cursor);
                }
            }
            End => {
                break
            }
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

fn draw_menu_with_mode(mut menu: BrowseMenu, db: Rc<RefCell<SimpleJsonDatabase>>, mode: MenuMode) {
    let mut db = db.borrow_mut();
    let iter = match mode {
        MenuMode::AddedTime => db.order_by_added_time(),
        MenuMode::Random => db.order_by_random(),
        MenuMode::Fav => db.filter_by_fav(),
    };

    menu.set_item(
            iter
            .flat_map(|item| {
                Some((
                    item.path.parent()?.join(item.movie.thumb.clone()?),
                    item.movie.title.clone(),
                ))
            })
            .collect(),
    );
    menu.draw();
}
