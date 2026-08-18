//! CPU and memory load, straight from Win32 — no crate needed for two calls.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

const BAR_CELLS: usize = 10;

fn filetime_to_u64(ft: FILETIME) -> u64 {
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

/// The previous `GetSystemTimes` reading. CPU load only exists as a difference
/// between two of them, so the very first sample has nothing to report.
#[derive(Debug, Default)]
pub struct CpuSampler {
    prev_idle: u64,
    prev_total: u64,
}

impl CpuSampler {
    /// `None` on the first call and if the clock did not move between calls.
    pub fn sample(&mut self) -> Option<u32> {
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
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

fn bar(percent: Option<u32>) -> String {
    let filled = match percent {
        // Round to the nearest cell rather than truncating.
        Some(p) => ((p as usize * BAR_CELLS + 50) / 100).min(BAR_CELLS),
        None => 0,
    };
    "\u{2588}".repeat(filled) + &"\u{2591}".repeat(BAR_CELLS - filled)
}

/// Percentages are padded to a fixed width so the window does not twitch
/// wider and back every second as the numbers change.
fn percent_text(percent: Option<u32>) -> String {
    match percent {
        Some(p) => format!("{:>3}%", p),
        None => "  --".to_string(),
    }
}

/// Display slots the system block occupies. Startup reserves this many at the
/// tail of `state.lines`, and the timer overwrites exactly those.
pub const LINE_COUNT: usize = 2;

pub fn lines(cpu: Option<u32>, ram: Option<u32>) -> [String; LINE_COUNT] {
    [row("CPU", cpu), row("RAM", ram)]
}

fn row(label: &str, percent: Option<u32>) -> String {
    format!("{} {} {}", label, bar(percent), percent_text(percent))
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
    fn bar_fills_proportionally() {
        assert_eq!(bar(Some(0)).chars().filter(|c| *c == '\u{2588}').count(), 0);
        assert_eq!(bar(Some(100)).chars().filter(|c| *c == '\u{2588}').count(), 10);
        assert_eq!(bar(Some(61)).chars().filter(|c| *c == '\u{2588}').count(), 6);
        assert_eq!(bar(None).chars().count(), BAR_CELLS);
        assert_eq!(bar(Some(55)).chars().count(), BAR_CELLS); // width never varies
    }

    #[test]
    fn rows_are_equal_width_whatever_the_numbers() {
        let widths: Vec<usize> = [Some(0), Some(5), Some(61), Some(100), None]
            .into_iter()
            .flat_map(|p| lines(p, p))
            .map(|l| l.chars().count())
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "row width must not depend on the value: {:?}",
            widths
        );
    }
}
