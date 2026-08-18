# balance-overlay

A small always-on-top Windows overlay that keeps your API credit balances and your machine's load on screen, out of the way of everything else.

It polls DeepSeek, OpenRouter and ProxyAPI for their remaining credit, and samples CPU and memory load once a second. Clicks pass straight through it, it never takes focus, and it stays out of Alt+Tab — you can leave it running over a full-screen editor and forget it is there.

It shows one of two things at a time, and a hotkey swaps between them:

- **System** — CPU and RAM load as charts of the last minute. This is what it starts with.
- **Balances** — one line per provider with its remaining credit.

Sampling continues in both modes, so switching back to the charts shows the minute that actually passed rather than an empty graph.

## Building

```bash
cargo build --release
```

The result is `target/release/balance-overlay.exe`, a single file with no runtime dependencies.

Windows only, and not by accident: it is built on Win32, GDI and `RegisterHotKey` throughout, so there is nothing to port.

## First run

Run the executable once. It writes `balance_overlay.toml` next to itself and starts in system mode, which needs no configuration at all. To get the balance mode to show anything, add at least one token and restart.

```toml
deepseek_token = "sk-..."
openrouter_token = ""
proxyapi_token = ""
```

The file is resolved against the **working directory**, not the location of the executable, so a shortcut has to set "Start in" to the folder holding it.

Providers with an empty token are skipped entirely — no line, no request. A provider that fails to answer keeps its line and shows the reason there:

```
OPENROUTER: Invalid token
```

That is deliberate. The binary has no console, so the overlay itself is the only place an error can appear.

## Hotkeys

| Keys | Action |
| --- | --- |
| `Shift`+`F11` | Show / hide |
| `Alt`+`Shift`+`F11` | Switch between system and balances |
| `Ctrl`+`Shift`+`F11` | Quit |

## Configuration

Every key is optional and falls back to the default below, so an older config file keeps working after an update. The file is read once at startup — changes need a restart.

| Key | Default | Meaning |
| --- | --- | --- |
| `deepseek_token` | `""` | DeepSeek API key. Empty means the line is not shown. |
| `openrouter_token` | `""` | OpenRouter API key. |
| `proxyapi_token` | `""` | ProxyAPI key. |
| `start_mode` | `"system"` | Mode to open in: `system` or `balances`. Anything else reads as `system`. |
| `refresh_interval_secs` | `60` | Seconds between balance polls, floored at 5. |
| `font_name` | `"Consolas"` | Any installed font. A monospace one keeps the numbers from shifting. |
| `font_size` | `18` | Logical pixels at 96 DPI. |
| `font_bold` | `true` | |
| `text_color` | `"FFFFFF"` | `RRGGBB`, with or without a leading `#`. |
| `outline_color` | `"000000"` | Text outline, and the well the charts sit in. |
| `outline_width` | `1` | In pixels, and quadratic in cost: the text is redrawn at every offset in a `(2w+1)²` box. |
| `meter_color` | `"FFFFFF"` | Chart fill. Its frame and grid are dimmer shades of the same color. |
| `meter_width` | `120` | Chart width in logical pixels. |
| `opacity` | `200` | 0–255, applied to everything except the charts, which stay opaque. |
| `pos_x`, `pos_y` | `10`, `10` | Top-left corner, logical pixels. |

Sizes are logical values at 96 DPI: the overlay is per-monitor DPI aware and scales them itself, so it looks the same on a 150% display and follows along when the scaling changes.

Tokens are stored in this file as plain text. It is listed in `.gitignore`, and it is worth keeping it that way.

## Notes

The window is sized to its own contents, so it shrinks and grows as the mode changes and as lines come and go.

A provider with no token has no line in balance mode. If none of them have one, that mode says `NO TOKENS` instead of leaving a stale frame on screen.

The charts hold 60 samples at one per second, newest on the right. CPU load only exists as a difference between two readings, so it shows `--` for the first second after launch; memory is absolute and appears immediately.

Charts are drawn opaque while the text stays translucent, which needs per-pixel alpha — the window is pushed through `UpdateLayeredWindow` rather than painted on `WM_PAINT`. Without it a translucent chart competes with whatever happens to be behind the overlay.

`CLAUDE.md` in the repository root goes into how the pieces fit together, and why several of them are the way they are.

## License

MIT, see [LICENSE](LICENSE).
