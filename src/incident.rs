use crate::status::Status;
use crate::store::TransitionRow;
use serde::Serialize;

/// Incidents inline in `/api/v1/status` cover the last 24h, newest 5. Fixed
/// constants rather than query parameters: consumers wanting more use
/// `/api/v1/incidents`, which is where the expensive detail lives.
pub const RECENT_WINDOW_SECS: i64 = 86_400;
pub const RECENT_LIMIT: usize = 5;

/// A maximal contiguous period of non-Ok committed status.
///
/// `duration_secs` is derivable from the timestamps, but is included anyway:
/// for an ongoing incident the consumer would otherwise have to know the
/// server's clock to compute it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Incident {
    /// May precede the query window — an incident is never truncated at a
    /// query boundary, because that would misreport its duration.
    pub started_at: i64,
    /// `None` while ongoing. There is deliberately no separate `resolved`
    /// flag: two fields that can disagree is a bug waiting to happen.
    pub ended_at: Option<i64>,
    pub duration_secs: i64,
    pub worst_status: Status,
    /// Rollup message from the transition where `worst_status` was first
    /// reached.
    pub message: String,
}

/// A component that was non-Ok at some point during an incident, folded across
/// every sample in the incident's span. Healthy components during an outage
/// are noise and are omitted entirely.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FailingComponent {
    pub name: String,
    pub worst_status: Status,
    pub critical: bool,
    /// The component's message at its worst moment; earlier wins on ties.
    pub message: String,
    /// First/last sample in the incident where this component was non-Ok.
    /// Distinguishes "both failed together" from "disk3 was down the whole
    /// outage and free space only blipped at the end".
    pub first_seen: i64,
    pub last_seen: i64,
}

/// An incident as returned by `/api/v1/incidents`: identified by monitor and
/// carrying the per-component detail that the inline `/status` shape omits.
#[derive(Debug, Clone, Serialize)]
pub struct IncidentDetail {
    pub monitor_id: i64,
    pub monitor_name: String,
    #[serde(flatten)]
    pub incident: Incident,
    pub failing_components: Vec<FailingComponent>,
}

/// Group committed status transitions into incidents.
///
/// `prior` is the transition that *opened* the incident in force at the window
/// start — the earliest non-Ok transition after the last Ok at or before it,
/// not merely the last transition before it. A monitor can commit several
/// non-Ok transitions in a row with no intervening Ok (the scheduler's debounce
/// is in-memory, so a restart re-commits the current status), and seeding from
/// the tail of that run would report `started_at` too late, differently for
/// every window size. See `Store::open_incident_start`.
///
/// `transitions` must be ascending by `at`; out-of-order input misgroups
/// incidents. `failing_components` is deliberately not computed here — it comes
/// from a sample scan in the store, keeping this grouping pure and DB-free.
///
/// Returns newest first.
///
/// A `prior` of `None` means no incident was open at the window start: either
/// the monitor was Ok, or nothing was ever recorded and its status is unknown
/// *by absence*. Neither opens an incident. The latter diverges from
/// `compute_uptime`, which honestly counts that span as `unknown_secs`: absence
/// of data is not evidence of failure, and a brand-new monitor must not fake an
/// outage.
pub fn compute_incidents(
    prior: Option<&TransitionRow>,
    transitions: &[TransitionRow],
    now: i64,
) -> Vec<Incident> {
    let mut incidents = Vec::new();
    // The incident currently open, as (started_at, worst_status, message).
    let mut open: Option<(i64, Status, &str)> = None;

    if let Some(p) = prior.filter(|p| p.status != Status::Ok) {
        open = Some((p.at, p.status, &p.message));
    }

    for row in transitions {
        match (row.status, open.as_mut()) {
            // Recovery closes whatever is open. An Ok with nothing open can
            // only happen if the caller passed a non-transition; ignore it.
            (Status::Ok, _) => {
                if let Some((started_at, worst_status, message)) = open.take() {
                    incidents.push(Incident {
                        started_at,
                        ended_at: Some(row.at),
                        duration_secs: (row.at - started_at).max(0),
                        worst_status,
                        message: message.to_string(),
                    });
                }
            }
            // Escalation merges into the open incident rather than starting a
            // new one: degraded -> critical -> ok is one outage, not two. Only
            // a strictly worse status takes the message, so ties keep the
            // earlier line.
            (status, Some((_, worst_status, message))) => {
                if status.rank() > worst_status.rank() {
                    *worst_status = status;
                    *message = &row.message;
                }
            }
            (status, None) => open = Some((row.at, status, &row.message)),
        }
    }

    if let Some((started_at, worst_status, message)) = open {
        incidents.push(Incident {
            started_at,
            ended_at: None,
            duration_secs: (now - started_at).max(0),
            worst_status,
            message: message.to_string(),
        });
    }

    incidents.reverse();
    incidents
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(status: Status, message: &str, at: i64) -> TransitionRow {
        TransitionRow {
            status,
            message: message.into(),
            at,
        }
    }

    #[test]
    fn escalation_merges_into_one_incident() {
        // Degraded 100 -> Critical 200 -> Ok 400 is ONE incident of 300s, and
        // the message is the one from the escalation, not from the open.
        let incidents = compute_incidents(
            Some(&t(Status::Ok, "up", 0)),
            &[
                t(Status::Degraded, "slow", 100),
                t(Status::Critical, "2 of 8 components unhealthy", 200),
                t(Status::Ok, "recovered", 400),
            ],
            500,
        );
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].started_at, 100);
        assert_eq!(incidents[0].ended_at, Some(400));
        assert_eq!(incidents[0].duration_secs, 300);
        assert_eq!(incidents[0].worst_status, Status::Critical);
        assert_eq!(incidents[0].message, "2 of 8 components unhealthy");
    }

    #[test]
    fn severity_tie_keeps_the_earlier_message() {
        // Critical twice at the same severity: the first line is the one that
        // explains how it got that bad, so it wins.
        let incidents = compute_incidents(
            Some(&t(Status::Ok, "up", 0)),
            &[
                t(Status::Critical, "disk3 failing", 100),
                t(Status::Critical, "disk3 and disk4 failing", 200),
                t(Status::Ok, "recovered", 300),
            ],
            400,
        );
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].message, "disk3 failing");
    }

    #[test]
    fn flap_produces_two_incidents() {
        let incidents = compute_incidents(
            Some(&t(Status::Ok, "up", 0)),
            &[
                t(Status::Critical, "down", 100),
                t(Status::Ok, "up", 150),
                t(Status::Critical, "down again", 200),
                t(Status::Ok, "up", 250),
            ],
            300,
        );
        assert_eq!(incidents.len(), 2);
        // newest first
        assert_eq!(incidents[0].started_at, 200);
        assert_eq!(incidents[1].started_at, 100);
    }

    #[test]
    fn ongoing_incident_has_null_end_and_duration_to_now() {
        let incidents = compute_incidents(
            Some(&t(Status::Ok, "up", 0)),
            &[t(Status::Critical, "down", 100)],
            460,
        );
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].ended_at, None);
        assert_eq!(incidents[0].duration_secs, 360);
    }

    #[test]
    fn prior_non_ok_opens_at_its_real_start_before_the_window() {
        // Window starts at 1000 but the outage began at 700; the incident is
        // reported whole rather than truncated at the window boundary.
        let incidents = compute_incidents(
            Some(&t(Status::Critical, "down", 700)),
            &[t(Status::Ok, "recovered", 1200)],
            1300,
        );
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].started_at, 700);
        assert_eq!(incidents[0].ended_at, Some(1200));
        assert_eq!(incidents[0].duration_secs, 500);
        assert_eq!(incidents[0].message, "down");
    }

    #[test]
    fn re_committed_status_inside_the_window_does_not_move_the_start() {
        // A restart re-commits the status the monitor is already in. That row
        // must fold into the open incident rather than restating when it began.
        let incidents = compute_incidents(
            Some(&t(Status::Degraded, "disk3 failing", 700)),
            &[
                t(Status::Degraded, "disk3 failing", 1100),
                t(Status::Ok, "recovered", 1200),
            ],
            1300,
        );
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].started_at, 700);
        assert_eq!(incidents[0].duration_secs, 500);
    }

    #[test]
    fn brand_new_monitor_produces_no_incident() {
        // No transition at or before the window start: status is unknown by
        // absence, and absence of data is not evidence of failure.
        assert!(compute_incidents(None, &[], 1000).is_empty());
    }

    #[test]
    fn unknown_opens_an_incident_like_any_other_non_ok_status() {
        // A check that couldn't reach the service at 3am is exactly the outage
        // worth seeing; consumers filter on worst_status if they disagree.
        let incidents = compute_incidents(
            Some(&t(Status::Ok, "up", 0)),
            &[t(Status::Unknown, "unreachable", 100)],
            200,
        );
        assert_eq!(incidents.len(), 1);
        assert_eq!(incidents[0].worst_status, Status::Unknown);
        assert_eq!(incidents[0].message, "unreachable");
    }
}
