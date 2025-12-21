use enclose::enclose;
use fltk::{
    app, draw,
    enums::{Color, Cursor, Event, Key},
    frame::Frame,
    group::{Group, Wizard},
    prelude::{GroupExt, WidgetBase, WidgetExt, WindowExt},
    window::{GlWindow, Window},
};
use libmpv2::Mpv;
use std::{
    env,
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use crate::ui::progress_bar::FlatProgressBar;

mod ui;

const INIT_WIN_WIDTH: i32 = 600;
const INIT_WIN_HEIGHT: i32 = 800;
const CONTROLS_WIDTH: i32 = INIT_WIN_WIDTH;
const CONTROLS_HEIGHT: i32 = 10;

#[derive(Clone, Debug)]
enum AppHandleEvent {
    TimePosUpdated(f64),
    GoToVideo(String),
    GoToMenu,
    FullScreen(Option<bool>),
    HideControl,
    ShowControl,
}

#[derive(Clone, Debug)]
enum MpvEvent {
    Seek(f64),
    DragStart,
    DragEnd(f64),
    LoadFile(String),
    TogglePause,
    Stop,
}

fn main() {
    #[cfg(target_os = "linux")]
    unsafe {
        env::set_var("FLTK_BACKEND", "x11");
    }

    let (app_tx, app_rx) = app::channel::<AppHandleEvent>();
    let (mpv_tx, mpv_rx) = std::sync::mpsc::channel::<MpvEvent>();

    let global = app::App::default();

    let mut win = Window::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("KK");
    win.make_resizable(true);

    let mut wizard = Wizard::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .center_of_parent();

    let menu_group = Group::default().with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT);
    let _menu_win = menu_window(app_tx);
    menu_group.end();

    let video_group = Group::default().with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT);
    let (video_layer, mut controls, mut progress_bar) = mpv_window(mpv_tx.clone());
    video_group.end();

    wizard.end();
    win.end();
    win.show();

    let mut mpv = Mpv::new().expect("Main MPV init failed");
    mpv.set_property("wid", video_layer.raw_handle() as i64)
        .unwrap();
    mpv_property(&mpv);

    let _mpv_handle = std::thread::spawn(enclose!((app_tx) move || {
        let mut is_pause = false;
        let mut total_dur: f64 = 0.;
        loop {
            if let Some(Ok(event)) = mpv.wait_event(0.1) {
                use libmpv2::events::Event::*;
                use libmpv2::events::PropertyData;
                match event {
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
                            "volume" => {

                            }
                            _ => ()
                        }
                    }
                    PropertyChange {
                        name: event_name,
                        change: PropertyData::Flag(val),
                        ..
                    } => {
                        match event_name {
                            "pause" => {
                                is_pause = val;
                            }
                            "core-idle" => {

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
                    Seek(pct) => {
                        let pct = (pct as i32).to_string();
                        mpv.command("seek", &[&pct, "absolute-percent", "exact"]).ok();
                    }
                    DragStart => {
                        mpv.set_property("pause", true).ok();
                    }
                    DragEnd(pct) => {
                        let pct = (pct as i32).to_string();
                        mpv.command("seek", &[&pct, "absolute-percent", "exact"]).ok();
                        mpv.set_property("pause", false).ok();
                    }
                    LoadFile(path) => {
                        mpv.command("loadfile", &[&path]).ok();
                        is_pause = false;
                    }
                    Stop => {
                        mpv.command("stop", &[]).ok();
                    }
                    TogglePause => {
                        mpv.set_property("pause", !is_pause).ok();
                    }
                }
            }
        }
    }));

    win.handle(enclose!((app_tx, mpv_tx) move |win, ev| {
        match ev {
            Event::KeyDown|Event::Shortcut => {
                let key = app::event_key();
                return match key {
                    Key::Enter => {
                        app_tx.send(AppHandleEvent::GoToVideo("/home/ckaznable/tmp/a.mp4".to_string()));
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
                    k if k == Key::from_char(' ') => {
                        mpv_tx.send(MpvEvent::TogglePause).ok();
                        true
                    }
                    k if k == Key::from_char('f') => {
                        app_tx.send(AppHandleEvent::FullScreen(None));
                        true
                    }
                    _ => false
                };
            }
            _ => false
        }
    }));

    let mut in_video = false;
    while global.wait() {
        let Some(ev) = app_rx.recv() else {
            continue;
        };

        use AppHandleEvent::*;
        match ev {
            TimePosUpdated(new_time) => {
                progress_bar.set_value(new_time);
            }
            GoToVideo(p) => {
                in_video = true;
                wizard.set_current_widget(&video_group);
                mpv_tx.send(MpvEvent::LoadFile(p)).ok();
            }
            GoToMenu => {
                wizard.set_current_widget(&menu_group);
                mpv_tx.send(MpvEvent::Stop).ok();
            }
            FullScreen(v) => {
                if !in_video {
                    continue;
                }

                let is_fullscreen = v.unwrap_or(!win.fullscreen_active());
                if is_fullscreen {
                    println!("f hide");
                    controls.hide();
                    win.set_cursor(Cursor::None);
                } else {
                    println!("f show");
                    controls.show();
                    win.set_cursor(Cursor::Default);
                }
                win.fullscreen(is_fullscreen);
            }
            HideControl => {
                println!("hide con");
                controls.hide();
                win.set_cursor(Cursor::None);
            }
            ShowControl => {
                println!("show con");
                controls.show();
                win.set_cursor(Cursor::Default);
            }
        }
    }
}

fn mpv_controls(tx: Sender<MpvEvent>) -> (Window, FlatProgressBar) {
    let mut controls = Window::default()
        .with_pos(0, INIT_WIN_HEIGHT - CONTROLS_HEIGHT)
        .with_size(CONTROLS_WIDTH, CONTROLS_HEIGHT)
        .with_label("");
    controls.make_resizable(true);
    controls.set_color(Color::from_rgba(0, 0, 0, 150));
    controls.set_border(false);

    let mut progress_bar = FlatProgressBar::new(0, 0, INIT_WIN_WIDTH, CONTROLS_HEIGHT);
    let mut last_seek_time = Instant::now();
    let throttle_duration = Duration::from_millis(150);
    progress_bar.handle(enclose!((tx) move |w, ev| {
        use fltk::enums::Event;

        let get_progress = |w: &Frame| {
            let mouse_x = app::event_x() - w.x();
            let pct = mouse_x as f64 / w.w() as f64;
            pct * 100.
        };

        match ev {
            Event::Push => {
                tx.send(MpvEvent::DragStart).ok();
                true
            }
            Event::Drag => {
                if last_seek_time.elapsed() >= throttle_duration {
                    tx.send(MpvEvent::Seek(get_progress(&w))).ok();
                    last_seek_time = Instant::now();
                    true
                } else {
                    false
                }
            }
            Event::Released => {
                tx.send(MpvEvent::DragEnd(get_progress(&w))).ok();
                true
            }
            _ => false
        }
    }));

    controls.end();
    (controls, progress_bar)
}

fn mpv_window(tx: Sender<MpvEvent>) -> (GlWindow, Window, FlatProgressBar) {
    let mut video_layer = GlWindow::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("");
    video_layer.make_resizable(true);
    video_layer.set_border(false);
    video_layer.end();

    let (controls, progress_bar) = mpv_controls(tx.clone());

    (video_layer, controls, progress_bar)
}

fn menu_window(tx: fltk::app::Sender<AppHandleEvent>) -> Window {
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
        mpv.set_property("vo", "x11").unwrap();
    }

    mpv.set_property("hwdec", "auto").unwrap();

    mpv.observe_property("time-pos", Format::Double, 0).unwrap();
    mpv.observe_property("duration", Format::Double, 1).unwrap();
    mpv.observe_property("pause", Format::Flag, 2).unwrap();
    mpv.observe_property("volume", Format::Double, 3).unwrap();
    mpv.observe_property("core-idle", Format::Flag, 4).unwrap();
}
