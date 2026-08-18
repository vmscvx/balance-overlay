use windows::{
    Win32::Foundation::*,
    Win32::Graphics::Gdi::*,
};

use crate::{scale, AppState, GRID_CELL, METER_GAP, PAD_LEFT};

/// One line of the overlay. Providers are text; the system block draws itself.
#[derive(Debug, Clone)]
pub enum Row {
    Text(String),
    /// Instantaneous value, drawn as a filled progress bar.
    Bar { label: &'static str, percent: Option<u32> },
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
    pub fn label(&self) -> Option<&'static str> {
        match self {
            Row::Text(_) => None,
            Row::Bar { label, .. } | Row::Graph { label, .. } => Some(label),
        }
    }

    fn percent(&self) -> Option<u32> {
        match self {
            Row::Text(_) => None,
            Row::Bar { percent, .. } | Row::Graph { percent, .. } => *percent,
        }
    }
}

/// Percentages are padded to a fixed width so the column does not jitter.
pub fn percent_text(percent: Option<u32>) -> String {
    match percent {
        Some(p) => format!("{:>3}%", p),
        None => "  --".to_string(),
    }
}

pub fn text_width(hdc: HDC, text: &str) -> i32 {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    unsafe {
        if GetTextExtentPoint32W(hdc, &wide, &mut size).as_bool() {
            return size.cx;
        }
    }
    0
}

/// Width a meter row needs: label, meter, and the percentage column. Shared by
/// `relayout` and `paint` so measuring and drawing cannot drift apart.
pub fn meter_row_width(hdc: HDC, label_width: i32, meter_width: i32, dpi: u32) -> i32 {
    let gap = scale(METER_GAP, dpi);
    label_width + gap + meter_width + gap + text_width(hdc, &percent_text(Some(100)))
}

/// The widest label among the meter rows; both meters share it so their bars
/// start at the same x even in a proportional font.
pub fn label_column_width(hdc: HDC, rows: &[Row]) -> i32 {
    rows.iter()
        .filter_map(|r| r.label())
        .map(|l| text_width(hdc, l))
        .max()
        .unwrap_or(0)
}

pub unsafe fn paint(hdc: HDC, state: &AppState) {
    let font = state.font.lock().unwrap();
    let font = font.unwrap_or_default();

    let rows = state.lines.lock().unwrap().clone();

    if rows.is_empty() {
        return;
    }

    let cfg = state.config.lock().unwrap();
    let outline_width = cfg.outline_width;
    let text_rgb = parse_hex_color(&cfg.text_color);
    let outline_rgb = parse_hex_color(&cfg.outline_color);
    let meter_rgb = parse_hex_color(&cfg.meter_color);
    let meter_width_logical = cfg.meter_width;
    let bg_rgb = 0x00202020u32;
    drop(cfg);

    let (_, _, width, height) = *state.window_rect.lock().unwrap();

    if width <= 0 || height <= 0 {
        return;
    }

    let mem_dc = CreateCompatibleDC(hdc);
    if mem_dc.is_invalid() {
        return;
    }

    let bmp = CreateCompatibleBitmap(hdc, width, height);
    if bmp.is_invalid() {
        let _ = DeleteDC(mem_dc);
        return;
    }

    let old_bmp = SelectObject(mem_dc, bmp);

    let bg_brush = CreateSolidBrush(COLORREF(bg_rgb));
    let rect = RECT { left: 0, top: 0, right: width, bottom: height };
    let _ = FillRect(mem_dc, &rect, bg_brush);
    let _ = DeleteObject(bg_brush);

    let old_font = SelectObject(mem_dc, font);
    SetBkMode(mem_dc, TRANSPARENT);

    // The DC reports its monitor DPI because the process is per-monitor aware;
    // config lengths are logical and have to be scaled before use.
    let dpi = GetDeviceCaps(mem_dc, LOGPIXELSX) as u32;
    let pad_left = scale(PAD_LEFT, dpi);
    let gap = scale(METER_GAP, dpi);
    let meter_width = scale(meter_width_logical, dpi);
    let grid_cell = scale(GRID_CELL, dpi);
    let label_width = label_column_width(mem_dc, &rows);

    let num_lines = rows.len() as i32;
    let line_height = height / num_lines;

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
                draw_row_text(mem_dc, text, text_rect(pad_left), text_rgb, outline_rgb, outline_width);
            }
            _ => {
                let label = row.label().unwrap_or("");
                draw_row_text(mem_dc, label, text_rect(pad_left), text_rgb, outline_rgb, outline_width);

                // A meter reads better as a band than as a full-height block.
                let meter_h = (line_height * 5 / 9).max(4);
                let meter_top = y + (line_height - meter_h) / 2;
                let meter_x = pad_left + label_width + gap;
                let meter = RECT {
                    left: meter_x,
                    top: meter_top,
                    right: meter_x + meter_width,
                    bottom: meter_top + meter_h,
                };

                match row {
                    Row::Graph { history, capacity, .. } => {
                        draw_graph(mem_dc, meter, history, *capacity, meter_rgb, grid_cell)
                    }
                    _ => draw_bar(mem_dc, meter, row.percent(), meter_rgb),
                }

                draw_row_text(
                    mem_dc,
                    &percent_text(row.percent()),
                    text_rect(meter.right + gap),
                    text_rgb,
                    outline_rgb,
                    outline_width,
                );
            }
        }
    }

    let _ = BitBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);

    SelectObject(mem_dc, old_font);
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem_dc);
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

/// Track, grid and fill are three steps of one color, so a single config key
/// drives all of them and they can never clash.
const TRACK_SHADE: u32 = 5;
const GRID_SHADE: u32 = 7;

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

/// Track, then the caller's fill, then the frame on top so the fill cannot
/// paint over its own outline.
unsafe fn fill_and_frame(hdc: HDC, rect: RECT, fill: u32, body: impl FnOnce(HDC)) {
    let track = CreateSolidBrush(COLORREF(shade(fill, TRACK_SHADE)));
    let _ = FillRect(hdc, &rect, track);

    body(hdc);

    let _ = FrameRect(hdc, &rect, track);
    let _ = DeleteObject(track);
}

unsafe fn draw_bar(hdc: HDC, rect: RECT, percent: Option<u32>, fill: u32) {
    fill_and_frame(hdc, rect, fill, |hdc| {
        let Some(percent) = percent else { return };
        let filled = (rect.right - rect.left) * percent.min(100) as i32 / 100;
        if filled <= 0 {
            return;
        }
        let brush = CreateSolidBrush(COLORREF(fill));
        let _ = FillRect(hdc, &RECT { right: rect.left + filled, ..rect }, brush);
        let _ = DeleteObject(brush);
    });
}

unsafe fn draw_graph(hdc: HDC, rect: RECT, history: &[u32], capacity: usize, fill: u32, grid_cell: i32) {
    fill_and_frame(hdc, rect, fill, |hdc| {
        // Under the chart, so the fill covers it where the load has been.
        draw_grid(hdc, rect, grid_cell, shade(fill, GRID_SHADE));

        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if history.len() < 2 || capacity < 2 || w <= 1 || h <= 0 {
            return;
        }

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
    });
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
    #[test]
    fn hex_to_colorref_swaps_channels() {
        assert_eq!(super::parse_hex_color("FF0000"), 0x0000FF); // red stays red
        assert_eq!(super::parse_hex_color("#0000FF"), 0xFF0000);
        assert_eq!(super::parse_hex_color("FFFFFF"), 0xFFFFFF);
        assert_eq!(super::parse_hex_color("zzz"), 0x00FFFFFF); // garbage -> white
    }

    #[test]
    fn shades_step_down_without_bleeding_between_channels() {
        use super::{shade, GRID_SHADE, TRACK_SHADE};
        assert_eq!(shade(0x000000, TRACK_SHADE), 0x000000);
        assert_eq!(shade(0xFFFFFF, TRACK_SHADE), 0x4F4F4F);
        assert_eq!(shade(0x0000FF, TRACK_SHADE), 0x00004F); // stays in its channel
        // Track is darker than grid, and both are darker than the fill.
        assert!(shade(0xFFFFFF, TRACK_SHADE) < shade(0xFFFFFF, GRID_SHADE));
        assert!(shade(0xFFFFFF, GRID_SHADE) < 0xFFFFFF);
    }
}
