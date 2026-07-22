use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Degraded,
    Critical,
    Unknown,
}

impl Status {
    /// Severity ordering used to pick the "worst" status during rollup.
    pub fn rank(&self) -> u8 {
        match self {
            Status::Ok => 0,
            Status::Degraded => 1,
            Status::Unknown => 2,
            Status::Critical => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_orders_ok_lowest_critical_highest() {
        assert!(Status::Ok.rank() < Status::Degraded.rank());
        assert!(Status::Degraded.rank() < Status::Unknown.rank());
        assert!(Status::Unknown.rank() < Status::Critical.rank());
    }

    #[test]
    fn serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Status::Ok).unwrap(), "\"ok\"");
        assert_eq!(
            serde_json::to_string(&Status::Critical).unwrap(),
            "\"critical\""
        );
    }
}
