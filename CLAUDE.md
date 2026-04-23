# Claude Usage Monitor

Desktop widget that displays Claude Code API usage limits as progress bars, positioned on the GNOME top panel area.

## What this app does

Shows two progress bars with real-time Claude Code subscription usage:
- **5h bar** — 5-hour rolling window utilization (current session)
- **7d bar** — 7-day weekly limit utilization

Each bar has three layers:
- **Main bar** (colored) — usage percentage (0–100%)
- **Blue bar** (7px, bottom) — time elapsed in the window
- **Yellow curve** — burn rate (%/hour) over time within the window, auto-leveled per bar. X axis = time in the window (synced with the blue bar), Y axis = rate with per-bar min floor (2 %/h for 5h, 0.2 %/h for 7d). Rate history persists across restarts in `~/.config/claude-usage-monitor/history.json`; resets when the window rolls over.

### Burn-rate coloring

The main bar color reflects projected usage at end of window, not just current percentage:
- **Green** — projected <= 80% (plenty of room)
- **Yellow** — projected 80–100% (tight, will barely fit)
- **Red** — projected > 100% (will run out before window resets)

Formula: `projected = usage_pct / time_elapsed_pct * 100`

### Popup details (left click)

Shows per-limit breakdown with reset times:
- 5h usage with time until reset
- 7d usage with time until reset
- Sonnet 7d (if available)
- Opus 7d (if available)
- "Refresh now" button
- Hint about controls

### Interactions

- **Left-click drag** — reposition the widget
- **Right click** — toggle popup with details
- **Scroll wheel** — resize (scale 0.5x to 8.0x)

## Data source

**Endpoint:** `GET https://api.anthropic.com/api/oauth/usage`

Headers:
- `Authorization: Bearer <accessToken>`
- `anthropic-beta: oauth-2025-04-20`

**Credentials:** `~/.claude/.credentials.json` field `claudeAiOauth.accessToken`

This is an undocumented endpoint used internally by Claude Code. It returns utilization percentages (0–100) for 5-hour and 7-day windows, plus per-model breakdowns (Sonnet, Opus) and reset timestamps.

**Response shape:**
```json
{
  "five_hour": { "utilization": 25.0, "resets_at": "2026-04-14T17:00:00+00:00" },
  "seven_day": { "utilization": 23.0, "resets_at": "2026-04-19T12:00:00+00:00" },
  "seven_day_sonnet": { "utilization": 0.0, "resets_at": "..." },
  "seven_day_opus": null
}
```

**Poll interval:** every 300 seconds (5 minutes). Token is re-read from disk on each request (Claude Code may refresh it externally).

## Target environment

- Ubuntu, GNOME Shell 42+, X11
- Multi-monitor setup (positions on rightmost monitor by default)
- Panel height: 27px (GNOME default)

## Window type

Uses `Gtk.WindowType.POPUP` (override_redirect) to render above GNOME panel. Positioned at y=28 (just below panel) so mouse events are not intercepted by GNOME Shell.

## Error handling

| Condition | Display |
|---|---|
| Normal | Colored bars with percentages |
| Network error | Last known values + `!` indicator |
| 401 auth error | `Claude: auth?` |
| No credentials file | `Claude: no creds` |
| Bad credentials JSON | `Claude: bad creds` |
| API field is null | Skip that field |

No self-refresh of OAuth tokens — Claude Code manages its own token lifecycle.

## Implementation

- `rust-gtk/` — Rust + GTK3 + Cairo (single binary, no runtime dependencies beyond GTK3)

## Running

```bash
# From source
cd rust-gtk && cargo run --release

# From installed package
claude-usage-monitor &
```

## Autostart

Copy a `.desktop` file to `~/.config/autostart/` to launch on login.
