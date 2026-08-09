#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmRepairStrategy {
    SwitchSource,
    SwitchQuery,
    BrowserFallback,
    DegradedSummary,
}

impl PmRepairStrategy {
    pub fn as_key(self) -> &'static str {
        match self {
            Self::SwitchSource => "switch_source",
            Self::SwitchQuery => "switch_query",
            Self::BrowserFallback => "browser_fallback",
            Self::DegradedSummary => "degraded_summary",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::SwitchSource => "Switch to a different source route immediately. Keep query intent stable and avoid repeating the same source.",
            Self::SwitchQuery => "Keep route family but rewrite query intent and locale phrasing to retrieve missing claim evidence.",
            Self::BrowserFallback => "Use browser-capable retrieval when ordinary search surfaces thin or blocked snippets.",
            Self::DegradedSummary => "No more retrieval. Synthesize the best supported answer, explicitly mark assumptions and unresolved gaps.",
        }
    }
}
