# Burn-rate curve over usage bars

Add a yellow curve over each progress bar that shows token burn rate (%/hour) as it evolved through the current window.

## Semantics

- **X axis**: time within the window (same mapping as the existing blue time-elapsed bar: `x = (t - window_start) / window_sec * bar_width`).
- **Y axis**: burn rate in %/hour, auto-leveled per bar, no labels.
- One independent curve per bar (5h and 7d).
- Curve resets when `resets_at` changes (new window → empty buffer).

## Data model

```rust
struct RateSample { t: DateTime<Utc>, util: f64 }
struct RateBuffer { resets_at: String, samples: Vec<RateSample> }
```

State gains `rate_5h: Option<RateBuffer>` and `rate_7d: Option<RateBuffer>`.

All samples kept (no cap, no decimation at store time). Decimation applied only at draw time if `samples.len() > 2 * bar_width_px`.

## Persistence

File: `~/.config/claude-usage-monitor/history.json`. Both buffers serialized together. Loaded at startup, rewritten after each successful fetch.

On load, if a stored buffer's `resets_at` is already in the past (`<= now`), discard it — window has rolled over while the app was off.

## Update flow

After every successful `fetch_usage` call (startup, timer, manual refresh) for each window:

1. If `buffer` is `None` or `buffer.resets_at != new.resets_at` → replace with fresh `RateBuffer { resets_at, samples: [ {t: now, util} ] }`.
2. Else append `RateSample { t: now, util: new.util }`.
3. Save history to disk.

If API returned no data for a window (null), skip — keep existing buffer untouched.

## Rate calculation (at draw time)

Between consecutive samples `(s[i-1], s[i])`:

```
delta_util  = max(0, s[i].util - s[i-1].util)    // negatives clamped (noise)
delta_hours = (s[i].t - s[i-1].t).num_seconds() / 3600
rate[i]     = delta_util / delta_hours            // %/hour
```

Each `rate[i]` is positioned at the midpoint of `(s[i-1].t, s[i].t)` along the X axis.

## Auto-level

```
scale_max = max(MIN_RATE_SCALE, max(rate[]))
y(rate) = bar_y + bar_h - (rate / scale_max) * bar_h
```

- `MIN_RATE_SCALE_5H = 2.0` %/h
- `MIN_RATE_SCALE_7D = 0.2` %/h

## Rendering

New function `draw_rate_curve(cr, x, y, w, h, buffer, window_sec, min_scale, scale)`.

Called from inside `draw_bar` after the blue time bar is drawn and before the text. Order per bar:

1. Track background
2. Usage fill
3. Blue time-elapsed bar
4. **Rate curve** (new)
5. Text with shadow (existing — stays on top for readability)

Curve path:
- Compute `(x, y)` points. Skip pairs where `delta_hours == 0`.
- If `points.len() < 2` → skip drawing.
- Decimate to ~`2*w` points if needed.
- Build Cairo path with `curve_to` using Catmull-Rom → cubic Bezier conversion, tension 1/3, for smoothing.

Stroke twice over the same path:
1. Dark halo: `line_width = 2.5 * scale`, `rgba(0,0,0,0.6)`.
2. Yellow line: `line_width = 1.5 * scale`, `rgba(1.0, 0.85, 0.0, 0.95)`.

## Edge cases

| Condition | Behavior |
|---|---|
| API returns null for a window | Don't touch buffer, don't draw curve if buffer absent |
| Only 1 sample in buffer | Nothing to draw (no rate defined yet) |
| Δt = 0 between samples | Skip that pair |
| Δutil < 0 within window | Clamp to 0 (flat curve segment) |
| `resets_at` changed | Replace buffer with fresh single-sample buffer |
| History file corrupt or missing | Start with empty buffers, no error to user |

## Files touched

- `rust-gtk/src/main.rs`: constants, data structures, persistence helpers, rate-curve drawing, update calls in fetch paths.
- `CLAUDE.md`: document the new curve and `history.json` location.
