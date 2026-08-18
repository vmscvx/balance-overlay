//! CPU and memory load, straight from Win32 — no crate needed for two calls.

use std::collections::VecDeque;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

use crate::render::Row;

/// Samples kept per chart. One per tick, so at `SYSTEM_TICK_MS` this is also
/// the number of seconds of history on screen.
pub const HISTORY_LEN: usize = 60;

/// Display slots the system block occupies. Startup reserves this many at the
/// tail of `state.lines`, and the timer overwrites exactly those.
pub const LINE_COUNT: usize = 2;

#[derive(Debug, Default)]
struct History(VecDeque<u32>);

impl History {
    fn push(&mut self, percent: u32) {
        if self.0.len() == HISTORY_LEN {
            self.0.pop_front();
        }
        self.0.push_back(percent.min(100));
    }

    fn latest(&self) -> Option<u32> {
        self.0.back().copied()
    }

    /// Oldest first, so the renderer can walk it left to right.
    fn snapshot(&self) -> Vec<u32> {
        self.0.iter().copied().collect()
    }

    fn row(&self, label: &'static str) -> Row {
        Row::Graph {
            label,
            percent: self.latest(),
            history: self.snapshot(),
            capacity: HISTORY_LEN,
        }
    }
}

fn filetime_to_u64(ft: FILETIME) -> u64 {
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

/// Both load histories, plus the previous `GetSystemTimes` reading. CPU load
/// only exists as a difference between two readings, so its chart stays empty
/// for one tick; memory is absolute and shows up immediately.
#[derive(Debug, Default)]
pub struct Metrics {
    prev_idle: u64,
    prev_total: u64,
    cpu: History,
    ram: History,
}

impl Metrics {
    pub fn sample(&mut self) {
        if let Some(percent) = self.read_cpu() {
            self.cpu.push(percent);
        }
        if let Some(percent) = memory_load() {
            self.ram.push(percent);
        }
    }

    pub fn rows(&self) -> [Row; LINE_COUNT] {
        [self.cpu.row("CPU"), self.ram.row("RAM")]
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
        assert_eq!(h.snapshot(), Vec::<u32>::new());
        assert_eq!(h.latest(), None);

        for i in 0..HISTORY_LEN as u32 * 2 {
            h.push(i % 101);
        }
        let kept = h.snapshot();
        assert_eq!(kept.len(), HISTORY_LEN);
        assert_eq!(h.latest(), kept.last().copied());
        // Oldest first: the window is the tail of what was pushed.
        let expected: Vec<u32> = (HISTORY_LEN as u32..HISTORY_LEN as u32 * 2)
            .map(|i| i % 101)
            .collect();
        assert_eq!(kept, expected);
    }

    #[test]
    fn samples_are_clamped_to_a_percentage() {
        let mut h = History::default();
        h.push(500);
        assert_eq!(h.latest(), Some(100));
    }
}
