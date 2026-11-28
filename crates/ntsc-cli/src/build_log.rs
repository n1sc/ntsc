//! Cargo-style build status lines, written to stderr as each phase starts.

use std::fmt::Display;
use std::io::{self, IsTerminal};
use std::time::Duration;

use super::{BuildMode, GREEN, colour_on, paint};

pub(crate) struct BuildLog {
    enabled: bool,
    colour: bool,
    phases: Vec<(&'static str, Duration)>,
}

impl BuildLog {
    pub(crate) fn new(json: bool) -> Self {
        Self {
            enabled: !json,
            colour: colour_on(io::stderr().is_terminal()),
            phases: Vec::new(),
        }
    }

    pub(crate) fn status(&self, verb: &str, message: impl Display) {
        if self.enabled {
            eprintln!(
                "{} {message}",
                paint(&format!("{verb:>12}"), GREEN, self.colour)
            );
        }
    }

    pub(crate) fn record(&mut self, phase: &'static str, duration: Duration) {
        if self.enabled {
            self.phases.push((phase, duration));
        }
    }

    pub(crate) fn finish(&self, mode: BuildMode, elapsed: Duration, bytes: Option<u64>) {
        if !self.enabled {
            return;
        }
        let times = self
            .phases
            .iter()
            .map(|(label, duration)| format!("{label} {}", elapsed_time(*duration)))
            .collect::<Vec<_>>()
            .join(", ");
        self.status("Timing", times);
        let optimization = match mode {
            BuildMode::Debug => "unoptimized",
            BuildMode::Release => "optimized",
        };
        let size = bytes
            .map(|n| format!(" ({:.1} MiB binary)", n as f64 / 1_048_576.0))
            .unwrap_or_default();
        self.status(
            "Finished",
            format_args!(
                "`{}` profile [{optimization}] in {}{size}",
                mode.dir_name(),
                elapsed_time(elapsed)
            ),
        );
    }
}

fn elapsed_time(duration: Duration) -> String {
    if duration < Duration::from_millis(1) {
        format!("{} µs", duration.as_micros())
    } else if duration < Duration::from_secs(1) {
        format!("{} ms", duration.as_millis())
    } else {
        format!("{:.2} s", duration.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_show_submillisecond_work_and_long_builds() {
        assert_eq!(elapsed_time(Duration::from_micros(130)), "130 µs");
        assert_eq!(elapsed_time(Duration::from_millis(45)), "45 ms");
        assert_eq!(elapsed_time(Duration::from_millis(5168)), "5.17 s");
    }
}
