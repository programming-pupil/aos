#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRepairStrategy {
    SwitchSource,
    SwitchQuery,
    BrowserFallback,
    PartialEvidenceBackfill,
    DegradedSummary,
}

impl PmRepairStrategy {
    pub fn as_key(self) -> &'static str {
        match self {
            Self::SwitchSource => "switch_source",
            Self::SwitchQuery => "switch_query",
            Self::BrowserFallback => "browser_fallback",
            Self::PartialEvidenceBackfill => "partial_evidence_backfill",
            Self::DegradedSummary => "degraded_summary",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::SwitchSource => {
                "Switch to a different source immediately, keep question intent stable."
            }
            Self::SwitchQuery => {
                "Keep source family but rewrite query phrasing/locale to reduce sparse hits."
            }
            Self::BrowserFallback => {
                "Search-only mode: switch source/query to bypass endpoint instability."
            }
            Self::PartialEvidenceBackfill => {
                "Only backfill unresolved claims; do not restart full retrieval pipeline."
            }
            Self::DegradedSummary => {
                "Stop retrieval expansion and return final conclusion with explicit evidence gaps."
            }
        }
    }

    pub fn for_attempt(next_attempt: usize) -> Self {
        match next_attempt {
            2 => Self::SwitchSource,
            3 => Self::SwitchQuery,
            4 => Self::PartialEvidenceBackfill,
            _ => Self::DegradedSummary,
        }
    }
}
