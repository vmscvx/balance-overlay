//! CPU and memory load, straight from Win32 — no crate needed for two calls.

use std::collections::VecDeque;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

use crate::render::Row;

/// Samples kept for the CPU chart. One per tick, so at `SYSTEM_TICK_MS` this is
/// also the number of seconds of history on screen.
pub const HISTORY_LEN: usize = 60;

/// Display slots the system block occupies. Startup reserves this many at the
/// tail of `state.lines`, and the timer overwrites exactly those.
pub const LINE_COUNT: usize = 2;

fn filetime_to_u64(ft: FILETIME) -> u64 {
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

/// The previous `GetSystemTimes` reading plus the recent load history. CPU load
/// only exists as a difference between two readings, so the first sample has
/// nothing to report and the chart starts empty.
#[derive(Debug, Default)]
pub struct CpuSampler {
    prev_idle: u64,
    prev_total: u64,
    history: VecDeque<u32>,
}

impl CpuSampler {
    /// Takes a reading and appends it to the history. Records nothing on the
    /// first call, or if the clock did not move between calls.
    pub fn sample(&mut self) {
        if let Some(percent) = self.read() {
            self.push(percent);
        }
    }

    fn push(&mut self, percent: u32) {
        if self.history.len() == HISTORY_LEN {
            self.history.pop_front();
        }
        self.history.push_back(percent.min(100));
    }

    pub fn latest(&self) -> Option<u32> {
        self.history.back().copied()
    }

    /// Oldest first, so the renderer can walk it left to right.
    pub fn history(&self) -> Vec<u32> {
        self.history.iter().copied().collect()
    }

    fn read(&mut self) -> Option<u32> {
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
pub fn memory_load() -> Option<u32> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(status.dwMemoryLoad.min(100))
}

pub fn rows(cpu: &CpuSampler, ram: Option<u32>) -> [Row; LINE_COUNT] {
    [
        Row::Graph {
            label: "CPU",
            percent: cpu.latest(),
            history: cpu.history(),
            capacity: HISTORY_LEN,
        },
        Row::Bar {
            label: "RAM",
            percent: ram,
        },
    ]
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
        let mut s = CpuSampler::default();
        assert_eq!(s.history(), Vec::<u32>::new());
        assert_eq!(s.latest(), None);

        for i in 0..HISTORY_LEN as u32 * 2 {
            s.push(i % 101);
        }
        let kept = s.history();
        assert_eq!(kept.len(), HISTORY_LEN);
        assert_eq!(s.latest(), kept.last().copied());
        // Oldest first: the window is the tail of what was pushed.
        let expected: Vec<u32> = (HISTORY_LEN as u32..HISTORY_LEN as u32 * 2)
            .map(|i| i % 101)
            .collect();
        assert_eq!(kept, expected);
    }

    #[test]
    fn samples_are_clamped_to_a_percentage() {
        let mut s = CpuSampler::default();
        s.push(500);
        assert_eq!(s.latest(), Some(100));
    }
}
