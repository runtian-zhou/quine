use serde::{Deserialize, Serialize};

pub const DEFAULT_STATUS_REPORT_MIN_TOOL_ROUNDS: u32 = 10;

pub const fn default_status_report_min_tool_rounds() -> u32 {
    DEFAULT_STATUS_REPORT_MIN_TOOL_ROUNDS
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionStatusReport {
    pub active: bool,
    pub progress_percent: u8,
    pub completed_summary: String,
    pub remaining_summary: String,
    pub tool_rounds_observed: u32,
}

impl SessionStatusReport {
    pub fn new(
        active: bool,
        progress_percent: u8,
        completed_summary: impl Into<String>,
        remaining_summary: impl Into<String>,
        tool_rounds_observed: u32,
    ) -> Self {
        Self {
            active,
            progress_percent: progress_percent.min(100),
            completed_summary: completed_summary.into(),
            remaining_summary: remaining_summary.into(),
            tool_rounds_observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{default_status_report_min_tool_rounds, SessionStatusReport};

    #[test]
    fn default_threshold_matches_expected_value() {
        assert_eq!(default_status_report_min_tool_rounds(), 10);
    }

    #[test]
    fn report_clamps_progress_to_100() {
        let report = SessionStatusReport::new(true, 255, "done", "next", 12);
        assert_eq!(report.progress_percent, 100);
    }
}
