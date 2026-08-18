# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build --release        # produces target/release/balance-overlay.exe
cargo clippy --all-targets
cargo test                   # pure-logic tests only: money/bar formatting, CPU math, color conversion
cargo test balance_formatting
```

Tests cover only the pure logic that can be checked without a window. Everything else is a manual, visual check: the binary has no console (`#![windows_subsystem = "windows"]`), so `eprintln!` output is invisible in normal use — never rely on it for diagnostics. On first run it writes `balance_overlay.toml` next to the executable (defaults, empty tokens) and hides itself, because with no tokens there are no lines to draw. To see anything, fill in at least one of `deepseek_token` / `proxyapi_token` / `openrouter_token` in that file and restart.

Windows-only: the `windows` crate, GDI, and `RegisterHotKey` mean this does not build or run anywhere else.

## Architecture

A layered always-on-top overlay window that polls API-credit balances and paints them as text, one line per provider, plus a CPU chart and a RAM bar showing local load. Two threads, one shared state object.

**Threads.** The main thread creates the window and runs the classic `GetMessageW` pump. A second `std::thread` owns a single-threaded tokio runtime running `balance_fetcher_loop`, which polls every configured API in turn, writes the formatted strings into shared state, and posts `WM_UPDATE_DISPLAY` (a `WM_USER + 1` custom message) to wake the UI thread. All cross-thread signalling goes through `PostMessageW`; the fetcher thread never draws and never calls a window-manipulating API, because those send synchronous messages back to the UI thread.

**Shared state.** `AppState` in `main.rs` holds everything behind `Mutex`es and lives in a `OnceLock<Arc<AppState>>` global. The global exists because `wnd_proc` is an `extern "system"` callback with no user-data pointer; `get_state()` is how the window procedure reaches the app. Lock order in practice is *lines → config → font → window dimensions*; keep it that way, and drop guards before calling any Win32 function that can re-enter the window procedure.

**Providers are a table, not fields.** `Provider` (`balance.rs`) carries its own label and URL, and `sources(&cfg)` in `main.rs` produces the active `(Provider, token)` list by filtering out blank tokens. Adding a provider means: a config field, an enum variant with its label/URL, one row in `sources()`, and a response-parsing arm in `fetch`. Nothing else in the app is per-provider — line count, window size, and painting all derive from the length of that list.

**Rows are the display model.** `render::Row` is what `state.lines` holds: `Text` for providers, `Bar` for an instantaneous value, `Graph` for a history. Provider rows are pre-formatted strings, not numbers — including error text (`"OPENROUTER: Invalid token"`) and the initial `"LOADING..."` — so errors are visible in the overlay itself, which is the only diagnostic channel this app has. Meter rows carry raw percentages instead, because they are drawn rather than written.

**Measuring and drawing must agree.** A meter row is `label | meter | percentage`, and `relayout` has to predict its width before `paint` lays it out. `render::meter_row_width` and `render::label_column_width` exist so both callers compute it the same way; changing the geometry in `paint` alone silently clips the row. `paint` gets its DPI from `GetDeviceCaps(dc, LOGPIXELSX)`, which reports the monitor DPI only because the process is per-monitor aware.

**`lines` has a fixed slot layout and two writers.** Slots `0..n` are the providers in `sources()` order; the last `system::LINE_COUNT` slots are the system block when `show_system` is on. The vector is allocated once at startup and *never replaced* — each writer assigns into its own slots by index, because the two run at different rates and on different threads. Sizing follows from `lines.len()`, so a failing API keeps its slot rather than collapsing the layout.

**System metrics run on the UI thread.** `system.rs` is two Win32 calls: `GlobalMemoryStatusEx` (whose `dwMemoryLoad` is already a percentage) and `GetSystemTimes`. CPU load only exists as a *difference* between two samples, so `CpuSampler` holds the previous one plus a `HISTORY_LEN` ring of readings, and the first tick renders `--`. Both calls take microseconds and never block, which is why a `WM_TIMER` at `SYSTEM_TICK_MS` does the sampling directly in `wnd_proc` — no thread, no async, no new dependency. A per-second cadence is the point: the same number sampled by the 60-second balance poll would be meaningless, and one tick is one column of the chart.

**Meters are GDI, and monochrome by construction.** `draw_bar` and `draw_graph` share `fill_and_frame`, which lays the track down first and the frame last so a fill can never paint over its own outline. Track, grid and fill are three `shade()` steps of the single `meter_color`, so they cannot clash however the user recolors them. The chart is a `Polygon` anchored to the right edge and spaced by `capacity`, not by how many samples exist — that is what makes a half-full history scroll in at a steady rate instead of stretching. The grid rounds its cell to a divisor of the meter height, so the cells come out square and the rows even.

**Layout.** `relayout` measures the widest line with `GetTextExtentPoint32W` against the real font, then positions and sizes the window in one `SetWindowPos`. It runs on the UI thread only — from `WM_UPDATE_DISPLAY`, from `WM_DPICHANGED`, and once at startup — and returns early when the resulting rect is unchanged, which is the common case on a poll. `render::paint` re-derives per-line height as `height / lines.len()`, so that calculation must stay in step with `line_height`.

**DPI.** The process declares `PER_MONITOR_AWARE_V2` before any window exists, which opts out of DWM bitmap-stretching: text stays sharp, but nothing is scaled for you. Every config dimension (`font_size`, `pos_x`, `pos_y`, `PAD_X`, `line_height`) is a logical value at 96 DPI and must go through `scale()` before it reaches Win32 — `window_rect` and everything `render.rs` touches are already physical pixels. `WM_DPICHANGED` rebuilds the font at the new scale and re-lays out; it deliberately ignores the suggested rect Windows passes in `lparam`, since the window's size comes from its text, not from the old size. `outline_width` is intentionally *not* scaled — it is a hairline by intent, and the user can raise it in config.

**Painting.** `render::paint` double-buffers into a memory DC and `BitBlt`s once, to avoid flicker on the layered window. Outline text is brute force: `draw_text_outline` redraws the string at every offset in a `(2w+1)²` box before the fill pass, so `outline_width` in config is quadratic in cost.

**Window flags** in `create_window` are load-bearing as a set: `WS_EX_LAYERED` for `SetLayeredWindowAttributes` opacity, `WS_EX_TRANSPARENT` + `WS_EX_NOACTIVATE` so clicks pass through and focus is never stolen, `WS_EX_TOOLWINDOW` to stay out of Alt+Tab, `WS_EX_TOPMOST` to stay above everything. Dropping any one of them changes the "invisible passive overlay" behaviour.

**Hotkeys** are registered on the window, not globally scoped to the process: Shift+F11 toggles visibility, Ctrl+Shift+F11 exits. `RegisterHotKey` matches modifiers exactly, so the two do not collide.

**Config** (`config.rs`) is a flat TOML struct where every field has a `#[serde(default)]`, so adding a field is backward-compatible with existing user files. It is read once at startup and cloned into `AppState`; there is no reload path, so changing config requires a restart. `load_or_create` writes the file only when it does not exist — an unparseable config falls back to in-memory defaults and is deliberately left on disk, because overwriting it would destroy the user's API tokens.

**API layer** (`balance.rs`) returns `Result<_, String>` throughout — no error type, because every error's only destiny is being rendered as a line of text. `fetch` shares the request, auth-failure, and body-read path, then branches per provider only for parsing, since the response shapes have nothing in common: DeepSeek returns `balance_infos[0].total_balance` as a *string* that must be parsed, ProxyAPI returns a float but can answer 200 with an `"Invalid API Key"` body (hence the text sniff before deserializing), and OpenRouter reports `total_credits` and `total_usage` separately — the balance is their difference.

## Gotchas

- The `WM_TIMER` handler overwrites the last `system::LINE_COUNT` slots, which are the system block only because the timer is started **only** when `show_system` is on. Starting it unconditionally, or letting `system::rows` return a different count than startup reserved, would silently overwrite a provider's line.
- `render::percent_text` pads to a fixed width, and `meter_row_width` measures the widest case, so the window does not twitch wider and back every second as the numbers change.
- Every `CreateSolidBrush` needs its `DeleteObject`, and the pen and brush selected for `Polygon` have to be restored before the brush is deleted. This runs once a second forever, so a leak here is not survivable.
- The `reqwest::Client` is built once outside the poll loop; rebuilding it per request re-does the whole TLS setup.
- `refresh_interval_secs` is floored at `MIN_REFRESH_SECS` — a `0` in the config would otherwise mean an unthrottled request loop.
- `format_balance` rounds once in integer cents; computing integer and fractional parts independently is what used to turn `12.999` into `12.100`.
- `parse_hex_color` swaps R and B, because GDI `COLORREF` is `0x00BBGGRR`, not RGB. Grey/white/black are palindromic, so a regression here stays invisible until someone configures an actual color.
- The font is owned by `state.font` and swapped by `replace_font`, which deletes the old handle. Both it and `render::paint` must stay on the UI thread — deleting a font selected into another thread's DC is a use-after-free.
- `balance_overlay.toml` is resolved against the *working directory*, not the exe's folder, so a shortcut to the installed copy must set "Start in". The installed copy lives in `%APPDATA%\balance-overlay\` and holds API tokens in plaintext; the repo gitignores its local equivalent.
