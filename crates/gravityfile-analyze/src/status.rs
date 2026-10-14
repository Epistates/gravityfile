//! Explicit completeness and bounded diagnostics for analysis reports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gravityfile_core::FileTree;
use serde::{Deserialize, Serialize};

/// Whether the requested analysis was fully evaluated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisState {
    /// Older serialized reports did not record completeness.
    #[default]
    Unknown,
    /// All candidates in the configured scope were evaluated without errors.
    Complete,
    /// Scan warnings or analysis failures may hide results.
    Partial,
    /// The caller cancelled the analysis.
    Cancelled,
}

/// A file that could not be evaluated reliably.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisIssue {
    /// Affected file path.
    #[serde(with = "gravityfile_core::path_serde")]
    pub path: PathBuf,
    /// Explanation of the failure, bounded to 8 KiB by the analysis collector.
    pub message: String,
}

/// Completeness metadata. Result display limits do not imply partial analysis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalysisStatus {
    /// Overall state, distinct from whether matches were found.
    pub state: AnalysisState,
    /// Warnings inherited from the source scan.
    pub scan_warning_count: u64,
    /// Number of candidate failures, including omitted details.
    pub issue_count: u64,
    /// At most 100 failures, in path order.
    pub issues: Vec<AnalysisIssue>,
    /// Failures whose details were omitted to bound report memory.
    pub issues_omitted: u64,
}

impl AnalysisStatus {
    /// True only for a report explicitly known to be complete.
    pub fn is_complete(&self) -> bool {
        self.state == AnalysisState::Complete
    }

    pub(crate) fn from_scan(tree: &FileTree, cancelled: bool) -> Self {
        Self {
            state: if cancelled {
                AnalysisState::Cancelled
            } else if tree.has_warnings() {
                AnalysisState::Partial
            } else {
                AnalysisState::Complete
            },
            scan_warning_count: tree.warning_count(),
            ..Self::default()
        }
    }
}

impl std::fmt::Display for AnalysisState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unknown => "unknown",
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Cancelled => "cancelled",
        })
    }
}

#[derive(Default)]
pub(crate) struct IssueCollector(Mutex<(u64, BTreeMap<PathBuf, String>)>);

impl IssueCollector {
    pub(crate) fn record(&self, path: &Path, error: &std::io::Error) {
        let mut inner = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        inner.0 = inner.0.saturating_add(1);
        if inner.1.len() == 100
            && inner
                .1
                .last_key_value()
                .is_some_and(|(last, _)| path > last.as_path())
        {
            return;
        }
        let message = bounded_error_message(error);
        inner
            .1
            .entry(path.to_path_buf())
            .and_modify(|previous| {
                // Multiple workers can observe different failures for one path.
                // Keep a stable representative without retaining every message.
                if message < *previous {
                    previous.clone_from(&message);
                }
            })
            .or_insert(message);
        // Keep the lexically first paths regardless of worker completion order.
        if inner.1.len() > 100 {
            inner.1.pop_last();
        }
    }

    pub(crate) fn finish(self, tree: &FileTree, cancelled: bool) -> AnalysisStatus {
        let (count, issues) = self
            .0
            .into_inner()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut status = AnalysisStatus::from_scan(tree, cancelled);
        if count > 0 && !cancelled {
            status.state = AnalysisState::Partial;
        }
        status.issue_count = count;
        status.issues_omitted = count.saturating_sub(issues.len() as u64);
        status.issues = issues
            .into_iter()
            .map(|(path, message)| AnalysisIssue { path, message })
            .collect();
        status
    }
}

const MAX_ISSUE_MESSAGE_BYTES: usize = 8 * 1024;

fn bounded_error_message(error: &std::io::Error) -> String {
    use std::fmt::Write;
    const MARKER: &str = "... [truncated]";
    struct LimitedMessage {
        text: String,
        truncated: bool,
    }
    impl Write for LimitedMessage {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            let remaining =
                (MAX_ISSUE_MESSAGE_BYTES - MARKER.len()).saturating_sub(self.text.len());
            if value.len() <= remaining {
                self.text.push_str(value);
                return Ok(());
            }
            let mut end = remaining;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            self.text.push_str(&value[..end]);
            self.truncated = true;
            Err(std::fmt::Error)
        }
    }
    let mut message = LimitedMessage {
        text: String::with_capacity(MAX_ISSUE_MESSAGE_BYTES),
        truncated: false,
    };
    let _ = write!(&mut message, "{error}");
    if message.truncated {
        message.text.push_str(MARKER);
    }
    message.text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_messages_are_utf8_safe_and_bounded() {
        let error = std::io::Error::other("雪".repeat(100_000));
        let message = bounded_error_message(&error);
        assert!(message.len() <= MAX_ISSUE_MESSAGE_BYTES);
        assert!(message.ends_with("... [truncated]"));
        assert!(message.starts_with("雪"));
        assert_eq!(
            bounded_error_message(&std::io::Error::other("ordinary")),
            "ordinary"
        );
    }

    #[test]
    fn issue_retention_is_independent_of_message_and_path_arrival_order() {
        let forward = IssueCollector::default();
        let reverse = IssueCollector::default();
        for index in 0..150 {
            let path = PathBuf::from(format!("{index:03}"));
            forward.record(&path, &std::io::Error::other("z failure"));
            forward.record(&path, &std::io::Error::other("a failure"));
        }
        for index in (0..150).rev() {
            let path = PathBuf::from(format!("{index:03}"));
            reverse.record(&path, &std::io::Error::other("a failure"));
            reverse.record(&path, &std::io::Error::other("z failure"));
        }
        let first = forward.0.into_inner().unwrap();
        let second = reverse.0.into_inner().unwrap();
        assert_eq!(first, second);
        assert_eq!(first.0, 300);
        assert_eq!(first.1.len(), 100);
        assert_eq!(first.1.last_key_value().unwrap().0, Path::new("099"));
        assert!(first.1.values().all(|message| message == "a failure"));
    }
}
