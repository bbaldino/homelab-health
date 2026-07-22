use crate::status::Status;

/// Commits a status change only after `threshold` consecutive matching
/// observations, so a transient blip never flips the committed status.
pub struct Debounce {
    threshold: u32,
    committed: Option<Status>,
    candidate: Option<Status>,
    count: u32,
}

impl Debounce {
    pub fn new(threshold: u32) -> Self {
        Debounce {
            threshold: threshold.max(1),
            committed: None,
            candidate: None,
            count: 0,
        }
    }

    /// Feed one raw observation. Returns Some(status) when the committed
    /// status changes as a result.
    pub fn record(&mut self, status: Status) -> Option<Status> {
        if self.committed == Some(status) {
            self.candidate = None;
            self.count = 0;
            return None;
        }
        if self.candidate == Some(status) {
            self.count += 1;
        } else {
            self.candidate = Some(status);
            self.count = 1;
        }
        if self.count >= self.threshold {
            self.committed = Some(status);
            self.candidate = None;
            self.count = 0;
            Some(status)
        } else {
            None
        }
    }

    pub fn committed(&self) -> Option<Status> {
        self.committed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_after_threshold_consecutive() {
        let mut d = Debounce::new(2);
        assert_eq!(d.record(Status::Ok), None);
        assert_eq!(d.record(Status::Ok), Some(Status::Ok));
        assert_eq!(d.committed(), Some(Status::Ok));
    }

    #[test]
    fn single_blip_does_not_commit() {
        let mut d = Debounce::new(2);
        d.record(Status::Ok);
        d.record(Status::Ok); // committed Ok
        assert_eq!(d.record(Status::Critical), None); // blip
        assert_eq!(d.record(Status::Ok), None); // back to Ok, candidate cleared
        assert_eq!(d.committed(), Some(Status::Ok));
    }

    #[test]
    fn sustained_change_commits() {
        let mut d = Debounce::new(2);
        d.record(Status::Ok);
        d.record(Status::Ok);
        assert_eq!(d.record(Status::Critical), None);
        assert_eq!(d.record(Status::Critical), Some(Status::Critical));
    }

    #[test]
    fn threshold_one_commits_immediately() {
        let mut d = Debounce::new(1);
        assert_eq!(d.record(Status::Degraded), Some(Status::Degraded));
    }
}
