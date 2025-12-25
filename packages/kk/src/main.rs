use enclose::enclose;
use fltk::{
    app, draw,
    enums::{Color, Cursor, Event, Key, Mode},
    group::{Group, Wizard},
    prelude::{GroupExt, WidgetBase, WidgetExt, WindowExt},
    window::{GlWindow, Window},
};
use libmpv2::Mpv;
use serde_json::json;
use std::{cell::Cell, env, rc::Rc};

const INIT_WIN_WIDTH: i32 = 600;
const INIT_WIN_HEIGHT: i32 = 800;

#[derive(Clone, Debug)]
enum AppHandleEvent {
    TimePosUpdated(f64),
    GoToVideo(String, Option<Vec<f64>>),
    GoToMenu,
    FullScreen(Option<bool>),
    SetCusor(Cursor),
}

#[derive(Clone, Debug)]
enum MpvEvent {
    LoadFile(String),
    SetMarker(Vec<f64>),
    Stop,
    JumpNextMarker,
    TogglePause,
    TriggerMarkerSend,
}

fn main() {
    #[cfg(target_os = "linux")]
    unsafe {
        env::set_var("FLTK_BACKEND", "x11");
    }

    let (app_tx, app_rx) = app::channel::<AppHandleEvent>();
    let (mpv_tx, mpv_rx) = std::sync::mpsc::channel::<MpvEvent>();

    app::set_visual(Mode::Rgb | Mode::Alpha).unwrap();
    let app = app::App::default();

    let mut win = Window::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("KK");
    win.make_resizable(true);

    let mut wizard = Wizard::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .center_of_parent();

    let menu_group = Group::default().with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT);
    let _menu_win = menu_window();
    menu_group.end();

    let video_group = Group::default().with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT);
    let video_layer = mpv_window();
    video_group.end();

    wizard.end();
    win.end();
    win.show();

    let mut mpv = Mpv::new().expect("Main MPV init failed");
    mpv.set_property("wid", video_layer.raw_handle() as i64)
        .unwrap();
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
                }
            }
        }
    }));

    let in_video = Rc::new(Cell::new(false));
    win.handle(enclose!((app_tx, mpv_tx, in_video) move |_win, ev| {
        match ev {
            Event::KeyDown|Event::Shortcut => {
                let key = app::event_key();
                return match key {
                    Key::Enter => {
                        let marker: Vec<f64> = vec![10., 65.];
                        app_tx.send(AppHandleEvent::GoToVideo("/home/ckaznable/tmp/a.mp4".to_string(), Some(marker)));
                        true
                    }
                    Key::Escape => {
                        app_tx.send(AppHandleEvent::FullScreen(Some(false)));
                        true
                    }
                    k if k == Key::from_char('q') => {
                        app_tx.send(AppHandleEvent::GoToMenu);
                        true
                    }
                    k if k == Key::from_char('f') => {
                        app_tx.send(AppHandleEvent::FullScreen(None));
                        true
                    }
                    k if k == Key::from_char('n') && in_video.get() => {
                        mpv_tx.send(MpvEvent::JumpNextMarker).ok();
                        true
                    }
                    k if k == Key::from_char('m') && in_video.get() => {
                        mpv_tx.send(MpvEvent::TriggerMarkerSend).ok();
                        true
                    }
                    k if k == Key::from_char(' ') && in_video.get() => {
                        mpv_tx.send(MpvEvent::TogglePause).ok();
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
                wizard.set_current_widget(&menu_group);
                in_video.set(false);
                mpv_tx.send(MpvEvent::Stop).ok();
            }
            FullScreen(v) => {
                if !in_video.get() {
                    continue;
                }

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
        }
    }
}

fn mpv_window() -> GlWindow {
    let mut video_layer = GlWindow::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("");
    video_layer.make_resizable(true);
    video_layer.set_border(false);
    video_layer.end();

    video_layer
}

fn menu_window() -> Window {
    let mut win = Window::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("MPV Linux Test");
    win.make_resizable(true);

    win.draw(enclose!(() move|w| {
        draw::draw_rect_fill(w.x(), w.y(), w.w(), w.h(), Color::from_rgb(70, 55, 30));
    }));

    win.end();
    win
}

#[inline]
fn mpv_property(mpv: &Mpv) {
    use libmpv2::Format;

    #[cfg(target_os = "linux")]
    {
        mpv.set_property("gpu-api", "opengl").unwrap();
        mpv.set_property("gpu-context", "x11egl").unwrap();
    }

    mpv.set_property("hwdec", "auto").unwrap();

    mpv.observe_property("time-pos", Format::Double, 0).unwrap();
    mpv.observe_property("duration", Format::Double, 1).unwrap();
}
