use windows::{
    Win32::Foundation::*,
    Win32::Graphics::Gdi::*,
};

use crate::AppState;

pub unsafe fn paint(hdc: HDC, state: &AppState) {
    let font = state.font.lock().unwrap();
    let font = font.unwrap_or_default();

    let lines = state.lines.lock().unwrap().clone();

    if lines.is_empty() {
        return;
    }

    let cfg = state.config.lock().unwrap();
    let outline_width = cfg.outline_width;
    let text_rgb = parse_hex_color(&cfg.text_color);
    let outline_rgb = parse_hex_color(&cfg.outline_color);
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
    let rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    let _ = FillRect(mem_dc, &rect, bg_brush);
    let _ = DeleteObject(bg_brush);

    let old_font = SelectObject(mem_dc, font);
    SetBkMode(mem_dc, TRANSPARENT);

    let num_lines = lines.len() as i32;
    let line_height = height / num_lines;

    for (i, line) in lines.iter().enumerate() {
        let y = (i as i32) * line_height;
        let mut text_rect = RECT {
            left: 8,
            top: y + 4,
            right: width - 4,
            bottom: y + line_height,
        };

        let mut wide: Vec<u16> = line.encode_utf16().collect();
        wide.push(0);

        if outline_width > 0 {
            SetTextColor(mem_dc, COLORREF(outline_rgb));
            draw_text_outline(mem_dc, &mut wide, &text_rect, outline_width);
        }

        SetTextColor(mem_dc, COLORREF(text_rgb));
        DrawTextW(
            mem_dc,
            &mut wide,
            &mut text_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }

    let _ = BitBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);

    SelectObject(mem_dc, old_font);
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem_dc);
}

unsafe fn draw_text_outline(
    hdc: HDC,
    text: &mut [u16],
    rect: &RECT,
    width: u32,
) {
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
/// that isn't a shade of grey.
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
}
