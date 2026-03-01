use dashmap::DashSet;
use std::sync::Arc;

/// Thread-safe tracker of visited page states.
/// A state is identified by a fingerprint of (URL + sorted action path).
#[derive(Clone, Default)]
pub struct StateTracker {
    visited: Arc<DashSet<String>>,
}

impl StateTracker {
    pub fn new() -> Self {
        Self {
            visited: Arc::new(DashSet::new()),
        }
    }

    /// Returns `true` if this fingerprint is new (and marks it visited).
    /// Returns `false` if already seen.
    pub fn visit(&self, fingerprint: &str) -> bool {
        self.visited.insert(fingerprint.to_string())
    }

    /// Peek without marking visited (used for pre-queue dedup).
    #[allow(dead_code)]
    pub fn is_visited(&self, fingerprint: &str) -> bool {
        self.visited.contains(fingerprint)
    }

    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.visited.len()
    }
}
