use enclose::enclose;
use fltk::{
    app, draw,
    enums::{Color, Event, Key},
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
}

#[derive(Clone, Debug)]
enum MpvEvent {
    Seek(f64),
    DragStart,
    DragEnd(f64),
    LoadFile(String),
    Stop,
}

fn main() {
    #[cfg(target_os = "linux")]
    unsafe {
        env::set_var("FLTK_BACKEND", "x11");
    }

    let (app_tx, app_rx) = app::channel::<AppHandleEvent>();
    let (mpv_tx, mpv_rx) = std::sync::mpsc::channel::<MpvEvent>();

    let app = app::App::default();

    let mut win = Window::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("test");

    let mut wizard = Wizard::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .center_of_parent();

    let menu_group = Group::default().with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT);
    let _menu_win = menu_window(app_tx);
    menu_group.end();

    let video_group = Group::default().with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT);
    let (video_layer,  mut progress_bar) = mpv_window(mpv_tx.clone());
    video_group.end();

    wizard.end();
    win.end();
    win.show();

    let mut mpv = Mpv::new().expect("Main MPV init failed");
    mpv.set_property("wid", video_layer.raw_handle() as i64)
        .unwrap();
    mpv_property(&mpv);

    let _mpv_handle = std::thread::spawn(enclose!((app_tx) move || {
        let mut total_dur: f64 = 0.;
        loop {
            if let Some(Ok(event)) = mpv.wait_event(0.1) {
                use libmpv2::events::Event::*;
                use libmpv2::events::PropertyData::Double;
                if let PropertyChange {
                        name: event_name,
                        change: Double(val),
                        ..
                    } = event {
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
                    }
                    Stop => {
                        mpv.command("stop", &[]).ok();
                    }
                }
            }
        }
    }));

    win.handle(enclose!((app_tx) move |_, ev| {
        if !matches!(ev, Event::KeyDown) {
            return false;
        }

        let key = app::event_key();
        match key {
            k if k == Key::from_char('q') => {
                app_tx.send(AppHandleEvent::GoToMenu);
            }
            k if k == Key::from_char(' ') => {

            }
            _ => ()
        }
        false
    }));

    mpv_tx
        .send(MpvEvent::LoadFile("/home/john/tmp/a.mp4".to_string()))
        .ok();

    while app.wait() {
        let Some(ev) = app_rx.recv() else {
            continue;
        };

        use AppHandleEvent::*;
        match ev {
            TimePosUpdated(new_time) => {
                progress_bar.set_value(new_time);
            },
            GoToVideo(_) => {
                wizard.set_current_widget(&video_group);
            },
            GoToMenu => {
                wizard.set_current_widget(&menu_group);
                mpv_tx.send(MpvEvent::Stop).ok();
            },
        }
    }
}

fn mpv_controls(tx: Sender<MpvEvent>) -> (Window, FlatProgressBar) {
    let mut controls = Window::default()
        .with_pos(0, INIT_WIN_HEIGHT - CONTROLS_HEIGHT)
        .with_size(CONTROLS_WIDTH, CONTROLS_HEIGHT)
        .with_label("");
    controls.set_color(Color::from_rgba(0, 0, 0, 150));
    controls.set_border(false);

    let mut progress_bar = FlatProgressBar::new(0, 0, INIT_WIN_WIDTH, CONTROLS_HEIGHT);
    let mut last_seek_time = Instant::now();
    let throttle_duration = Duration::from_millis(150);
    progress_bar.handle(enclose!((tx) move |w, ev, _| {
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

fn mpv_window(tx: Sender<MpvEvent>) -> (GlWindow, FlatProgressBar) {
    let mut video_layer = GlWindow::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("");
    video_layer.set_border(false);
    video_layer.end();

    let (_, progress_bar) = mpv_controls(tx.clone());

    (video_layer, progress_bar)
}

fn menu_window(tx: fltk::app::Sender<AppHandleEvent>) -> Window {
    let mut win = Window::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("MPV Linux Test");

    win.draw(enclose!(() move|w| {
        draw::draw_rect_fill(w.x(), w.y(), w.w(), w.h(), Color::from_rgb(70, 55, 30));
    }));

    win.end();
    win
}

#[inline]
fn mpv_property(mpv: &Mpv) {
    use libmpv2::Format::Double;

    #[cfg(target_os = "linux")]
    {
        mpv.set_property("gpu-api", "opengl").unwrap();
        mpv.set_property("gpu-context", "x11egl").unwrap();
        mpv.set_property("vo", "x11").unwrap();
    }

    mpv.set_property("hwdec", "auto").unwrap();

    mpv.observe_property("time-pos", Double, 0).unwrap();
    mpv.observe_property("duration", Double, 1).unwrap();
}
