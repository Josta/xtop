//! Window statistics over recorded visual totals, not sums of process peaks.
use crate::model::{Metrics, Point};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Measurement {
    #[default]
    Live,
    Average1m,
    Average5m,
    Peak1m,
    Peak5m,
}
impl Measurement {
    pub fn next(self) -> Self {
        match self {
            Self::Live => Self::Average1m,
            Self::Average1m => Self::Average5m,
            Self::Average5m => Self::Peak1m,
            Self::Peak1m => Self::Peak5m,
            Self::Peak5m => Self::Live,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Average1m => "1 min average",
            Self::Average5m => "5 min average",
            Self::Peak1m => "1 min peak",
            Self::Peak5m => "5 min peak",
        }
    }
    pub fn window(self) -> Duration {
        Duration::from_secs(match self {
            Self::Average5m | Self::Peak5m => 300,
            _ => 60,
        })
    }
    pub fn is_peak(self) -> bool {
        matches!(self, Self::Peak1m | Self::Peak5m)
    }
}

#[derive(Clone, Copy)]
pub enum Metric {
    Cpu,
    Ram,
    Swap,
    Footprint,
}
impl Metric {
    pub fn sample(self, m: &Metrics) -> Option<(f64, bool)> {
        let (value, covered) = match self {
            Self::Cpu => (m.cpu, m.cpu_covered),
            Self::Ram => (m.pss as f64, m.covered),
            Self::Swap => (m.swap as f64, m.covered),
            Self::Footprint => (m.pss as f64 + m.swap as f64, m.covered),
        };
        (covered > 0).then_some((value, covered < m.count))
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub average: Option<f64>,
    pub peak: Option<f64>,
    pub partial: bool,
}

pub fn summarize(
    points: &VecDeque<Point>,
    now: Instant,
    window: Duration,
    max_gap: Duration,
    metric: Metric,
) -> Stats {
    let start = now - window;
    let mut peak: Option<f64> = None;
    let mut weighted = 0.0;
    let mut covered_seconds = 0.0;
    let mut partial = false;
    for point in points.iter().filter(|p| {
        p.at <= now && (p.at > start || (!matches!(metric, Metric::Cpu) && p.at == start))
    }) {
        if let Some((value, incomplete)) = metric.sample(&point.metrics) {
            peak = Some(peak.map_or(value, |p| p.max(value)));
            partial |= incomplete;
        } else {
            partial = true;
        }
    }
    for (left, right) in points.iter().zip(points.iter().skip(1)) {
        let from = left.at.max(start);
        let until = right.at.min(now);
        if until <= from || right.at.duration_since(left.at) > max_gap {
            continue;
        }
        // CPU samples describe the preceding collection interval; memory is a
        // sampled gauge held until the next measurement/collection.
        let sample = match metric {
            Metric::Cpu => right,
            _ => left,
        };
        if let Some((value, incomplete)) = metric.sample(&sample.metrics) {
            let seconds = until.duration_since(from).as_secs_f64();
            weighted += value * seconds;
            covered_seconds += seconds;
            partial |= incomplete;
            peak = Some(peak.map_or(value, |p| p.max(value)));
        }
    }
    Stats {
        average: (covered_seconds > 0.0).then(|| weighted / covered_seconds),
        peak,
        partial: partial || covered_seconds + 0.001 < window.as_secs_f64(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn point(at: Instant, value: u64, covered: usize) -> Point {
        Point {
            at,
            metrics: Metrics {
                count: 1,
                covered,
                cpu_covered: covered,
                cpu: value as f64,
                pss: value,
                swap: value,
                ..Default::default()
            },
        }
    }
    #[test]
    fn modes_cycle_in_order_and_footprint_peak_uses_simultaneous_totals() {
        let mut mode = Measurement::Live;
        for expected in [
            Measurement::Average1m,
            Measurement::Average5m,
            Measurement::Peak1m,
            Measurement::Peak5m,
            Measurement::Live,
        ] {
            mode = mode.next();
            assert_eq!(mode, expected);
        }
        let now = Instant::now();
        let mut a = point(now - Duration::from_secs(60), 100, 1);
        a.metrics.swap = 0;
        let mut b = point(now, 0, 1);
        b.metrics.swap = 100;
        let h = VecDeque::from([a, b]);
        let stats = summarize(
            &h,
            now,
            Duration::from_secs(60),
            Duration::from_secs(60),
            Metric::Footprint,
        );
        assert_eq!(stats.peak, Some(100.0));
        assert_eq!(stats.average, Some(100.0));
        assert!(!stats.partial);
    }

    #[test]
    fn averages_are_time_weighted_and_clip_the_window() {
        let now = Instant::now();
        let h = VecDeque::from([
            point(now - Duration::from_secs(70), 10, 1),
            point(now - Duration::from_secs(50), 20, 1),
            point(now, 40, 1),
        ]);
        let ram = summarize(
            &h,
            now,
            Duration::from_secs(60),
            Duration::from_secs(60),
            Metric::Ram,
        );
        assert!((ram.average.unwrap() - 1100.0 / 60.0).abs() < 1e-9);
        assert_eq!(ram.peak, Some(40.0));
        assert!(!ram.partial);
        let cpu = summarize(
            &h,
            now,
            Duration::from_secs(60),
            Duration::from_secs(60),
            Metric::Cpu,
        );
        assert!((cpu.average.unwrap() - 2200.0 / 60.0).abs() < 1e-9);
        let boundary = VecDeque::from([
            point(now - Duration::from_secs(60), 999, 1),
            point(now - Duration::from_secs(50), 20, 1),
            point(now, 40, 1),
        ]);
        let cpu = summarize(
            &boundary,
            now,
            Duration::from_secs(60),
            Duration::from_secs(60),
            Metric::Cpu,
        );
        assert_eq!(cpu.peak, Some(40.0)); // boundary CPU belongs to the earlier interval
        assert!(!cpu.partial);
    }
    #[test]
    fn gaps_missing_and_partial_samples_do_not_become_zero() {
        let now = Instant::now();
        let h = VecDeque::from([
            point(now - Duration::from_secs(60), 10, 1),
            point(now - Duration::from_secs(59), 20, 0),
            point(now, 90, 1),
        ]);
        let ram = summarize(
            &h,
            now,
            Duration::from_secs(60),
            Duration::from_secs(3),
            Metric::Ram,
        );
        assert_eq!(ram.average, Some(10.0));
        assert_eq!(ram.peak, Some(90.0));
        assert!(ram.partial);
        let cpu = summarize(
            &h,
            now,
            Duration::from_secs(60),
            Duration::from_secs(3),
            Metric::Cpu,
        );
        assert_eq!(cpu.average, None);
        assert!(cpu.partial);
        let unknown = VecDeque::from([point(now, 0, 0)]);
        assert_eq!(
            summarize(
                &unknown,
                now,
                Duration::from_secs(300),
                Duration::from_secs(3),
                Metric::Swap
            )
            .peak,
            None
        );
    }
    #[test]
    fn peaks_follow_aggregate_snapshots_and_new_history_is_partial() {
        let now = Instant::now();
        let mut h = VecDeque::from([
            point(now - Duration::from_secs(1), 100, 1),
            point(now, 100, 1),
        ]);
        let stats = summarize(
            &h,
            now,
            Duration::from_secs(300),
            Duration::from_secs(3),
            Metric::Ram,
        );
        assert_eq!(stats.peak, Some(100.0));
        assert!(stats.partial);
        h[0].metrics.count = 2;
        let stats = summarize(
            &h,
            now,
            Duration::from_secs(1),
            Duration::from_secs(3),
            Metric::Ram,
        );
        assert!(stats.partial);
    }
}
