#![windows_subsystem = "windows"]

mod balance;
mod config;
mod render;
mod system;

use std::sync::Arc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, OnceLock,
};
use std::thread;

use windows::{
    core::{w, PCWSTR},
    Win32::Foundation::*,
    Win32::Graphics::Gdi::*,
    Win32::System::LibraryLoader::*,
    Win32::UI::HiDpi::*,
    Win32::UI::Input::KeyboardAndMouse::*,
    Win32::UI::WindowsAndMessaging::*,
};

use balance::Provider;
use render::Row;

fn str_to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[derive(Debug)]
pub struct AppState {
    pub config: Mutex<config::Config>,
    /// One display row per configured provider, in `sources()` order,
    /// followed by the system block.
    pub lines: Mutex<Vec<Row>>,
    pub visible: AtomicBool,
    pub hwnd: Mutex<Option<HWND>>,
    pub font: Mutex<Option<HFONT>>,
    /// UI thread only, ticked by WM_TIMER.
    pub metrics: Mutex<system::Metrics>,
}

const HOTKEY_TOGGLE: u32 = 1;
const HOTKEY_EXIT: u32 = 2;
const WM_UPDATE_DISPLAY: u32 = WM_USER + 1;
const TIMER_SYSTEM: usize = 1;
/// CPU load is a rate, so it needs a cadence of its own: the balance poll runs
/// a hundred times slower and would report a meaningless number.
const SYSTEM_TICK_MS: u32 = 1000;

/// Text insets inside the window, logical pixels.
pub const PAD_LEFT: i32 = 8;
pub const PAD_RIGHT: i32 = 6;
/// Space on either side of a meter, between its label and its percentage.
pub const METER_GAP: i32 = 6;
/// Preferred chart grid cell, logical pixels. The drawing code snaps it to a
/// divisor of the meter height so the cells come out square and fit exactly.
pub const GRID_CELL: i32 = 5;
const MIN_REFRESH_SECS: u64 = 5;
const BASE_DPI: i32 = 96;

static APP_STATE: OnceLock<Arc<AppState>> = OnceLock::new();

fn get_state() -> &'static Arc<AppState> {
    APP_STATE.get().expect("APP_STATE not initialized")
}

fn sources(cfg: &config::Config) -> Vec<(Provider, String)> {
    // Alphabetical by label; this order is the display order.
    [
        (Provider::DeepSeek, &cfg.deepseek_token),
        (Provider::OpenRouter, &cfg.openrouter_token),
        (Provider::ProxyApi, &cfg.proxyapi_token),
    ]
    .into_iter()
    .filter(|(_, token)| !token.trim().is_empty())
    .map(|(p, token)| (p, token.trim().to_string()))
    .collect()
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state = get_state();

    match msg {
        WM_PAINT => {
            // Nothing to paint: UpdateLayeredWindow owns the window surface and
            // keeps it across expose events. Validate the region and move on.
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_ERASEBKGND => {
            return LRESULT(1);
        }
        WM_HOTKEY => {
            let id = wparam.0 as u32;
            if id == HOTKEY_TOGGLE {
                let visible = state.visible.load(Ordering::SeqCst);
                let _ = ShowWindow(hwnd, if visible { SW_HIDE } else { SW_SHOW });
                state.visible.store(!visible, Ordering::SeqCst);
            } else if id == HOTKEY_EXIT {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_UPDATE_DISPLAY => {
            // Drawing lives here, on the UI thread: the fetcher thread must not
            // touch the window, and every window call it made would block on a
            // synchronous message back to this one.
            render::present(hwnd, state, GetDpiForWindow(hwnd));
            return LRESULT(0);
        }
        WM_TIMER => {
            if wparam.0 == TIMER_SYSTEM {
                let mut metrics = state.metrics.lock().unwrap();
                metrics.sample();
                let rows = metrics.rows();
                drop(metrics);

                let mut lines = state.lines.lock().unwrap();
                // The system block is the tail of the vector, in the order
                // startup pushed it; the fetcher owns everything before it.
                let start = lines.len().saturating_sub(system::LINE_COUNT);
                for (slot, row) in lines[start..].iter_mut().zip(rows) {
                    *slot = row;
                }
                drop(lines);

                render::present(hwnd, state, GetDpiForWindow(hwnd));
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            // The suggested rect Windows passes in lparam is for windows that
            // keep their size; ours is derived from the text, so ignore it and
            // rebuild the font at the new scale instead.
            let dpi = (wparam.0 & 0xFFFF) as u32;
            let cfg = state.config.lock().unwrap().clone();
            replace_font(state, &cfg, dpi);
            render::present(hwnd, state, dpi);
            return LRESULT(0);
        }
        WM_DESTROY => {
            let _ = UnregisterHotKey(hwnd, HOTKEY_TOGGLE as i32);
            let _ = UnregisterHotKey(hwnd, HOTKEY_EXIT as i32);

            let mut font_lock = state.font.lock().unwrap();
            if let Some(font) = font_lock.take() {
                let _ = DeleteObject(font);
            }

            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn create_window(cfg: &config::Config) -> HWND {
    let hinstance = unsafe { GetModuleHandleW(None).expect("GetModuleHandleW failed") };

    let class_name = w!("BalanceOverlayWnd");

    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance.into(),
        lpszClassName: class_name,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).expect("LoadCursorW failed") },
        hbrBackground: HBRUSH::default(),
        ..Default::default()
    };

    unsafe { RegisterClassW(&wc) };

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED
                | WS_EX_TOPMOST
                | WS_EX_TRANSPARENT
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE,
            class_name,
            w!("BalanceOverlay"),
            WS_POPUP | WS_VISIBLE,
            // Placeholder geometry; relayout() sets the real one once the
            // window exists and its monitor's DPI can be queried.
            cfg.pos_x,
            cfg.pos_y,
            300,
            100,
            None,
            None,
            hinstance,
            None,
        )
    };

    if hwnd.0 == 0 {
        panic!("CreateWindowExW failed");
    }

    // No SetLayeredWindowAttributes: it and UpdateLayeredWindow are mutually
    // exclusive, and opacity is baked per pixel in render::present instead.

    hwnd
}

/// Config values are logical pixels at 96 DPI; everything Win32 sees is physical.
pub fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / BASE_DPI
}

pub fn line_height(cfg: &config::Config, dpi: u32) -> i32 {
    scale((cfg.font_size * 3 / 2).max(24), dpi)
}

fn create_font(cfg: &config::Config, dpi: u32) -> HFONT {
    let font_name_w = str_to_wide(&cfg.font_name);
    unsafe {
        CreateFontW(
            scale(cfg.font_size, dpi),
            0,
            0,
            0,
            if cfg.font_bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            FF_DONTCARE.0 as u32,
            PCWSTR(font_name_w.as_ptr()),
        )
    }
}

/// UI thread only — the old font may be selected into a DC on this thread.
fn replace_font(state: &AppState, cfg: &config::Config, dpi: u32) {
    let font = create_font(cfg, dpi);
    if let Some(old) = state.font.lock().unwrap().replace(font) {
        unsafe {
            let _ = DeleteObject(old);
        }
    }
}

async fn balance_fetcher_loop(state: Arc<AppState>) {
    let client = match balance::client() {
        Ok(c) => c,
        Err(e) => {
            if let Some(slot) = state.lines.lock().unwrap().first_mut() {
                *slot = Row::Text(e);
            }
            notify(&state);
            return;
        }
    };

    loop {
        let (refresh_interval, srcs) = {
            let cfg = state.config.lock().unwrap();
            (cfg.refresh_interval_secs.max(MIN_REFRESH_SECS), sources(&cfg))
        };

        let mut lines: Vec<Row> = Vec::with_capacity(srcs.len());
        for (provider, token) in &srcs {
            lines.push(Row::Text(balance::fetch_line(&client, *provider, token).await));
        }
        {
            let mut slots = state.lines.lock().unwrap();
            for (slot, text) in slots.iter_mut().zip(lines) {
                *slot = text;
            }
        }

        notify(&state);

        tokio::time::sleep(std::time::Duration::from_secs(refresh_interval)).await;
    }
}

fn notify(state: &AppState) {
    if let Some(hwnd) = *state.hwnd.lock().unwrap() {
        unsafe {
            let _ = PostMessageW(hwnd, WM_UPDATE_DISPLAY, None, None);
        }
    }
}

fn main() {
    // Before any window exists: opt out of DWM bitmap-stretching, so the text
    // stays sharp and GetDpiForWindow reports the monitor's real DPI.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let cfg = config::Config::load_or_create("balance_overlay.toml");

    let srcs = sources(&cfg);
    // Fixed slot layout: one per provider, then the system line if enabled.
    // Both writers address their own slots, so neither replaces the vector.
    let mut lines: Vec<Row> = srcs
        .iter()
        .map(|(p, _)| Row::Text(format!("{}: LOADING...", p.label())))
        .collect();
    if cfg.show_system {
        lines.extend(system::Metrics::default().rows());
    }

    let state = Arc::new(AppState {
        config: Mutex::new(cfg.clone()),
        lines: Mutex::new(lines),
        visible: AtomicBool::new(true),
        hwnd: Mutex::new(None),
        font: Mutex::new(None),
        metrics: Mutex::new(system::Metrics::default()),
    });

    APP_STATE.set(state.clone()).expect("APP_STATE already set");

    let hwnd = create_window(&cfg);
    *state.hwnd.lock().unwrap() = Some(hwnd);

    let dpi = unsafe { GetDpiForWindow(hwnd) };
    replace_font(&state, &cfg, dpi);
    unsafe { render::present(hwnd, &state, dpi) };

    if cfg.show_system {
        unsafe { SetTimer(hwnd, TIMER_SYSTEM, SYSTEM_TICK_MS, None) };
    }

    if state.lines.lock().unwrap().is_empty() {
        state.visible.store(false, Ordering::SeqCst);
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }

    unsafe {
        if RegisterHotKey(hwnd, HOTKEY_TOGGLE as i32, MOD_SHIFT, VK_F11.0 as u32).is_err() {
            eprintln!("Warning: Failed to register Shift+F11 (maybe already in use?)");
        }
        if RegisterHotKey(
            hwnd,
            HOTKEY_EXIT as i32,
            MOD_CONTROL | MOD_SHIFT,
            VK_F11.0 as u32,
        )
        .is_err()
        {
            eprintln!("Warning: Failed to register Ctrl+Shift+F11 (maybe already in use?)");
        }
    }

    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(balance_fetcher_loop(state));
    });

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
