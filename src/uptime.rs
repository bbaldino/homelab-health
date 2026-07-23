use crate::status::Status;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub status: Status,
    pub start: i64,
    pub end: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Uptime {
    pub window_secs: i64,
    pub ok_secs: i64,
    pub degraded_secs: i64,
    pub critical_secs: i64,
    pub unknown_secs: i64,
    pub percent_ok: f64,
    pub segments: Vec<Segment>,
}

/// transitions must be ascending by `at`; out-of-order input misattributes durations.
pub fn compute_uptime(
    prior: Status,
    transitions: &[(Status, i64)],
    window_start: i64,
    now: i64,
) -> Uptime {
    let mut ok = 0i64;
    let mut degraded = 0i64;
    let mut critical = 0i64;
    let mut unknown = 0i64;
    let mut segments = Vec::new();

    let mut current = prior;
    let mut seg_start = window_start;

    let mut add = |status: Status, start: i64, end: i64| {
        let dur = (end - start).max(0);
        match status {
            Status::Ok => ok += dur,
            Status::Degraded => degraded += dur,
            Status::Critical => critical += dur,
            Status::Unknown => unknown += dur,
        }
        segments.push(Segment { status, start, end });
    };

    for (status, at) in transitions {
        let at = (*at).clamp(window_start, now);
        if at > seg_start {
            add(current, seg_start, at);
            seg_start = at;
        }
        current = *status;
    }
    add(current, seg_start, now);

    segments.retain(|s| s.end > s.start);

    let window_secs = (now - window_start).max(0);
    // Percentage is over OBSERVED time (ok+degraded+critical), not wall-clock:
    // "unknown" spans (before monitoring started, or unreachable) don't count as
    // downtime, so a monitor that's been up since we started watching reads ~100%.
    let observed = ok + degraded + critical;
    let percent_ok = if observed > 0 {
        ok as f64 / observed as f64 * 100.0
    } else {
        0.0
    };

    Uptime {
        window_secs,
        ok_secs: ok,
        degraded_secs: degraded,
        critical_secs: critical,
        unknown_secs: unknown,
        percent_ok,
        segments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ok_whole_window_is_100() {
        // prior Ok, no transitions in window; window 100s.
        let u = compute_uptime(Status::Ok, &[], 0, 100);
        assert_eq!(u.ok_secs, 100);
        assert_eq!(u.percent_ok, 100.0);
        assert_eq!(u.segments.len(), 1);
    }

    #[test]
    fn one_outage_splits_and_scores() {
        // Ok from 0..60, Critical 60..100 (transition to Critical at t=60).
        let u = compute_uptime(Status::Ok, &[(Status::Critical, 60)], 0, 100);
        assert_eq!(u.ok_secs, 60);
        assert_eq!(u.critical_secs, 40);
        assert_eq!(u.percent_ok, 60.0);
        assert_eq!(u.segments.len(), 2);
        assert_eq!(u.segments[0].status, Status::Ok);
        assert_eq!(u.segments[1].status, Status::Critical);
    }

    #[test]
    fn no_prior_history_is_unknown() {
        let u = compute_uptime(Status::Unknown, &[], 0, 50);
        assert_eq!(u.unknown_secs, 50);
        assert_eq!(u.percent_ok, 0.0);
    }

    #[test]
    fn unknown_time_excluded_from_percent() {
        // Unknown for the first half (before we were watching), Ok for the second.
        // Of observed time it was 100% ok, even though half the window is unknown.
        let u = compute_uptime(Status::Unknown, &[(Status::Ok, 50)], 0, 100);
        assert_eq!(u.unknown_secs, 50);
        assert_eq!(u.ok_secs, 50);
        assert_eq!(u.percent_ok, 100.0);
    }

    #[test]
    fn transition_at_now_produces_no_zero_width_segment() {
        // Ok whole window; a transition exactly at `now` must not add a 0-width segment.
        let u = compute_uptime(Status::Ok, &[(Status::Critical, 100)], 0, 100);
        assert_eq!(u.segments.len(), 1);
        assert_eq!(u.ok_secs, 100);
        assert_eq!(u.critical_secs, 0);
    }
}
