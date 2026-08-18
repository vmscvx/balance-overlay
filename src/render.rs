use std::ffi::c_void;

use windows::{
    Win32::Foundation::*,
    Win32::Graphics::Gdi::*,
    Win32::UI::WindowsAndMessaging::*,
};

use crate::{line_height, scale, AppState, GRID_CELL, METER_GAP, PAD_LEFT, PAD_RIGHT};

/// One line of the overlay. Providers are text; the system block draws itself.
#[derive(Debug, Clone)]
pub enum Row {
    Text(String),
    /// Load history, drawn as a filled area chart, newest sample on the right.
    Graph {
        label: &'static str,
        percent: Option<u32>,
        history: Vec<u32>,
        /// Samples the chart is scaled for, so it scrolls in at a constant
        /// speed instead of stretching while the history fills up.
        capacity: usize,
    },
}

impl Row {
    fn label(&self) -> Option<&'static str> {
        match self {
            Row::Text(_) => None,
            Row::Graph { label, .. } => Some(label),
        }
    }
}

/// Frame, grid and fill are three steps of the meter color, so one config key
/// drives all of them and they can never clash.
const FRAME_SHADE: u32 = 5;
const GRID_SHADE: u32 = 3;
const WINDOW_BG: u32 = 0x0020_2020;

/// Percentages are padded to a fixed width so the column does not jitter.
fn percent_text(percent: Option<u32>) -> String {
    match percent {
        Some(p) => format!("{:>3}%", p),
        None => "  --".to_string(),
    }
}

fn text_width(hdc: HDC, text: &str) -> i32 {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    unsafe {
        if GetTextExtentPoint32W(hdc, &wide, &mut size).as_bool() {
            return size.cx;
        }
    }
    0
}

/// Where each part of a meter row sits. Measuring and drawing cannot drift
/// apart because the width the window needs comes from the same numbers that
/// place the meter inside it.
struct Metrics {
    pad_left: i32,
    gap: i32,
    label_width: i32,
    meter_width: i32,
    percent_width: i32,
    grid_cell: i32,
}

impl Metrics {
    fn measure(hdc: HDC, rows: &[Row], meter_width_logical: i32, dpi: u32) -> Self {
        Self {
            pad_left: scale(PAD_LEFT, dpi),
            gap: scale(METER_GAP, dpi),
            // Every meter shares the widest label, so the charts line up even
            // in a proportional font.
            label_width: rows
                .iter()
                .filter_map(|r| r.label())
                .map(|l| text_width(hdc, l))
                .max()
                .unwrap_or(0),
            meter_width: scale(meter_width_logical, dpi),
            percent_width: text_width(hdc, &percent_text(Some(100))),
            grid_cell: scale(GRID_CELL, dpi),
        }
    }

    fn meter_left(&self) -> i32 {
        self.pad_left + self.label_width + self.gap
    }

    fn row_width(&self, hdc: HDC, row: &Row) -> i32 {
        match row {
            Row::Text(text) => self.pad_left + text_width(hdc, text),
            Row::Graph { .. } => {
                self.meter_left() + self.meter_width + self.gap + self.percent_width
            }
        }
    }
}

/// Draws the whole overlay and hands it to the window as a per-pixel-alpha
/// surface. This replaces painting on `WM_PAINT`: `UpdateLayeredWindow` owns
/// the window bitmap, and it is the only way to keep the meters opaque while
/// the rest of the overlay stays translucent.
pub unsafe fn present(hwnd: HWND, state: &AppState, dpi: u32) {
    let rows = state.lines.lock().unwrap().clone();
    if rows.is_empty() {
        return;
    }

    let cfg = state.config.lock().unwrap().clone();
    let font = state.font.lock().unwrap().unwrap_or_default();

    let screen_dc = GetDC(None);
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        ReleaseDC(None, screen_dc);
        return;
    }
    let old_font = SelectObject(mem_dc, font);

    let m = Metrics::measure(mem_dc, &rows, cfg.meter_width, dpi);
    let lh = line_height(&cfg, dpi);
    let width = rows.iter().map(|r| m.row_width(mem_dc, r)).max().unwrap_or(0)
        + scale(PAD_RIGHT, dpi)
        + cfg.outline_width as i32 * 2;
    let height = rows.len() as i32 * lh;

    if width > 0 && height > 0 {
        let mut bits: *mut c_void = std::ptr::null_mut();
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative height means top-down rows, so index 0 is the top.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        if let Ok(bmp) = CreateDIBSection(screen_dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            let old_bmp = SelectObject(mem_dc, bmp);

            let opaque = draw(mem_dc, &rows, &cfg, &m, width, height, lh);

            // GDI leaves the alpha byte at zero, so it is ours to fill in.
            // Meters get full opacity; everything else the configured value.
            let pixels =
                std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize);
            apply_alpha(pixels, width, height, cfg.opacity, &opaque);

            let position = POINT {
                x: scale(cfg.pos_x, dpi),
                y: scale(cfg.pos_y, dpi),
            };
            let size = SIZE { cx: width, cy: height };
            let source = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                // Per-pixel alpha is already baked in; nothing extra on top.
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                hwnd,
                screen_dc,
                Some(&position),
                Some(&size),
                mem_dc,
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );

            SelectObject(mem_dc, old_bmp);
            let _ = DeleteObject(bmp);
        }
    }

    SelectObject(mem_dc, old_font);
    let _ = DeleteDC(mem_dc);
    ReleaseDC(None, screen_dc);
}

/// Returns the rectangles that must end up fully opaque.
unsafe fn draw(
    hdc: HDC,
    rows: &[Row],
    cfg: &crate::config::Config,
    m: &Metrics,
    width: i32,
    height: i32,
    line_height: i32,
) -> Vec<RECT> {
    let text_rgb = parse_hex_color(&cfg.text_color);
    let outline_rgb = parse_hex_color(&cfg.outline_color);
    let meter_rgb = parse_hex_color(&cfg.meter_color);

    let bg = CreateSolidBrush(COLORREF(WINDOW_BG));
    let all = RECT { left: 0, top: 0, right: width, bottom: height };
    let _ = FillRect(hdc, &all, bg);
    let _ = DeleteObject(bg);

    SetBkMode(hdc, TRANSPARENT);

    let mut opaque = Vec::new();

    for (i, row) in rows.iter().enumerate() {
        let y = (i as i32) * line_height;
        let text_rect = |left: i32| RECT {
            left,
            top: y + 4,
            right: width - 4,
            bottom: y + line_height,
        };

        match row {
            Row::Text(text) => {
                draw_row_text(hdc, text, text_rect(m.pad_left), text_rgb, outline_rgb, cfg.outline_width);
            }
            Row::Graph { label, percent, history, capacity } => {
                draw_row_text(hdc, label, text_rect(m.pad_left), text_rgb, outline_rgb, cfg.outline_width);

                // A meter reads better as a band than as a full-height block.
                let meter_h = (line_height * 5 / 9).max(4);
                let top = y + (line_height - meter_h) / 2;
                let meter = RECT {
                    left: m.meter_left(),
                    top,
                    right: m.meter_left() + m.meter_width,
                    bottom: top + meter_h,
                };
                draw_graph(hdc, meter, history, *capacity, meter_rgb, outline_rgb, m.grid_cell);
                opaque.push(meter);

                draw_row_text(
                    hdc,
                    &percent_text(*percent),
                    text_rect(meter.right + m.gap),
                    text_rgb,
                    outline_rgb,
                    cfg.outline_width,
                );
            }
        }
    }

    opaque
}

/// Bakes the alpha channel in, premultiplied, which is what `ULW_ALPHA` wants.
fn apply_alpha(pixels: &mut [u32], width: i32, height: i32, base: u8, opaque: &[RECT]) {
    for y in 0..height {
        for x in 0..width {
            let inside = opaque
                .iter()
                .any(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom);
            let a = if inside { 255u32 } else { base as u32 };

            let i = (y * width + x) as usize;
            let px = pixels[i];
            let (b, g, r) = (px & 0xFF, (px >> 8) & 0xFF, (px >> 16) & 0xFF);
            pixels[i] = (a << 24) | (r * a / 255) << 16 | (g * a / 255) << 8 | (b * a / 255);
        }
    }
}

unsafe fn draw_row_text(
    hdc: HDC,
    text: &str,
    mut rect: RECT,
    text_rgb: u32,
    outline_rgb: u32,
    outline_width: u32,
) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);

    if outline_width > 0 {
        SetTextColor(hdc, COLORREF(outline_rgb));
        draw_text_outline(hdc, &mut wide, &rect, outline_width);
    }

    SetTextColor(hdc, COLORREF(text_rgb));
    DrawTextW(hdc, &mut wide, &mut rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
}

/// `num/16` of full intensity, per channel.
fn shade(rgb: u32, num: u32) -> u32 {
    let ch = |shift: u32| ((rgb >> shift) & 0xFF) * num / 16;
    ch(0) | (ch(8) << 8) | (ch(16) << 16)
}

/// Faint reference lines under the chart, in square cells. `target` is only a
/// wish: the cell picked is the one that divides the meter height evenly,
/// closest to that wish, because a cell that does not divide it leaves a
/// one-pixel sliver of a row at the top. The same cell then goes across.
/// Anchored bottom-right, on the zero line and the newest sample.
unsafe fn draw_grid(hdc: HDC, rect: RECT, target: i32, color: u32) {
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if target < 2 || w < 4 || h < 4 {
        return;
    }
    // Exact fit first, closeness to the target second.
    let Some(cell) = (2..=h / 2).min_by_key(|c| (h % c, (c - target).abs())) else {
        return;
    };

    let brush = CreateSolidBrush(COLORREF(color));

    let mut x = rect.right - cell;
    while x > rect.left {
        let line = RECT { left: x, top: rect.top, right: x + 1, bottom: rect.bottom };
        let _ = FillRect(hdc, &line, brush);
        x -= cell;
    }
    let mut y = rect.bottom - cell;
    while y > rect.top {
        let line = RECT { left: rect.left, top: y, right: rect.right, bottom: y + 1 };
        let _ = FillRect(hdc, &line, brush);
        y -= cell;
    }

    let _ = DeleteObject(brush);
}

unsafe fn draw_graph(
    hdc: HDC,
    rect: RECT,
    history: &[u32],
    capacity: usize,
    fill: u32,
    well: u32,
    grid_cell: i32,
) {
    let well_brush = CreateSolidBrush(COLORREF(well));
    let _ = FillRect(hdc, &rect, well_brush);
    let _ = DeleteObject(well_brush);

    // Under the chart, so the fill covers it where the load has been.
    draw_grid(hdc, rect, grid_cell, shade(fill, GRID_SHADE));

    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if history.len() >= 2 && capacity >= 2 && w > 1 && h > 0 {
        // The newest sample sits on the right edge and older ones step left by
        // a fixed slot, so a half-full history scrolls in instead of
        // stretching to fill the width.
        let last = history.len() - 1;
        let x_at = |from_right: usize| {
            (rect.right - (from_right as i32) * w / (capacity as i32 - 1)).max(rect.left)
        };

        let mut points: Vec<POINT> = history
            .iter()
            .enumerate()
            .map(|(i, &p)| POINT {
                x: x_at(last - i),
                y: rect.bottom - (p.min(100) as i32 * h / 100),
            })
            .collect();
        // Close the area along the bottom edge.
        points.push(POINT { x: points[last].x, y: rect.bottom });
        points.push(POINT { x: points[0].x, y: rect.bottom });

        let brush = CreateSolidBrush(COLORREF(fill));
        let old_brush = SelectObject(hdc, brush);
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        let _ = Polygon(hdc, &points);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(brush);
    }

    // Frame last, so the fill cannot paint over its own edge.
    let frame = CreateSolidBrush(COLORREF(shade(fill, FRAME_SHADE)));
    let _ = FrameRect(hdc, &rect, frame);
    let _ = DeleteObject(frame);
}

unsafe fn draw_text_outline(hdc: HDC, text: &mut [u16], rect: &RECT, width: u32) {
    let w = width as i32;
    for dx in -w..=w {
        for dy in -w..=w {
            if dx == 0 && dy == 0 {
                continue;
            }
            let mut r = RECT {
                left: rect.left + dx,
                top: rect.top + dy,
                right: rect.right + dx,
                bottom: rect.bottom + dy,
            };
            DrawTextW(hdc, text, &mut r, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
        }
    }
}

/// "RRGGBB" -> COLORREF, which is 0x00BBGGRR. The swap matters for every color
/// that is not a shade of grey.
fn parse_hex_color(hex: &str) -> u32 {
    let rgb = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0x00FFFFFF);
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_colorref_swaps_channels() {
        assert_eq!(parse_hex_color("FF0000"), 0x0000FF); // red stays red
        assert_eq!(parse_hex_color("#0000FF"), 0xFF0000);
        assert_eq!(parse_hex_color("FFFFFF"), 0xFFFFFF);
        assert_eq!(parse_hex_color("zzz"), 0x00FFFFFF); // garbage -> white
    }

    #[test]
    fn shades_step_down_without_bleeding_between_channels() {
        assert_eq!(shade(0x000000, FRAME_SHADE), 0x000000);
        assert_eq!(shade(0xFFFFFF, FRAME_SHADE), 0x4F4F4F);
        assert_eq!(shade(0x0000FF, FRAME_SHADE), 0x00004F); // stays in its channel
        // Grid is the faintest mark, then the frame, then the fill itself.
        assert!(shade(0xFFFFFF, GRID_SHADE) < shade(0xFFFFFF, FRAME_SHADE));
        assert!(shade(0xFFFFFF, FRAME_SHADE) < 0xFFFFFF);
    }

    #[test]
    fn alpha_is_opaque_inside_the_meters_and_premultiplied_outside() {
        let white = 0x00FF_FFFFu32;
        let mut pixels = vec![white; 4];
        let opaque = [RECT { left: 1, top: 0, right: 2, bottom: 1 }];
        apply_alpha(&mut pixels, 2, 2, 128, &opaque);

        // Inside the meter: fully opaque, color untouched.
        assert_eq!(pixels[1], 0xFFFF_FFFF);
        // Outside: alpha in the top byte, every channel scaled by it.
        let scaled = 0xFFu32 * 128 / 255;
        assert_eq!(pixels[0], 0x8000_0000 | scaled << 16 | scaled << 8 | scaled);
        assert_eq!(pixels[2], pixels[0]);
        assert_eq!(pixels[3], pixels[0]);
    }

    #[test]
    fn black_keeps_its_alpha_with_nothing_to_scale() {
        let mut pixels = vec![0x0000_0000u32; 1];
        apply_alpha(&mut pixels, 1, 1, 200, &[]);
        assert_eq!(pixels[0], 0xC800_0000);
    }
}
