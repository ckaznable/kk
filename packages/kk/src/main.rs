use enclose::enclose;
use fltk::{
    app::{self, TimeoutHandle}, draw,
    enums::{Color, Cursor, Event, Key},
    frame::Frame,
    group::{Group, Wizard},
    prelude::{GroupExt, WidgetBase, WidgetExt, WindowExt},
    window::{GlWindow, Window},
};
use libmpv2::Mpv;
use std::{
    cell::Cell, env, rc::Rc, time::{Duration, Instant}
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
    HideControlInFullScreen,
    ShowControlInFullScreen,
}

#[derive(Clone, Debug)]
enum MpvEvent {
    Seek(f64),
    DragStart,
    DragEnd(f64),
    LoadFile(String),
    TogglePause(Option<bool>),
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
        .with_label("KK");
    win.make_resizable(true);

    let mut wizard = Wizard::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .center_of_parent();

    let menu_group = Group::default().with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT);
    let _menu_win = menu_window();
    menu_group.end();

    let video_group = Group::default().with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT);
    let (video_layer, mut controls, mut progress_bar) = mpv_window();
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
                    TogglePause(pause) => {
                        if let Some(p) = pause {
                            mpv.set_property("pause", p).ok();
                        } else {
                            mpv.set_property("pause", !is_pause).ok();
                        }
                    }
                }
            }
        }
    }));

    let mut last_seek_time = Instant::now();
    let throttle_duration = Duration::from_millis(150);
    let hide_controls_timeout_handle: Rc<Cell<Option<TimeoutHandle>>> = Rc::new(Cell::new(None));
    progress_bar.handle(enclose!((mpv_tx, app_tx) move |w, ev| {
        use fltk::enums::Event;

        let get_progress = |w: &Frame| {
            let mouse_x = app::event_x() - w.x();
            let pct = mouse_x as f64 / w.w() as f64;
            pct * 100.
        };

        match ev {
            Event::Push => {
                mpv_tx.send(MpvEvent::DragStart).ok();
                true
            }
            Event::Drag => {
                if last_seek_time.elapsed() >= throttle_duration {
                    mpv_tx.send(MpvEvent::Seek(get_progress(&w))).ok();
                    last_seek_time = Instant::now();
                    true
                } else {
                    false
                }
            }
            Event::Released => {
                mpv_tx.send(MpvEvent::DragEnd(get_progress(&w))).ok();
                true
            }
            Event::Move => {
                if w.visible() {
                    return false;
                }

                let Some(timeout_handle) = hide_controls_timeout_handle.get() else {
                    return false;
                };

                app_tx.send(AppHandleEvent::ShowControlInFullScreen);
                app::remove_timeout3(timeout_handle);
                let handle = app::add_timeout3(1., enclose!((app_tx) move |_| {
                    app_tx.send(AppHandleEvent::HideControlInFullScreen);
                }));
                hide_controls_timeout_handle.set(Some(handle));

                true
            }
            _ => false
        }
    }));

    win.handle(enclose!((app_tx, mpv_tx, progress_bar) move |_win, ev| {
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
                        mpv_tx.send(MpvEvent::TogglePause(None)).ok();
                        true
                    }
                    k if k == Key::from_char('f') => {
                        app_tx.send(AppHandleEvent::FullScreen(None));
                        true
                    }
                    k if k == Key::from_char('n') => {
                        if let Some(mark) = progress_bar.next_mark() {
                            mpv_tx.send(MpvEvent::Seek(mark * 100.)).ok();
                        }

                        true
                    }
                    k if k == Key::from_char('m') => {
                        progress_bar.add_mark_with_current_timepos();
                        true
                    }
                    _ => false
                };
            }
            _ => false
        }
    }));

    let mut in_video = false;
    while app.wait() {
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
                    controls.hide();
                    win.set_cursor(Cursor::None);
                } else {
                    controls.show();
                    win.set_cursor(Cursor::Default);
                }
                win.fullscreen(is_fullscreen);
            }
            HideControlInFullScreen => {
                if win.fullscreen_active() {
                    controls.hide();
                    win.set_cursor(Cursor::None);
                }
            }
            ShowControlInFullScreen => {
                if win.fullscreen_active() {
                    controls.show();
                    win.set_cursor(Cursor::Default);
                }
            }
        }
    }
}

fn mpv_controls() -> (Window, FlatProgressBar) {
    let mut controls = Window::default()
        .with_pos(0, INIT_WIN_HEIGHT - CONTROLS_HEIGHT)
        .with_size(CONTROLS_WIDTH, CONTROLS_HEIGHT)
        .with_label("");
    controls.make_resizable(true);
    controls.set_color(Color::from_rgba(0, 0, 0, 150));
    controls.set_border(false);

    let progress_bar = FlatProgressBar::new(0, 0, INIT_WIN_WIDTH, CONTROLS_HEIGHT);
    controls.end();
    (controls, progress_bar)
}

fn mpv_window() -> (GlWindow, Window, FlatProgressBar) {
    let mut video_layer = GlWindow::default()
        .with_size(INIT_WIN_WIDTH, INIT_WIN_HEIGHT)
        .with_label("");
    video_layer.make_resizable(true);
    video_layer.set_border(false);
    video_layer.end();

    let (controls, progress_bar) = mpv_controls();

    (video_layer, controls, progress_bar)
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
        mpv.set_property("vo", "x11").unwrap();
    }

    mpv.set_property("hwdec", "auto").unwrap();

    mpv.observe_property("time-pos", Format::Double, 0).unwrap();
    mpv.observe_property("duration", Format::Double, 1).unwrap();
    mpv.observe_property("pause", Format::Flag, 2).unwrap();
    mpv.observe_property("volume", Format::Double, 3).unwrap();
    mpv.observe_property("core-idle", Format::Flag, 4).unwrap();
}
