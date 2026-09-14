//! Validated daily recording windows, including overnight and multiple time slots.
use chrono::{Local, NaiveTime, Timelike};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    start: u32,
    duration: u32,
}
pub fn parse(starts: Option<&str>, hours: Option<&str>) -> Result<Vec<Window>, String> {
    let starts = starts
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let hours = hours
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if starts.is_empty() || starts.len() > 16 || !(hours.len() == 1 || hours.len() == starts.len())
    {
        return Err("定时录制需要有效的开始时间和对应时长".into());
    }
    starts
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let time = NaiveTime::parse_from_str(s, "%H:%M:%S")
                .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M"))
                .map_err(|_| "开始时间应为 HH:MM，多个时间用逗号分隔")?;
            let n = hours[if hours.len() == 1 { 0 } else { i }]
                .parse::<f64>()
                .map_err(|_| "监控时长应为小时数")?;
            if !n.is_finite() || n <= 0.0 || n > 24.0 {
                return Err("监控时长必须大于 0 且不超过 24 小时".into());
            }
            Ok(Window {
                start: time.num_seconds_from_midnight(),
                duration: (n * 3600.0).ceil() as u32,
            })
        })
        .collect()
}
pub fn contains(windows: &[Window], time: NaiveTime) -> bool {
    let t = time.num_seconds_from_midnight();
    windows
        .iter()
        .any(|w| (t + 86400 - w.start) % 86400 < w.duration)
}
pub fn active(record: &crate::model::Recording) -> Result<bool, String> {
    if record.scheduled_recording != Some(true) {
        return Ok(true);
    }
    Ok(contains(
        &parse(
            record.scheduled_start_time.as_deref(),
            record.monitor_hours.as_deref(),
        )?,
        Local::now().time(),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_cross_midnight_and_match_individual_durations() {
        let w = parse(Some("23:00,10:30"), Some("2,0.5")).unwrap();
        for s in ["23:30:00", "00:30:00", "10:45:00"] {
            assert!(contains(
                &w,
                NaiveTime::parse_from_str(s, "%H:%M:%S").unwrap()
            ));
        }
        for s in ["01:00:00", "11:00:00", "21:00:00"] {
            assert!(!contains(
                &w,
                NaiveTime::parse_from_str(s, "%H:%M:%S").unwrap()
            ));
        }
    }
    #[test]
    fn malformed_windows_are_not_silently_ignored() {
        for (a, b) in [
            ("", "3"),
            ("25:00", "1"),
            ("10:00", "0"),
            ("10:00,12:00", "1,2,3"),
            ("10:00", "NaN"),
        ] {
            assert!(parse(Some(a), Some(b)).is_err());
        }
    }
}
