use crate::status::Status;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Component {
    pub name: String,
    pub status: Status,
    pub critical: bool,
    #[serde(default)]
    pub message: String,
}

impl Component {
    pub fn new(
        name: impl Into<String>,
        status: Status,
        critical: bool,
        message: impl Into<String>,
    ) -> Self {
        Component {
            name: name.into(),
            status,
            critical,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckReport {
    pub status: Status,
    pub message: String,
    #[serde(default)]
    pub components: Vec<Component>,
}

impl CheckReport {
    pub fn new(status: Status, message: impl Into<String>) -> Self {
        CheckReport {
            status,
            message: message.into(),
            components: Vec::new(),
        }
    }

    pub fn ok(message: impl Into<String>) -> Self {
        CheckReport::new(Status::Ok, message)
    }

    pub fn from_components(components: Vec<Component>) -> Self {
        let (status, message) = rollup(&components);
        CheckReport {
            status,
            message,
            components,
        }
    }
}

/// Reduce a component's raw status to its effective contribution given
/// its criticality (non-critical failures are capped at Degraded).
fn effective(status: Status, critical: bool) -> Status {
    match (status, critical) {
        (Status::Ok, _) => Status::Ok,
        (Status::Degraded, _) => Status::Degraded,
        (Status::Critical, true) => Status::Critical,
        (Status::Critical, false) => Status::Degraded,
        (Status::Unknown, true) => Status::Unknown,
        (Status::Unknown, false) => Status::Degraded,
    }
}

/// Roll up components into a single (status, message). The parent status is
/// the worst effective status; the message names the driving components.
pub fn rollup(components: &[Component]) -> (Status, String) {
    let mut worst = Status::Ok;
    for comp in components {
        let eff = effective(comp.status, comp.critical);
        if eff.rank() > worst.rank() {
            worst = eff;
        }
    }

    if worst == Status::Ok {
        return (Status::Ok, "all components ok".to_string());
    }

    let drivers: Vec<String> = components
        .iter()
        .filter(|c| effective(c.status, c.critical) == worst)
        .map(|c| c.name.clone())
        .collect();

    (worst, format!("{:?}: {}", worst, drivers.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, status: Status, critical: bool) -> Component {
        Component::new(name, status, critical, "detail")
    }

    #[test]
    fn all_ok_rolls_up_to_ok() {
        let (status, _) = rollup(&[c("a", Status::Ok, true), c("b", Status::Ok, false)]);
        assert_eq!(status, Status::Ok);
    }

    #[test]
    fn critical_component_makes_parent_critical() {
        let (status, msg) = rollup(&[
            c("a", Status::Ok, true),
            c("driveway", Status::Critical, true),
        ]);
        assert_eq!(status, Status::Critical);
        assert!(msg.contains("driveway"));
    }

    #[test]
    fn noncritical_critical_component_caps_at_degraded() {
        let (status, _) = rollup(&[c("detector", Status::Critical, false)]);
        assert_eq!(status, Status::Degraded);
    }

    #[test]
    fn noncritical_unknown_degrades() {
        let (status, _) = rollup(&[c("x", Status::Unknown, false)]);
        assert_eq!(status, Status::Degraded);
    }

    #[test]
    fn critical_unknown_surfaces_as_unknown() {
        let (status, _) = rollup(&[c("x", Status::Unknown, true)]);
        assert_eq!(status, Status::Unknown);
    }

    #[test]
    fn critical_beats_unknown() {
        let (status, _) = rollup(&[
            c("a", Status::Unknown, true),
            c("b", Status::Critical, true),
        ]);
        assert_eq!(status, Status::Critical);
    }

    #[test]
    fn from_components_sets_rolled_up_status() {
        let report = CheckReport::from_components(vec![c("cam", Status::Critical, true)]);
        assert_eq!(report.status, Status::Critical);
        assert_eq!(report.components.len(), 1);
    }
}
