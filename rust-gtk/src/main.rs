use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, Utc};
use gdk::prelude::*;
use gdk::{EventMask, ScrollDirection};
use glib::ControlFlow;
use gtk::prelude::*;
use gtk::{EventBox, WindowType};
use serde::{Deserialize, Serialize};

// ── Constants ──────────────────────────────────────────────────────────────

const CREDENTIALS_PATH: &str = ".claude/.credentials.json";
const CONFIG_PATH: &str = ".config/claude-usage-monitor/config.json";
const HISTORY_PATH: &str = ".config/claude-usage-monitor/history.json";
const API_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const DEFAULT_POLL_MINUTES: u32 = 5;
const MIN_POLL_MINUTES: u32 = 1;
const MAX_POLL_MINUTES: u32 = 60;
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
const COLOR_CURVE: (f64, f64, f64) = (1.0, 0.85, 0.0);

const WINDOW_5H_SEC: f64 = 5.0 * 3600.0;
const WINDOW_7D_SEC: f64 = 7.0 * 24.0 * 3600.0;

const MIN_RATE_SCALE_5H: f64 = 2.0;  // %/hour autolevel floor
const MIN_RATE_SCALE_7D: f64 = 0.2;
const RATE_BIN_PX_7D: f64 = 5.0;  // 7d curve averaged into fixed-width bins

// Plasma background: computed on a tiny grid, upscaled bilinear — cost is scale-independent
const PLASMA_W: i32 = 64;
const PLASMA_H: i32 = 8;
const PLASMA_SPEED: f64 = 1.2;
const PLASMA_FPS: u64 = 30;

// Borderlands 2 backgrounds: GIF frames pre-cropped and pre-pixelated to exactly what is shown,
// raw BGRA (= cairo ARGB32 on little-endian). Drawn twice side by side, each copy ~4.2:1.
// desert: https://i.makeagif.com/media/3-29-2015/LDBuRn.gif
//   convert in.gif -coalesce -crop 320x76+0+52 +repage -scale 100x24! -modulate 95,140 -background black -alpha remove -alpha opaque -depth 8 bgra:bl2_desert_100x24.bgra
// psycho: https://64.media.tumblr.com/ff517781cf1e606406bf9b9b41a3b5ce/tumblr_prvhamlYFc1ut1d6co3_540.gifv (every 2nd frame)
//   convert in.gif -coalesce -delete $(seq -s, 1 2 39) -crop 540x128+0+86 +repage -scale 100x24! -modulate 95,140 -background black -alpha remove -alpha opaque -depth 8 bgra:bl2_psycho_100x24.bgra
const BL2_DESERT: &[u8] = include_bytes!("../assets/bl2_desert_100x24.bgra");
// zero: https://i.makeagif.com/media/9-22-2019/PSEoL5.gif (top strip)
//   convert in.gif -coalesce -crop 480x114+0+0 +repage -scale 100x24! -modulate 95,140 -background black -alpha remove -alpha opaque -depth 8 bgra:bl2_zero_100x24.bgra
const BL2_PSYCHO: &[u8] = include_bytes!("../assets/bl2_psycho_100x24.bgra");
const BL2_ZERO: &[u8] = include_bytes!("../assets/bl2_zero_100x24.bgra");
const BL2_W: i32 = 100;
const BL2_H: i32 = 24;
// Screen-door grid: one cell = GRID_CELL tile px, the last column/row is black.
// Stretched together with the frame, so line thickness grows with scale (≈1px at ~7x).
const GRID_CELL: i32 = 7;

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

#[derive(Deserialize, serde::Serialize, Clone, Copy, PartialEq, Debug, Default)]
#[serde(rename_all = "lowercase")]
enum Background {
    Off,
    #[default]
    Plasma,
    Desert,
    Psycho,
    Zero,
}

impl Background {
    fn next(self) -> Self {
        match self {
            Self::Off => Self::Plasma,
            Self::Plasma => Self::Desert,
            Self::Desert => Self::Psycho,
            Self::Psycho => Self::Zero,
            Self::Zero => Self::Off,
        }
    }

    // Frames and per-frame duration for GIF modes
    fn frames(self) -> Option<(&'static [u8], f64)> {
        match self {
            Self::Desert => Some((BL2_DESERT, 0.13)),
            Self::Psycho => Some((BL2_PSYCHO, 0.06)),
            Self::Zero => Some((BL2_ZERO, 0.2 / 3.0)),  // source delays 70/60/70 ms
            _ => None,
        }
    }

    // Redraw interval: GIF modes redraw only when the frame changes
    fn tick(self) -> Option<Duration> {
        match self {
            Self::Off => None,
            Self::Plasma => Some(Duration::from_millis(1000 / PLASMA_FPS)),
            _ => self.frames().map(|(_, sec)| Duration::from_secs_f64(sec)),
        }
    }
}

#[derive(Deserialize, serde::Serialize, Clone, Debug)]
#[serde(default)]
struct Config {
    poll_minutes: u32,
    background: Background,
}

impl Default for Config {
    fn default() -> Self {
        Self { poll_minutes: DEFAULT_POLL_MINUTES, background: Background::default() }
    }
}

fn config_path() -> std::path::PathBuf {
    dirs_or_home().join(CONFIG_PATH)
}

fn load_config() -> Config {
    let Ok(content) = std::fs::read_to_string(config_path()) else {
        return Config::default();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

fn save_config(cfg: &Config) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(&path, json);
    }
}

// ── Rate history ───────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize, Clone, Debug)]
struct RateSample {
    t: DateTime<Utc>,
    util: f64,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
struct RateBuffer {
    resets_at: String,
    samples: Vec<RateSample>,
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
struct History {
    rate_5h: Option<RateBuffer>,
    rate_7d: Option<RateBuffer>,
}

fn history_path() -> std::path::PathBuf {
    dirs_or_home().join(HISTORY_PATH)
}

fn buffer_is_stale(b: &RateBuffer) -> bool {
    match DateTime::<FixedOffset>::parse_from_rfc3339(&b.resets_at) {
        Ok(r) => r.with_timezone(&Utc) <= Utc::now(),
        Err(_) => true,
    }
}

fn load_history() -> History {
    let Ok(content) = std::fs::read_to_string(history_path()) else {
        return History::default();
    };
    let mut h: History = serde_json::from_str(&content).unwrap_or_default();
    if h.rate_5h.as_ref().map_or(false, buffer_is_stale) {
        h.rate_5h = None;
    }
    if h.rate_7d.as_ref().map_or(false, buffer_is_stale) {
        h.rate_7d = None;
    }
    h
}

fn save_history(h: &History) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(h) {
        let _ = std::fs::write(&path, json);
    }
}

/// API returns `resets_at` with sub-second jitter between calls even when the
/// logical window hasn't rolled over. Treat two timestamps as the same window
/// when they fall within ±2 minutes.
fn same_window(a: &str, b: &str) -> bool {
    let Ok(ta) = DateTime::<FixedOffset>::parse_from_rfc3339(a) else { return false };
    let Ok(tb) = DateTime::<FixedOffset>::parse_from_rfc3339(b) else { return false };
    (ta.timestamp() - tb.timestamp()).abs() <= 120
}

fn update_rate_buffer(
    buffer: &mut Option<RateBuffer>,
    util: Option<f64>,
    resets_at: Option<&str>,
) {
    let (Some(u), Some(r)) = (util, resets_at) else { return };
    let now = Utc::now();
    match buffer {
        Some(b) if same_window(&b.resets_at, r) => {
            b.samples.push(RateSample { t: now, util: u });
        }
        _ => {
            *buffer = Some(RateBuffer {
                resets_at: r.to_string(),
                samples: vec![RateSample { t: now, util: u }],
            });
        }
    }
}

struct State {
    usage: UsageData,
    last_good: Option<UsageData>,
    scale: f64,
    drag_start: Option<(f64, f64)>,
    popup: Option<gtk::Window>,
    config: Config,
    timer_source: Option<glib::SourceId>,
    anim_source: Option<glib::SourceId>,
    history: History,
}

impl State {
    fn new() -> Self {
        Self {
            usage: UsageData::default(),
            last_good: None,
            scale: 1.0,
            drag_start: None,
            popup: None,
            config: load_config(),
            timer_source: None,
            anim_source: None,
            history: load_history(),
        }
    }

    fn record_usage(&mut self, u: &UsageData) {
        update_rate_buffer(
            &mut self.history.rate_5h,
            u.five_hour,
            u.five_hour_resets.as_deref(),
        );
        update_rate_buffer(
            &mut self.history.rate_7d,
            u.seven_day,
            u.seven_day_resets.as_deref(),
        );
        save_history(&self.history);
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn lerp_rgb(a: (f64, f64, f64), b: (f64, f64, f64), t: f64) -> (f64, f64, f64) {
    let t = t.clamp(0.0, 1.0);
    (
        a.0 + (b.0 - a.0) * t,
        a.1 + (b.1 - a.1) * t,
        a.2 + (b.2 - a.2) * t,
    )
}

fn get_bar_color(usage_pct: f64, time_pct: Option<f64>) -> (f64, f64, f64) {
    // Driver: projected usage when the window has elapsed enough; otherwise
    // raw usage pct (at window start time_pct ≈ 0 and projection is noisy).
    let driver = time_pct
        .filter(|&tp| tp > 1.0)
        .map(|tp| usage_pct / tp * 100.0)
        .unwrap_or(usage_pct);

    if driver <= 80.0 {
        COLOR_GREEN
    } else if driver >= 100.0 {
        COLOR_RED
    } else if driver <= 90.0 {
        lerp_rgb(COLOR_GREEN, COLOR_YELLOW, (driver - 80.0) / 10.0)
    } else {
        lerp_rgb(COLOR_YELLOW, COLOR_RED, (driver - 90.0) / 10.0)
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

fn draw_plasma(cr: &cairo::Context, w: f64, h: f64, t: f64) {
    let t = t * PLASMA_SPEED;
    let stride = cairo::Format::ARgb32.stride_for_width(PLASMA_W as u32).unwrap();
    let mut data = vec![0u8; (stride * PLASMA_H) as usize];
    // Moving center for the radial ripple
    let cx = 4.0 + 3.5 * (t * 0.37).sin();
    let cy = 0.5 + (t * 0.5).cos();
    for j in 0..PLASMA_H {
        for i in 0..PLASMA_W {
            // Grid units: ~square cells, x in 0..8, y in 0..1
            let x = i as f64 / PLASMA_H as f64;
            let y = j as f64 / PLASMA_H as f64;
            let v = (x * 1.3 + t).sin()
                + (y * 3.0 + t * 1.7).sin()
                + (x * 0.8 + y * 2.0 + t * 0.9).sin()
                + (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() * 2.5 - t * 1.3).sin();
            // v in -4..4 → neon rainbow, hue drifts over time
            let ph = v * PI * 0.5 + t * 0.3;
            let c = |off: f64| ((0.5 + 0.5 * (ph + off).sin()) * 255.0) as u32;
            let px = 0xFF00_0000 | (c(0.0) << 16) | (c(2.1) << 8) | c(4.2);
            let o = (j * stride + i * 4) as usize;
            data[o..o + 4].copy_from_slice(&px.to_ne_bytes());
        }
    }
    let surf = cairo::ImageSurface::create_for_data(
        data, cairo::Format::ARgb32, PLASMA_W, PLASMA_H, stride,
    ).unwrap();

    cr.save().ok();
    draw_rounded_rect(cr, 0.0, 0.0, w, h, CORNER_RADIUS);
    cr.clip();
    cr.scale(w / PLASMA_W as f64, h / PLASMA_H as f64);
    let _ = cr.set_source_surface(&surf, 0.0, 0.0);
    cr.source().set_filter(cairo::Filter::Bilinear);
    cr.source().set_extend(cairo::Extend::Pad);
    let _ = cr.paint_with_alpha(COLOR_BG_WINDOW.3);
    cr.restore().ok();
}

// Grid for both halves rendered at widget size. Built once per size: the box-filtered
// downscale is expensive at small scales, too slow to redo every frame.
fn grid_surface(w: f64, h: f64) -> cairo::ImageSurface {
    let out = cairo::ImageSurface::create(cairo::Format::ARgb32, w.ceil() as i32, h.ceil() as i32).unwrap();
    let cr = cairo::Context::new(&out).unwrap();
    // Grid tile, repeated: 1 cell of the frame = GRID_CELL tile px
    let tile = cairo::ImageSurface::create(cairo::Format::ARgb32, GRID_CELL, GRID_CELL).unwrap();
    {
        let tc = cairo::Context::new(&tile).unwrap();
        tc.set_source_rgb(0.0, 0.0, 0.0);
        tc.rectangle((GRID_CELL - 1) as f64, 0.0, 1.0, GRID_CELL as f64);
        tc.rectangle(0.0, (GRID_CELL - 1) as f64, GRID_CELL as f64, 1.0);
        let _ = tc.fill();
    }
    tile.flush();
    let grid = cairo::SurfacePattern::create(&tile);
    grid.set_extend(cairo::Extend::Repeat);
    // Good = box filter on downscale: at small scale the grid fades to even dimming, no moiré
    grid.set_filter(cairo::Filter::Good);
    let mut m = cairo::Matrix::identity();
    m.scale(GRID_CELL as f64, GRID_CELL as f64);
    grid.set_matrix(m);

    let half = w / 2.0;
    for k in 0..2 {
        cr.save().ok();
        cr.translate(k as f64 * half, 0.0);
        cr.scale(half / BL2_W as f64, h / BL2_H as f64);
        cr.rectangle(0.0, 0.0, BL2_W as f64, BL2_H as f64);
        let _ = cr.set_source(&grid);
        let _ = cr.fill();
        cr.restore().ok();
    }
    drop(cr);
    out
}

fn draw_frames(cr: &cairo::Context, w: f64, h: f64, frames: &[u8], frame: usize, grid: &cairo::ImageSurface) {
    let size = (BL2_W * BL2_H * 4) as usize;
    let f = frame % (frames.len() / size);
    let surf = cairo::ImageSurface::create_for_data(
        frames[f * size..(f + 1) * size].to_vec(),
        cairo::Format::ARgb32, BL2_W, BL2_H, BL2_W * 4,
    ).unwrap();

    cr.save().ok();
    draw_rounded_rect(cr, 0.0, 0.0, w, h, CORNER_RADIUS);
    cr.clip();
    // Two copies side by side, each in frame-cell units
    let half = w / 2.0;
    for k in 0..2 {
        cr.save().ok();
        cr.translate(k as f64 * half, 0.0);
        cr.scale(half / BL2_W as f64, h / BL2_H as f64);
        let _ = cr.set_source_surface(&surf, 0.0, 0.0);
        cr.source().set_filter(cairo::Filter::Nearest);
        cr.rectangle(0.0, 0.0, BL2_W as f64, BL2_H as f64);
        let _ = cr.fill();
        cr.restore().ok();
    }
    let _ = cr.set_source_surface(grid, 0.0, 0.0);
    let _ = cr.paint();
    cr.restore().ok();
}

fn draw_rate_curve(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    buffer: &RateBuffer,
    window_sec: f64,
    min_scale: f64,
    scale: f64,
    bin_px: Option<f64>,
) {
    let Ok(reset) = DateTime::<FixedOffset>::parse_from_rfc3339(&buffer.resets_at) else {
        return;
    };
    let reset_utc = reset.with_timezone(&Utc);
    let window_start = reset_utc - chrono::Duration::seconds(window_sec as i64);

    let s = &buffer.samples;
    if s.len() < 2 {
        return;
    }

    // (normalized_t in [0,1], rate %/hour)
    let mut points: Vec<(f64, f64)> = Vec::with_capacity(s.len() - 1);
    for i in 1..s.len() {
        let dt_sec = (s[i].t - s[i - 1].t).num_milliseconds() as f64 / 1000.0;
        if dt_sec <= 0.0 {
            continue;
        }
        let du = (s[i].util - s[i - 1].util).max(0.0);
        let rate = du / (dt_sec / 3600.0);
        let mid = s[i - 1].t + (s[i].t - s[i - 1].t) / 2;
        let norm_raw =
            (mid - window_start).num_milliseconds() as f64 / (window_sec * 1000.0);
        // Clamp rather than filter — near rollover, slight clock skew between the
        // API's `resets_at` and our `now` can push fresh samples just outside
        // [0,1], and dropping them creates an empty curve on the very frame
        // where the user most wants to see the update.
        let norm = norm_raw.clamp(0.0, 1.0);
        points.push((norm, rate));
    }
    if points.len() < 2 {
        return;
    }

    let max_rate = points.iter().map(|p| p.1).fold(min_scale, f64::max);

    // Resampling strategy:
    //   - bin_px = Some(px): fixed-pixel-width bins, rates averaged inside each
    //     bin. Density grows with scale (more pixels → more bins → more detail).
    //   - bin_px = None: keep raw points, decimate by step_by if too dense,
    //     always preserving the last point.
    let drawn: Vec<(f64, f64)> = if let Some(px) = bin_px {
        let n_bins = ((w / px).round() as usize).max(2);
        let mut bins: Vec<(f64, usize)> = vec![(0.0, 0); n_bins];
        for &(t, r) in &points {
            let idx = ((t * n_bins as f64) as usize).min(n_bins - 1);
            bins[idx].0 += r;
            bins[idx].1 += 1;
        }
        bins.iter()
            .enumerate()
            .filter_map(|(i, &(sum, count))| {
                if count == 0 {
                    return None;
                }
                let center = (i as f64 + 0.5) / n_bins as f64;
                Some((center, sum / count as f64))
            })
            .collect()
    } else {
        let target = ((2.0 * w).max(16.0)) as usize;
        let step = (points.len() / target).max(1);
        if step > 1 {
            let mut d: Vec<(f64, f64)> = points.iter().step_by(step).copied().collect();
            if let Some(&last) = points.last() {
                if d.last() != Some(&last) {
                    d.push(last);
                }
            }
            d
        } else {
            points
        }
    };
    if drawn.len() < 2 {
        return;
    }

    // Screen coords
    let screen: Vec<(f64, f64)> = drawn
        .iter()
        .map(|(t, r)| {
            let sx = x + t * w;
            let sy = (y + h - (r / max_rate) * h).clamp(y, y + h);
            (sx, sy)
        })
        .collect();

    // Catmull-Rom → cubic Bezier path
    cr.new_path();
    cr.move_to(screen[0].0, screen[0].1);
    for i in 0..screen.len() - 1 {
        let p0 = if i == 0 { screen[0] } else { screen[i - 1] };
        let p1 = screen[i];
        let p2 = screen[i + 1];
        let p3 = if i + 2 < screen.len() {
            screen[i + 2]
        } else {
            screen[i + 1]
        };
        let c1x = p1.0 + (p2.0 - p0.0) / 6.0;
        let c1y = p1.1 + (p2.1 - p0.1) / 6.0;
        let c2x = p2.0 - (p3.0 - p1.0) / 6.0;
        let c2y = p2.1 - (p3.1 - p1.1) / 6.0;
        cr.curve_to(c1x, c1y, c2x, c2y, p2.0, p2.1);
    }

    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);

    cr.set_line_width((0.5 * scale).max(0.5));
    cr.set_source_rgba(COLOR_CURVE.0, COLOR_CURVE.1, COLOR_CURVE.2, 0.95);
    let _ = cr.stroke();
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
    rate_buffer: Option<&RateBuffer>,
    window_sec: f64,
    min_rate_scale: f64,
    rate_bin_px: Option<f64>,
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

    // Burn-rate curve — yellow, over fill/time-bar, under text
    if let Some(buf) = rate_buffer {
        draw_rate_curve(
            cr, x, y, w, h, buf, window_sec, min_rate_scale, scale, rate_bin_px,
        );
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
    poll_minutes: u32,
    on_refresh: impl Fn() + 'static,
    on_interval_change: impl Fn(i32) + 'static,
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
    refresh_btn.connect_clicked(move |_| {
        on_refresh();
    });
    vbox.pack_start(&refresh_btn, false, false, 4);

    // Refresh interval controls
    let interval_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    interval_row.set_halign(gtk::Align::Center);
    let interval_label = gtk::Label::new(None);
    interval_label.set_markup(&format!(
        "<span foreground='white' font='Sans 9'>Refresh every {} min</span>",
        poll_minutes
    ));
    let minus_btn = gtk::Button::with_label("−");
    let plus_btn = gtk::Button::with_label("+");
    minus_btn.set_size_request(32, -1);
    plus_btn.set_size_request(32, -1);

    let on_change = Rc::new(on_interval_change);
    {
        let on_change = on_change.clone();
        minus_btn.connect_clicked(move |_| on_change(-1));
    }
    {
        let on_change = on_change.clone();
        plus_btn.connect_clicked(move |_| on_change(1));
    }

    interval_row.pack_start(&minus_btn, false, false, 0);
    interval_row.pack_start(&interval_label, false, false, 6);
    interval_row.pack_start(&plus_btn, false, false, 0);
    vbox.pack_start(&interval_row, false, false, 2);

    // Quit button
    let quit_btn = gtk::Button::with_label("Quit");
    quit_btn.connect_clicked(|_| {
        gtk::main_quit();
    });
    vbox.pack_start(&quit_btn, false, false, 2);

    // Hint
    let hint = gtk::Label::new(None);
    hint.set_markup("<span foreground='#999999' font='Sans 8'>LMB drag — move  |  MMB — background  |  Scroll — resize</span>");
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

fn start_timer(state: &Rc<RefCell<State>>, da: &gtk::DrawingArea) {
    // Cancel existing timer if any
    if let Some(src) = state.borrow_mut().timer_source.take() {
        src.remove();
    }
    let interval_sec = state.borrow().config.poll_minutes * 60;
    let st = state.clone();
    let da_clone = da.clone();
    let source = glib::timeout_add_seconds_local(interval_sec, move || {
        let new_usage = fetch_usage();
        let mut s = st.borrow_mut();
        if new_usage.error.is_some() {
            if let Some(ref good) = s.last_good {
                s.usage = good.clone();
                s.usage.error = Some("stale".into());
            } else {
                s.usage = new_usage;
            }
        } else {
            s.record_usage(&new_usage);
            s.last_good = Some(new_usage.clone());
            s.usage = new_usage;
        }
        da_clone.queue_draw();
        ControlFlow::Continue
    });
    state.borrow_mut().timer_source = Some(source);
}

fn start_anim(state: &Rc<RefCell<State>>, da: &gtk::DrawingArea) {
    let mut s = state.borrow_mut();
    if let Some(src) = s.anim_source.take() {
        src.remove();
    }
    if let Some(tick) = s.config.background.tick() {
        let da = da.clone();
        s.anim_source = Some(glib::timeout_add_local(tick, move || {
            da.queue_draw();
            ControlFlow::Continue
        }));
    }
}

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
    let t0 = Instant::now();
    {
        let st = state.clone();
        let grid_cache: RefCell<Option<cairo::ImageSurface>> = RefCell::new(None);
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
            let t = t0.elapsed().as_secs_f64();
            match s.config.background {
                Background::Off => {
                    draw_rounded_rect(cr, 0.0, 0.0, alloc_w, alloc_h, CORNER_RADIUS);
                    cr.set_source_rgba(COLOR_BG_WINDOW.0, COLOR_BG_WINDOW.1, COLOR_BG_WINDOW.2, COLOR_BG_WINDOW.3);
                    let _ = cr.fill();
                }
                Background::Plasma => draw_plasma(cr, alloc_w, alloc_h, t),
                bg => {
                    let (frames, sec) = bg.frames().unwrap();
                    let mut grid = grid_cache.borrow_mut();
                    let size = (alloc_w.ceil() as i32, alloc_h.ceil() as i32);
                    if grid.as_ref().map(|g| (g.width(), g.height())) != Some(size) {
                        *grid = Some(grid_surface(alloc_w, alloc_h));
                    }
                    draw_frames(cr, alloc_w, alloc_h, frames, (t / sec) as usize, grid.as_ref().unwrap());
                }
            }

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
                     &format!("5h:{:.0}%", five_pct), five_time, font_size, s.scale,
                     s.history.rate_5h.as_ref(), WINDOW_5H_SEC, MIN_RATE_SCALE_5H, None);
            draw_bar(cr, pad_h + bar_w + spacing, pad_v, bar_w, bar_h, seven_pct,
                     &format!("7d:{:.0}%", seven_pct), seven_time, font_size, s.scale,
                     s.history.rate_7d.as_ref(), WINDOW_7D_SEC, MIN_RATE_SCALE_7D,
                     Some(RATE_BIN_PX_7D));

            // Stale indicator — red dot in top-right corner
            if s.usage.error.as_deref() == Some("stale") {
                let dot_r = 3.5 * s.scale;
                let dot_x = alloc_w - dot_r - 3.0 * s.scale;
                let dot_y = dot_r + 3.0 * s.scale;
                cr.arc(dot_x, dot_y, dot_r, 0.0, 2.0 * PI);
                cr.set_source_rgba(COLOR_RED.0, COLOR_RED.1, COLOR_RED.2, 0.95);
                let _ = cr.fill();
            }

            glib::Propagation::Stop
        });
    }

    // ── Background animation ───────────────────────────────────────────
    start_anim(&state, &drawing_area);

    // ── Button press ───────────────────────────────────────────────────
    {
        let st = state.clone();
        let win_ref = win.clone();
        let da = drawing_area.clone();
        event_box.connect_button_press_event(move |_, event| {
            match event.button() {
                1 => {
                    // Left click — start drag; remember pointer offset from window origin
                    let (rx, ry) = event.root();
                    let (wx, wy) = win_ref.position();
                    st.borrow_mut().drag_start = Some((rx - wx as f64, ry - wy as f64));
                }
                2 => {
                    // Middle click — cycle background
                    {
                        let mut s = st.borrow_mut();
                        s.config.background = s.config.background.next();
                        save_config(&s.config);
                    }
                    start_anim(&st, &da);
                    da.queue_draw();
                }
                3 => {
                    // Right click — toggle popup
                    let existing_popup = st.borrow_mut().popup.take();
                    if let Some(popup) = existing_popup {
                        unsafe { popup.destroy(); }
                        return glib::Propagation::Stop;
                    }
                    let mut s = st.borrow_mut();
                    {
                        let (win_x, win_y) = win_ref.position();
                        let alloc_h = win_ref.allocated_height();
                        let usage = s.usage.clone();
                        let poll_min = s.config.poll_minutes;

                        let refresh_cb = {
                            let st_clone = st.clone();
                            let da_clone = da.clone();
                            move || {
                                let new_usage = fetch_usage();
                                let mut s = st_clone.borrow_mut();
                                if new_usage.error.is_some() {
                                    if let Some(ref good) = s.last_good {
                                        s.usage = good.clone();
                                        s.usage.error = Some("stale".into());
                                    } else {
                                        s.usage = new_usage;
                                    }
                                } else {
                                    s.record_usage(&new_usage);
                                    s.last_good = Some(new_usage.clone());
                                    s.usage = new_usage;
                                }
                                da_clone.queue_draw();
                            }
                        };

                        let interval_cb = {
                            let st_clone = st.clone();
                            let da_clone = da.clone();
                            let win_ref2 = win_ref.clone();
                            move |delta: i32| {
                                let (new_min, old_popup) = {
                                    let mut s = st_clone.borrow_mut();
                                    let cur = s.config.poll_minutes as i32;
                                    let new_val = (cur + delta)
                                        .clamp(MIN_POLL_MINUTES as i32, MAX_POLL_MINUTES as i32)
                                        as u32;
                                    s.config.poll_minutes = new_val;
                                    save_config(&s.config);
                                    (new_val, s.popup.take())
                                };
                                // Destroy OUTSIDE the borrow — destroy signal fires
                                // connect_destroy synchronously, which re-borrows.
                                if let Some(popup) = old_popup {
                                    unsafe { popup.destroy(); }
                                }
                                start_timer(&st_clone, &da_clone);
                                // Reopen popup so user can continue clicking
                                let (wx, wy) = win_ref2.position();
                                let ah = win_ref2.allocated_height();
                                let usage = st_clone.borrow().usage.clone();
                                let refresh_cb2 = {
                                    let st2 = st_clone.clone();
                                    let da2 = da_clone.clone();
                                    move || {
                                        let new_usage = fetch_usage();
                                        let mut s = st2.borrow_mut();
                                        if new_usage.error.is_some() {
                                            if let Some(ref g) = s.last_good {
                                                s.usage = g.clone();
                                                s.usage.error = Some("stale".into());
                                            } else {
                                                s.usage = new_usage;
                                            }
                                        } else {
                                            s.record_usage(&new_usage);
                                            s.last_good = Some(new_usage.clone());
                                            s.usage = new_usage;
                                        }
                                        da2.queue_draw();
                                    }
                                };
                                let interval_cb2 = {
                                    let st3 = st_clone.clone();
                                    let da3 = da_clone.clone();
                                    let win3 = win_ref2.clone();
                                    move |d: i32| {
                                        let (new_val, old_popup) = {
                                            let mut s = st3.borrow_mut();
                                            let cur = s.config.poll_minutes as i32;
                                            let nv = (cur + d).clamp(MIN_POLL_MINUTES as i32, MAX_POLL_MINUTES as i32) as u32;
                                            s.config.poll_minutes = nv;
                                            save_config(&s.config);
                                            (nv, s.popup.take())
                                        };
                                        if let Some(p) = old_popup { unsafe { p.destroy(); } }
                                        start_timer(&st3, &da3);
                                        // Just close and let user reopen; avoid deep recursion
                                        let _ = new_val;
                                        let _ = win3;
                                    }
                                };
                                let new_popup = create_popup(&usage, wx, wy, ah, new_min, refresh_cb2, interval_cb2);
                                new_popup.connect_destroy({
                                    let st4 = st_clone.clone();
                                    move |_| { st4.borrow_mut().popup = None; }
                                });
                                st_clone.borrow_mut().popup = Some(new_popup);
                            }
                        };

                        let popup = create_popup(&usage, win_x, win_y, alloc_h, poll_min, refresh_cb, interval_cb);
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
            let s = st.borrow();
            // Absolute positioning: position() lags behind move_(), so
            // accumulating deltas onto it loses movement on fast drags.
            if let Some((off_x, off_y)) = s.drag_start {
                let (rx, ry) = event.root();
                win_ref.move_((rx - off_x).round() as i32, (ry - off_y).round() as i32);
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
                ScrollDirection::Up => s.scale = (s.scale + 0.15).min(20.0),
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
            s.record_usage(&new_usage);
            s.last_good = Some(new_usage.clone());
        }
        s.usage = new_usage;
    }

    // ── Timer ──────────────────────────────────────────────────────────
    start_timer(&state, &drawing_area);

    win.show_all();
    win.connect_destroy(|_| gtk::main_quit());
    gtk::main();
}
