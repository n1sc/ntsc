//! NTSC standard library: `time` module.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::registry;

/// `time.now()` — milliseconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_time_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

#[unsafe(no_mangle)]
pub extern "C" fn ntsc_time_sleep(ms: f64) {
    // A blocking OS sleep on a scheduler worker would wedge it without a
    // suspension point; the guard brings cover instead (see
    // `ntask::scheduler::worker_will_block`).
    let _block = crate::BlockGuard::enter();
    let duration = std::time::Duration::from_millis(ms.max(0.0) as u64);
    std::thread::sleep(duration);
}

/// `time.format(timestamp_ms, fmt)` — milliseconds since the Unix epoch,
/// formatted with strftime-style specifiers; `fmt` defaults to
/// "%Y-%m-%d %H:%M:%S".
#[unsafe(no_mangle)]
pub extern "C" fn ntsc_time_format(timestamp_ms: f64, format_str: i64) -> i64 {
    let fmt = registry::get_string(format_str).unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".to_string());

    let secs = (timestamp_ms / 1000.0) as i64;
    let nanos = ((timestamp_ms as i64) % 1000) * 1_000_000;
    let duration = std::time::Duration::new(secs.max(0) as u64, nanos.max(0) as u32);
    let time = UNIX_EPOCH + duration;

    let datetime = format_time(time, &fmt);
    registry::put_string(datetime)
}

// Hand-rolled Gregorian date math keeps the runtime dependency-free.
fn format_time(time: std::time::SystemTime, fmt: &str) -> String {
    use std::time::Duration;

    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let total_secs = since_epoch.as_secs();

    let days = total_secs / 86400;
    let time_secs = total_secs % 86400;

    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    let mut y = 1970i64;
    let mut remaining_days = days as i64;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        y += 1;
    }

    let month_days = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 0;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining_days < md as i64 {
            m = i + 1;
            break;
        }
        remaining_days -= md as i64;
    }
    if m == 0 {
        m = 12;
    }
    let d = (remaining_days + 1) as u32;

    let mut result = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 1 < chars.len() {
            match chars[i + 1] {
                'Y' => result.push_str(&format!("{:04}", y)),
                'y' => result.push_str(&format!("{:02}", y % 100)),
                'm' => result.push_str(&format!("{:02}", m)),
                'd' => result.push_str(&format!("{:02}", d)),
                'H' => result.push_str(&format!("{:02}", hours)),
                'M' => result.push_str(&format!("{:02}", minutes)),
                'S' => result.push_str(&format!("{:02}", seconds)),
                '%' => result.push('%'),
                _ => {
                    result.push(chars[i]);
                    result.push(chars[i + 1]);
                }
            }
            i += 2;
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    result
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_now() {
        let t = ntsc_time_now();
        assert!(t > 1_700_000_000_000.0);
    }

    #[test]
    fn test_format() {
        let fmt = registry::put_string("%Y-%m-%d %H:%M:%S".to_string());
        let result = ntsc_time_format(0.0, fmt);
        assert_eq!(registry::get_string(result).unwrap(), "1970-01-01 00:00:00");
        registry::take_string(result);
        registry::take_string(fmt);
    }
}
