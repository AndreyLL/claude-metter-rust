use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;

use chrono::{DateTime, FixedOffset, Utc};
use gdk::prelude::*;
use gdk::{EventMask, ScrollDirection};
use glib::ControlFlow;
use gtk::prelude::*;
use gtk::{EventBox, WindowType};
use serde::Deserialize;

// ── Constants ──────────────────────────────────────────────────────────────

const CREDENTIALS_PATH: &str = ".claude/.credentials.json";
const API_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const POLL_INTERVAL_SEC: u32 = 120;
const REQUEST_TIMEOUT_SEC: u64 = 10;

const BAR_WIDTH: f64 = 90.0;
const BAR_HEIGHT: f64 = 20.0;
const BAR_SPACING: f64 = 6.0;
const PADDING_H: f64 = 8.0;
const PADDING_V: f64 = 2.0;
const CORNER_RADIUS: f64 = 3.0;

const COLOR_GREEN: (f64, f64, f64) = (0.298, 0.686, 0.314);
const COLOR_YELLOW: (f64, f64, f64) = (1.0, 0.596, 0.0);
const COLOR_RED: (f64, f64, f64) = (0.957, 0.263, 0.212);
const COLOR_BLUE_TIME: (f64, f64, f64) = (0.26, 0.52, 0.96);
const COLOR_BG_TRACK: (f64, f64, f64, f64) = (0.3, 0.3, 0.3, 0.5);
const COLOR_BG_WINDOW: (f64, f64, f64, f64) = (0.15, 0.15, 0.15, 0.85);

const WINDOW_5H_SEC: f64 = 5.0 * 3600.0;
const WINDOW_7D_SEC: f64 = 7.0 * 24.0 * 3600.0;

// ── Data types ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: OAuthCreds,
}

#[derive(Deserialize)]
struct OAuthCreds {
    #[serde(rename = "accessToken")]
    access_token: String,
}

#[derive(Deserialize, Default, Debug)]
struct ApiResponse {
    five_hour: Option<WindowData>,
    seven_day: Option<WindowData>,
    seven_day_sonnet: Option<WindowData>,
    seven_day_opus: Option<WindowData>,
}

#[derive(Deserialize, Default, Debug, Clone)]
struct WindowData {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Default, Clone, Debug)]
struct UsageData {
    five_hour: Option<f64>,
    five_hour_resets: Option<String>,
    seven_day: Option<f64>,
    seven_day_resets: Option<String>,
    sonnet_7d: Option<f64>,
    sonnet_7d_resets: Option<String>,
    opus_7d: Option<f64>,
    opus_7d_resets: Option<String>,
    error: Option<String>,
}

#[derive(Clone)]
struct State {
    usage: UsageData,
    last_good: Option<UsageData>,
    scale: f64,
    drag_start: Option<(f64, f64)>,
    popup: Option<gtk::Window>,
}

impl State {
    fn new() -> Self {
        Self {
            usage: UsageData::default(),
            last_good: None,
            scale: 1.0,
            drag_start: None,
            popup: None,
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn get_bar_color(usage_pct: f64, time_pct: Option<f64>) -> (f64, f64, f64) {
    if let Some(tp) = time_pct {
        if tp > 1.0 {
            let projected = usage_pct / tp * 100.0;
            return if projected <= 80.0 {
                COLOR_GREEN
            } else if projected <= 100.0 {
                COLOR_YELLOW
            } else {
                COLOR_RED
            };
        }
    }
    if usage_pct <= 50.0 {
        COLOR_GREEN
    } else if usage_pct <= 80.0 {
        COLOR_YELLOW
    } else {
        COLOR_RED
    }
}

fn time_until(iso_str: &str) -> String {
    let Ok(reset) = DateTime::<FixedOffset>::parse_from_rfc3339(iso_str) else {
        return "?".into();
    };
    let now = Utc::now();
    let delta = reset.signed_duration_since(now);
    let total_sec = delta.num_seconds();
    if total_sec <= 0 {
        return "now".into();
    }
    let hours = total_sec / 3600;
    let minutes = (total_sec % 3600) / 60;
    if hours > 24 {
        let days = hours / 24;
        format!("{}d {}h", days, hours % 24)
    } else if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}

fn time_elapsed_pct(iso_str: &str, window_sec: f64) -> f64 {
    let Ok(reset) = DateTime::<FixedOffset>::parse_from_rfc3339(iso_str) else {
        return 0.0;
    };
    let now = Utc::now();
    let remaining = (reset.signed_duration_since(now)).num_seconds().max(0) as f64;
    let elapsed = window_sec - remaining;
    (elapsed / window_sec * 100.0).clamp(0.0, 100.0)
}

fn credentials_path() -> std::path::PathBuf {
    dirs_or_home().join(CREDENTIALS_PATH)
}

fn dirs_or_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
}

// ── API ────────────────────────────────────────────────────────────────────

fn read_access_token() -> Result<String, String> {
    let path = credentials_path();
    let content =
        std::fs::read_to_string(&path).map_err(|_| "no creds".to_string())?;
    let creds: Credentials =
        serde_json::from_str(&content).map_err(|_| "bad creds".to_string())?;
    Ok(creds.claude_ai_oauth.access_token)
}

fn fetch_usage() -> UsageData {
    let mut data = UsageData::default();

    let token = match read_access_token() {
        Ok(t) => t,
        Err(e) => {
            data.error = Some(e);
            return data;
        }
    };

    let resp = ureq::get(API_URL)
        .set("Authorization", &format!("Bearer {}", token))
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SEC))
        .call();

    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::Status(401, _)) => {
            data.error = Some("auth?".into());
            return data;
        }
        Err(ureq::Error::Status(code, _)) => {
            data.error = Some(format!("HTTP {}", code));
            return data;
        }
        Err(_) => {
            data.error = Some("network".into());
            return data;
        }
    };

    let body: ApiResponse = match resp.into_json() {
        Ok(b) => b,
        Err(_) => {
            data.error = Some("parse".into());
            return data;
        }
    };

    if let Some(five) = body.five_hour {
        data.five_hour = five.utilization;
        data.five_hour_resets = five.resets_at;
    }
    if let Some(seven) = body.seven_day {
        data.seven_day = seven.utilization;
        data.seven_day_resets = seven.resets_at;
    }
    if let Some(sonnet) = body.seven_day_sonnet {
        data.sonnet_7d = sonnet.utilization;
        data.sonnet_7d_resets = sonnet.resets_at;
    }
    if let Some(opus) = body.seven_day_opus {
        data.opus_7d = opus.utilization;
        data.opus_7d_resets = opus.resets_at;
    }

    data
}

// ── Drawing ────────────────────────────────────────────────────────────────

fn draw_rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -0.5 * PI, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, 0.5 * PI);
    cr.arc(x + r, y + h - r, r, 0.5 * PI, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

fn draw_bar(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    pct: f64,
    label: &str,
    time_pct: f64,
    font_size: f64,
    scale: f64,
) {
    // Track background
    draw_rounded_rect(cr, x, y, w, h, 2.0);
    cr.set_source_rgba(COLOR_BG_TRACK.0, COLOR_BG_TRACK.1, COLOR_BG_TRACK.2, COLOR_BG_TRACK.3);
    let _ = cr.fill();

    // Usage fill
    if pct > 0.0 {
        let fill_w = (w * pct.min(100.0) / 100.0).max(2.0);
        draw_rounded_rect(cr, x, y, fill_w, h, 2.0);
        let tp = if time_pct > 0.0 { Some(time_pct) } else { None };
        let color = get_bar_color(pct, tp);
        cr.set_source_rgba(color.0, color.1, color.2, 0.85);
        let _ = cr.fill();
    }

    // Time elapsed bar — blue, bottom
    if time_pct > 0.0 {
        let time_bar_h = (7.0 * scale).max(3.0);
        let time_fill_w = (w * time_pct.min(100.0) / 100.0).max(2.0);
        let time_y = y + h - time_bar_h;
        draw_rounded_rect(cr, x, time_y, time_fill_w, time_bar_h, 2.0);
        cr.set_source_rgba(COLOR_BLUE_TIME.0, COLOR_BLUE_TIME.1, COLOR_BLUE_TIME.2, 0.8);
        let _ = cr.fill();
    }

    // Text shadow
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
    cr.set_font_size(font_size);
    let extents = cr.text_extents(label).unwrap();
    let tx = x + (w - extents.width()) / 2.0;
    let ty = y + (h + extents.height()) / 2.0;

    cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
    cr.move_to(tx + 1.0, ty + 1.0);
    let _ = cr.show_text(label);

    cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    cr.move_to(tx, ty);
    let _ = cr.show_text(label);
}

// ── Popup ──────────────────────────────────────────────────────────────────

fn create_popup(
    usage: &UsageData,
    parent_x: i32,
    parent_y: i32,
    parent_h: i32,
    on_refresh: impl Fn() + 'static,
) -> gtk::Window {
    let popup = gtk::Window::new(WindowType::Popup);
    popup.set_decorated(false);
    popup.set_resizable(false);
    popup.set_keep_above(true);

    if let Some(screen) = gtk::prelude::WidgetExt::screen(&popup) {
        if let Some(visual) = screen.rgba_visual() {
            popup.set_visual(Some(&visual));
        }
    }
    popup.set_app_paintable(true);
    popup.connect_draw(|_, cr| {
        cr.set_source_rgba(0.15, 0.15, 0.15, 0.92);
        cr.set_operator(cairo::Operator::Source);
        let _ = cr.paint();
        cr.set_operator(cairo::Operator::Over);
        glib::Propagation::Proceed
    });

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_top(10);
    vbox.set_margin_bottom(10);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let add_label = |text: &str| {
        let lbl = gtk::Label::new(None);
        lbl.set_markup(&format!("<span foreground='white' font='Monospace 10'>{}</span>", glib::markup_escape_text(text)));
        lbl.set_xalign(0.0);
        lbl
    };

    if let Some(ref err) = usage.error {
        vbox.pack_start(&add_label(&format!("Error: {}", err)), false, false, 0);
    } else {
        if let Some(pct) = usage.five_hour {
            let reset = usage.five_hour_resets.as_deref().map(time_until).unwrap_or("?".into());
            vbox.pack_start(&add_label(&format!("5h usage:  {:.0}%  —  resets in {}", pct, reset)), false, false, 0);
        }
        if let Some(pct) = usage.seven_day {
            let reset = usage.seven_day_resets.as_deref().map(time_until).unwrap_or("?".into());
            vbox.pack_start(&add_label(&format!("7d usage:  {:.0}%  —  resets in {}", pct, reset)), false, false, 0);
        }
        if let Some(pct) = usage.sonnet_7d {
            let reset = usage.sonnet_7d_resets.as_deref().map(time_until).unwrap_or("?".into());
            vbox.pack_start(&add_label(&format!("Sonnet 7d: {:.0}%  —  resets in {}", pct, reset)), false, false, 0);
        }
        if let Some(pct) = usage.opus_7d {
            let reset = usage.opus_7d_resets.as_deref().map(time_until).unwrap_or("?".into());
            vbox.pack_start(&add_label(&format!("Opus 7d:   {:.0}%  —  resets in {}", pct, reset)), false, false, 0);
        }
    }

    let refresh_btn = gtk::Button::with_label("Refresh now");
    let popup_weak = {
        // We'll connect destroy after showing
        refresh_btn.connect_clicked(move |_| {
            on_refresh();
        });
        refresh_btn
    };
    vbox.pack_start(&popup_weak, false, false, 4);

    // Hint
    let hint = gtk::Label::new(None);
    hint.set_markup("<span foreground='#999999' font='Sans 8'>LMB drag — move  |  Scroll — resize</span>");
    hint.set_xalign(0.5);
    vbox.pack_start(&hint, false, false, 0);

    popup.add(&vbox);
    popup.show_all();
    popup.move_(parent_x, parent_y + parent_h + 2);

    popup
}

// ── Sizing ─────────────────────────────────────────────────────────────────

fn calc_size(scale: f64) -> (f64, f64, f64, f64, f64, f64, f64) {
    let bar_w = BAR_WIDTH * scale;
    let bar_h = BAR_HEIGHT * scale;
    let spacing = BAR_SPACING * scale;
    let pad_h = PADDING_H * scale;
    let pad_v = PADDING_V * scale;
    let total_w = pad_h + bar_w + spacing + bar_w + pad_h;
    let total_h = pad_v + bar_h + pad_v;
    (bar_w, bar_h, spacing, pad_h, pad_v, total_w, total_h)
}

// ── Main ───────────────────────────────────────────────────────────────────

fn main() {
    gtk::init().expect("Failed to init GTK");

    let state = Rc::new(RefCell::new(State::new()));

    let win = gtk::Window::new(WindowType::Popup);
    win.set_decorated(false);
    win.set_resizable(false);
    win.set_keep_above(true);

    if let Some(screen) = gtk::prelude::WidgetExt::screen(&win) {
        if let Some(visual) = screen.rgba_visual() {
            win.set_visual(Some(&visual));
        }
    }
    win.set_app_paintable(true);

    let (_, _, _, _, _, total_w, total_h) = calc_size(1.0);
    win.set_size_request(total_w as i32, total_h as i32);

    let drawing_area = gtk::DrawingArea::new();
    drawing_area.set_size_request(total_w as i32, total_h as i32);

    let event_box = EventBox::new();
    event_box.add(&drawing_area);
    win.add(&event_box);

    event_box.set_events(
        EventMask::BUTTON_PRESS_MASK
            | EventMask::BUTTON_RELEASE_MASK
            | EventMask::POINTER_MOTION_MASK
            | EventMask::SCROLL_MASK,
    );

    // Position: top-right of rightmost monitor
    let display = gdk::Display::default().expect("No display");
    let n_monitors = display.n_monitors();
    let mut rightmost_geom = display.monitor(0).unwrap().geometry();
    for i in 1..n_monitors {
        if let Some(m) = display.monitor(i) {
            let g = m.geometry();
            if g.x() > rightmost_geom.x() {
                rightmost_geom = g;
            }
        }
    }
    let start_x = rightmost_geom.x() + rightmost_geom.width() - total_w as i32 - 250;
    let start_y = rightmost_geom.y() + 28;
    win.move_(start_x, start_y);

    // ── Draw handler ───────────────────────────────────────────────────
    {
        let st = state.clone();
        drawing_area.connect_draw(move |_widget, cr| {
            let s = st.borrow();
            let (bar_w, bar_h, spacing, pad_h, pad_v, _tw, _th) = calc_size(s.scale);
            let alloc_w = _tw;
            let alloc_h = _th;

            // Clear
            cr.set_operator(cairo::Operator::Source);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
            let _ = cr.paint();
            cr.set_operator(cairo::Operator::Over);

            // Background
            draw_rounded_rect(cr, 0.0, 0.0, alloc_w, alloc_h, CORNER_RADIUS);
            cr.set_source_rgba(COLOR_BG_WINDOW.0, COLOR_BG_WINDOW.1, COLOR_BG_WINDOW.2, COLOR_BG_WINDOW.3);
            let _ = cr.fill();

            let font_size = (11.0 * s.scale).max(8.0);

            if let Some(ref err) = s.usage.error {
                if err != "stale" {
                    cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
                    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
                    cr.set_font_size(font_size);
                    let text = format!("Claude: {}", err);
                    let ext = cr.text_extents(&text).unwrap();
                    cr.move_to((alloc_w - ext.width()) / 2.0, (alloc_h + ext.height()) / 2.0);
                    let _ = cr.show_text(&text);
                    return glib::Propagation::Stop;
                }
            }

            let five_pct = s.usage.five_hour.unwrap_or(0.0);
            let seven_pct = s.usage.seven_day.unwrap_or(0.0);

            let five_time = s.usage.five_hour_resets.as_deref()
                .map(|r| time_elapsed_pct(r, WINDOW_5H_SEC))
                .unwrap_or(0.0);
            let seven_time = s.usage.seven_day_resets.as_deref()
                .map(|r| time_elapsed_pct(r, WINDOW_7D_SEC))
                .unwrap_or(0.0);

            draw_bar(cr, pad_h, pad_v, bar_w, bar_h, five_pct,
                     &format!("5h:{:.0}%", five_pct), five_time, font_size, s.scale);
            draw_bar(cr, pad_h + bar_w + spacing, pad_v, bar_w, bar_h, seven_pct,
                     &format!("7d:{:.0}%", seven_pct), seven_time, font_size, s.scale);

            // Stale indicator
            if s.usage.error.as_deref() == Some("stale") {
                cr.set_source_rgba(1.0, 0.8, 0.0, 0.9);
                cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
                cr.set_font_size(font_size);
                cr.move_to(alloc_w - 14.0 * s.scale, 13.0 * s.scale);
                let _ = cr.show_text("!");
            }

            glib::Propagation::Stop
        });
    }

    // ── Button press ───────────────────────────────────────────────────
    {
        let st = state.clone();
        let win_ref = win.clone();
        let da = drawing_area.clone();
        event_box.connect_button_press_event(move |_, event| {
            match event.button() {
                1 => {
                    // Left click — start drag
                    let mut s = st.borrow_mut();
                    s.drag_start = Some(event.root());
                }
                3 => {
                    // Right click — toggle popup
                    let mut s = st.borrow_mut();
                    if let Some(ref popup) = s.popup {
                        popup.close();
                        s.popup = None;
                    } else {
                        let (win_x, win_y) = win_ref.position();
                        let alloc_h = win_ref.allocated_height();
                        let usage = s.usage.clone();
                        let da_clone = da.clone();
                        let st_clone = st.clone();
                        let popup = create_popup(&usage, win_x, win_y, alloc_h, move || {
                            // Refresh callback
                            let new_usage = fetch_usage();
                            let mut s = st_clone.borrow_mut();
                            if new_usage.error.as_deref() == Some("network") {
                                if let Some(ref good) = s.last_good {
                                    s.usage = good.clone();
                                    s.usage.error = Some("stale".into());
                                }
                            } else {
                                if new_usage.error.is_none() {
                                    s.last_good = Some(new_usage.clone());
                                }
                                s.usage = new_usage;
                            }
                            da_clone.queue_draw();
                        });
                        popup.connect_destroy({
                            let st2 = st.clone();
                            move |_| {
                                st2.borrow_mut().popup = None;
                            }
                        });
                        s.popup = Some(popup);
                    }
                }
                _ => {}
            }
            glib::Propagation::Stop
        });
    }

    // ── Button release ─────────────────────────────────────────────────
    {
        let st = state.clone();
        event_box.connect_button_release_event(move |_, event| {
            if event.button() == 1 {
                st.borrow_mut().drag_start = None;
            }
            glib::Propagation::Stop
        });
    }

    // ── Motion (drag) ──────────────────────────────────────────────────
    {
        let st = state.clone();
        let win_ref = win.clone();
        event_box.connect_motion_notify_event(move |_, event| {
            let mut s = st.borrow_mut();
            if let Some((start_x, start_y)) = s.drag_start {
                let (rx, ry) = event.root();
                let dx = rx - start_x;
                let dy = ry - start_y;
                let (wx, wy) = win_ref.position();
                win_ref.move_(wx + dx as i32, wy + dy as i32);
                s.drag_start = Some((rx, ry));
            }
            glib::Propagation::Stop
        });
    }

    // ── Scroll (resize) ────────────────────────────────────────────────
    {
        let st = state.clone();
        let win_ref = win.clone();
        let da = drawing_area.clone();
        event_box.connect_scroll_event(move |_, event| {
            let mut s = st.borrow_mut();
            match event.direction() {
                ScrollDirection::Up => s.scale = (s.scale + 0.15).min(8.0),
                ScrollDirection::Down => s.scale = (s.scale - 0.15).max(0.5),
                _ => return glib::Propagation::Proceed,
            }
            let (_, _, _, _, _, tw, th) = calc_size(s.scale);
            da.set_size_request(tw as i32, th as i32);
            win_ref.set_size_request(tw as i32, th as i32);
            win_ref.resize(tw as i32, th as i32);
            da.queue_draw();
            glib::Propagation::Stop
        });
    }

    // ── Initial fetch ──────────────────────────────────────────────────
    {
        let mut s = state.borrow_mut();
        let new_usage = fetch_usage();
        if new_usage.error.is_none() {
            s.last_good = Some(new_usage.clone());
        }
        s.usage = new_usage;
    }

    // ── Timer ──────────────────────────────────────────────────────────
    {
        let st = state.clone();
        let da = drawing_area.clone();
        glib::timeout_add_seconds_local(POLL_INTERVAL_SEC, move || {
            let new_usage = fetch_usage();
            let mut s = st.borrow_mut();
            if new_usage.error.as_deref() == Some("network") {
                if let Some(ref good) = s.last_good {
                    s.usage = good.clone();
                    s.usage.error = Some("stale".into());
                }
            } else {
                if new_usage.error.is_none() {
                    s.last_good = Some(new_usage.clone());
                }
                s.usage = new_usage;
            }
            da.queue_draw();
            ControlFlow::Continue
        });
    }

    win.show_all();
    win.connect_destroy(|_| gtk::main_quit());
    gtk::main();
}
