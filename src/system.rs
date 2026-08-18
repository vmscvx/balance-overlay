//! CPU, memory and network load, straight from Win32 — no crate needed for a
//! handful of calls.

use std::collections::VecDeque;

use windows::Win32::Foundation::{FILETIME, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::{IfOperStatusUp, MediaConnectStateConnected};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

use crate::render::Row;
use crate::SYSTEM_TICK_MS;

/// Samples kept per chart. One per tick, so at `SYSTEM_TICK_MS` this is also
/// the number of seconds of history on screen.
pub const HISTORY_LEN: usize = 60;

/// Display slots the system block occupies. Startup reserves this many at the
/// tail of `state.lines`, and the timer overwrites exactly those.
pub const LINE_COUNT: usize = 4;

/// Smallest full-scale value for the network charts. Without a floor, an idle
/// link autoscales its own noise to full height and looks like a busy one.
const NET_SCALE_FLOOR: u64 = 128 * 1024;

/// `InterfaceAndOperStatusFlags` bits, in header order.
const FLAG_HARDWARE: u8 = 1 << 0;
const FLAG_CONNECTOR: u8 = 1 << 2;

#[derive(Debug, Default)]
struct History(VecDeque<u64>);

impl History {
    fn push(&mut self, value: u64) {
        if self.0.len() == HISTORY_LEN {
            self.0.pop_front();
        }
        self.0.push_back(value);
    }

    fn latest(&self) -> Option<u64> {
        self.0.back().copied()
    }

    fn peak(&self) -> u64 {
        self.0.iter().copied().max().unwrap_or(0)
    }

    /// Samples mapped onto 0..100 against `full`, oldest first. Normalising
    /// here rather than on the way in is what lets an autoscaled chart rescale
    /// its whole history when a new peak arrives.
    fn normalized(&self, full: u64) -> Vec<u32> {
        let full = full.max(1);
        self.0
            .iter()
            .map(|&v| ((v.min(full) * 100) / full) as u32)
            .collect()
    }

    fn row(&self, label: &'static str, value: String, full: u64) -> Row {
        Row::Graph {
            label,
            value,
            history: self.normalized(full),
            capacity: HISTORY_LEN,
        }
    }
}

fn filetime_to_u64(ft: FILETIME) -> u64 {
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

/// Every load history, plus the readings each rate is measured against. CPU and
/// network only exist as differences between two samples, so both stay empty
/// for one tick; memory is absolute and shows up immediately.
#[derive(Debug, Default)]
pub struct Metrics {
    prev_idle: u64,
    prev_total: u64,
    cpu: History,
    ram: History,
    net: Net,
}

impl Metrics {
    pub fn sample(&mut self, preferred_interface: &str) {
        if let Some(percent) = self.read_cpu() {
            self.cpu.push(percent as u64);
        }
        if let Some(percent) = memory_load() {
            self.ram.push(percent as u64);
        }
        self.net.sample(preferred_interface);
    }

    pub fn rows(&self) -> [Row; LINE_COUNT] {
        let (rx_full, tx_full) = (
            self.net.rx.peak().max(NET_SCALE_FLOOR),
            self.net.tx.peak().max(NET_SCALE_FLOOR),
        );
        [
            self.cpu.row("CPU", percent_text(self.cpu.latest()), 100),
            self.ram.row("RAM", percent_text(self.ram.latest()), 100),
            self.net.rx.row("NET\u{2193}", rate_text(self.net.rx.latest()), rx_full),
            self.net.tx.row("NET\u{2191}", rate_text(self.net.tx.latest()), tx_full),
        ]
    }

    fn read_cpu(&mut self) -> Option<u32> {
        let (mut idle, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default());
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;

        let idle = filetime_to_u64(idle);
        // Kernel time already includes idle time; user time does not.
        let total = filetime_to_u64(kernel) + filetime_to_u64(user);

        let (prev_idle, prev_total) = (self.prev_idle, self.prev_total);
        (self.prev_idle, self.prev_total) = (idle, total);

        if prev_total == 0 {
            return None;
        }
        load_percent(idle.saturating_sub(prev_idle), total.saturating_sub(prev_total))
    }
}

fn load_percent(idle_delta: u64, total_delta: u64) -> Option<u32> {
    if total_delta == 0 {
        return None;
    }
    let busy = total_delta.saturating_sub(idle_delta);
    Some(((busy * 100 / total_delta) as u32).min(100))
}

/// Physical memory in use, percent.
fn memory_load() -> Option<u32> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(status.dwMemoryLoad.min(100))
}

/// Byte counters of one interface, and the two rate histories derived from them.
#[derive(Debug, Default)]
struct Net {
    /// `(interface luid, in octets, out octets)` from the previous tick. The
    /// luid is part of it because a rate computed across two different
    /// interfaces is not a rate at all.
    prev: Option<(u64, u64, u64)>,
    rx: History,
    tx: History,
}

impl Net {
    fn sample(&mut self, preferred: &str) {
        let Some(current) = (unsafe { select_interface(preferred) }) else {
            self.prev = None;
            return;
        };

        if let Some((luid, prev_in, prev_out)) = self.prev {
            if luid == current.0 {
                self.rx.push(per_second(current.1.saturating_sub(prev_in)));
                self.tx.push(per_second(current.2.saturating_sub(prev_out)));
            }
        }
        self.prev = Some(current);
    }
}

fn per_second(bytes_this_tick: u64) -> u64 {
    bytes_this_tick * 1000 / SYSTEM_TICK_MS as u64
}

fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Picks the interface to measure, as `(luid, in octets, out octets)`.
///
/// With no preference it takes the busiest *physical* adapter: the hardware and
/// connector flags are what separate a real card from a VPN tunnel, a
/// hypervisor switch or a TAP device. That is not only cosmetic — a tunnel runs
/// over the card it tunnels through, so counting both would report the same
/// bytes twice.
///
/// A non-empty `preferred` matches against the adapter alias and description
/// instead, which is the way to deliberately watch a tunnel.
unsafe fn select_interface(preferred: &str) -> Option<(u64, u64, u64)> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    if GetIfTable2(&mut table) != NO_ERROR || table.is_null() {
        return None;
    }

    let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
    let wanted = preferred.trim().to_lowercase();

    let mut best: Option<(u64, u64, u64)> = None;
    for row in rows {
        if row.OperStatus != IfOperStatusUp || row.MediaConnectState != MediaConnectStateConnected {
            continue;
        }

        let flags = row.InterfaceAndOperStatusFlags._bitfield;
        let wanted_here = if wanted.is_empty() {
            flags & (FLAG_HARDWARE | FLAG_CONNECTOR) == FLAG_HARDWARE | FLAG_CONNECTOR
        } else {
            wide_to_string(&row.Alias).to_lowercase().contains(&wanted)
                || wide_to_string(&row.Description).to_lowercase().contains(&wanted)
        };
        if !wanted_here {
            continue;
        }

        let total = row.InOctets.saturating_add(row.OutOctets);
        if best.is_none_or(|(_, i, o)| total > i.saturating_add(o)) {
            best = Some((row.InterfaceLuid.Value, row.InOctets, row.OutOctets));
        }
    }

    FreeMibTable(table as *const core::ffi::c_void);
    best
}

/// Padded to a fixed width so the value column does not jitter as the numbers
/// change; `render` sizes that column to the widest row.
fn percent_text(percent: Option<u64>) -> String {
    match percent {
        Some(p) => format!("{:>3}%", p),
        None => "  --".to_string(),
    }
}

fn rate_text(rate: Option<u64>) -> String {
    // Built with the same format as a real rate, so the placeholder cannot end
    // up a character narrower and shift the column on the first tick.
    let Some(rate) = rate else {
        return format!("{:>6} {:<4}", "--", "");
    };
    const KB: u64 = 1 << 10;
    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    let (value, unit) = match rate {
        r if r >= GB => (r as f64 / GB as f64, "GB/s"),
        r if r >= MB => (r as f64 / MB as f64, "MB/s"),
        _ => (rate as f64 / KB as f64, "KB/s"),
    };
    format!("{:>6.1} {:<4}", value, unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_load_from_deltas() {
        assert_eq!(load_percent(0, 0), None); // no time passed
        assert_eq!(load_percent(1000, 1000), Some(0)); // fully idle
        assert_eq!(load_percent(0, 1000), Some(100)); // pegged
        assert_eq!(load_percent(750, 1000), Some(25));
        assert_eq!(load_percent(2000, 1000), Some(0)); // idle > total: clamped, not underflowed
    }

    #[test]
    fn history_keeps_the_newest_samples_and_never_grows() {
        let mut h = History::default();
        assert_eq!(h.latest(), None);
        assert_eq!(h.normalized(100), Vec::<u32>::new());

        for i in 0..HISTORY_LEN as u64 * 2 {
            h.push(i);
        }
        assert_eq!(h.0.len(), HISTORY_LEN);
        assert_eq!(h.latest(), Some(HISTORY_LEN as u64 * 2 - 1));
        assert_eq!(h.peak(), HISTORY_LEN as u64 * 2 - 1);
    }

    #[test]
    fn normalizing_rescales_the_whole_history_against_the_scale() {
        let mut h = History::default();
        for v in [0, 25, 50, 100] {
            h.push(v);
        }
        assert_eq!(h.normalized(100), vec![0, 25, 50, 100]);
        // A new peak restates every earlier sample, which is the point of
        // keeping raw values instead of percentages.
        assert_eq!(h.normalized(200), vec![0, 12, 25, 50]);
        // Values above the scale clamp instead of overflowing the chart.
        assert_eq!(h.normalized(50), vec![0, 50, 100, 100]);
        // A zero scale must not divide by zero.
        assert_eq!(h.normalized(0), vec![0, 100, 100, 100]);
    }

    #[test]
    fn rates_read_in_sensible_units_at_a_fixed_width() {
        assert_eq!(rate_text(Some(0)), "   0.0 KB/s");
        assert_eq!(rate_text(Some(1536)), "   1.5 KB/s");
        assert_eq!(rate_text(Some(12 << 20)), "  12.0 MB/s");
        assert_eq!(rate_text(Some(3 << 30)), "   3.0 GB/s");

        let widths: Vec<usize> = [None, Some(0), Some(999 << 20), Some(5 << 30)]
            .into_iter()
            .map(|r| rate_text(r).chars().count())
            .collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{:?}", widths);
    }

    #[test]
    fn a_tick_of_bytes_is_a_rate_per_second() {
        // The chart labels itself per second, so the per-tick delta has to be
        // converted; at a one-second tick that is the identity.
        assert_eq!(per_second(4096), 4096 * 1000 / SYSTEM_TICK_MS as u64);
    }
}
