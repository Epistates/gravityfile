//! A single-view cache keyed by immutable report identity and directory scope.
use gravityfile_analyze::DuplicateReport;
use std::cell::{Ref, RefCell};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use tokio_util::sync::CancellationToken;

/// Prepare root counts on the analysis worker, with bounded cancellation latency.
pub(super) fn prepare_counts(
    report: &DuplicateReport,
    root: &Path,
    token: &CancellationToken,
) -> Option<Vec<(usize, bool)>> {
    let mut counts = Vec::with_capacity(report.groups.len());
    for group in &report.groups {
        let mut count = 0;
        let mut sorted = true;
        let mut previous = None;
        for chunk in group.paths.chunks(1024) {
            if token.is_cancelled() {
                return None;
            }
            for path in chunk {
                count += usize::from(path.starts_with(root));
                sorted &= previous.is_none_or(|previous| previous <= path);
                previous = Some(path);
            }
        }
        counts.push((count, sorted));
    }
    (!token.is_cancelled()).then_some(counts)
}

#[derive(Default)]
pub(super) struct DuplicateFilter(RefCell<Option<Filtered>>);

pub(super) struct Filtered {
    report: Weak<DuplicateReport>,
    root: PathBuf,
    scan_root: PathBuf,
    /// Original report index and number of paths in the displayed directory.
    pub groups: Vec<(usize, usize)>,
    pub wasted: u64,
    /// Validated once for this immutable report, reused across directory scopes.
    sorted: Vec<bool>,
}

impl DuplicateFilter {
    /// Install worker-prepared counts without traversing report paths on the UI.
    pub fn seed(&self, report: &Arc<DuplicateReport>, root: PathBuf, counts: Vec<(usize, bool)>) {
        if counts.len() != report.groups.len() {
            return;
        }
        let (counts, sorted): (Vec<_>, Vec<_>) = counts.into_iter().unzip();
        *self.0.borrow_mut() = Some(Filtered {
            report: Arc::downgrade(report),
            scan_root: root.clone(),
            root,
            groups: counts.into_iter().enumerate().collect(),
            sorted,
            wasted: report
                .groups
                .iter()
                .fold(0u64, |sum, group| sum.saturating_add(group.wasted_bytes)),
        });
    }

    pub fn get(
        &self,
        report: &Arc<DuplicateReport>,
        root: &Path,
        scan_root: &Path,
    ) -> Ref<'_, Filtered> {
        let identity = Arc::downgrade(report);
        let valid = self.0.borrow().as_ref().is_some_and(|cached| {
            cached.report.ptr_eq(&identity) && cached.root == root && cached.scan_root == scan_root
        });
        if !valid {
            let sorted = self
                .0
                .borrow()
                .as_ref()
                .filter(|cached| cached.report.ptr_eq(&identity))
                .map(|cached| cached.sorted.clone())
                .unwrap_or_else(|| {
                    report
                        .groups
                        .iter()
                        .map(|group| group.paths.is_sorted())
                        .collect()
                });
            let mut groups = Vec::new();
            let mut wasted = 0u64;
            for (index, group) in report.groups.iter().enumerate() {
                let count = if sorted[index] {
                    sorted_scope_count(&group.paths, root)
                } else {
                    group
                        .paths
                        .iter()
                        .filter(|path| path.starts_with(root))
                        .count()
                };
                if root == scan_root || count > 0 {
                    groups.push((index, count));
                    let reclaimable = if root == scan_root {
                        group.wasted_bytes
                    } else {
                        group.size.saturating_mul(count.saturating_sub(1) as u64)
                    };
                    wasted = wasted.saturating_add(reclaimable);
                }
            }
            *self.0.borrow_mut() = Some(Filtered {
                report: identity,
                root: root.to_owned(),
                scan_root: scan_root.to_owned(),
                groups,
                wasted,
                sorted,
            });
        }
        Ref::map(self.0.borrow(), |cached| {
            cached.as_ref().expect("cache initialized")
        })
    }
}

/// Path ordering and starts_with both compare components, so descendants of a
/// scope form one contiguous range in a sorted group.
fn sorted_scope_count(paths: &[PathBuf], root: &Path) -> usize {
    let start = paths.partition_point(|path| path.as_path() < root);
    paths[start..].partition_point(|path| path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gravityfile_analyze::{AnalysisStatus, DuplicateGroup};
    use gravityfile_core::ContentHash;

    fn report() -> Arc<DuplicateReport> {
        Arc::new(DuplicateReport {
            status: AnalysisStatus::default(),
            groups: vec![DuplicateGroup {
                hash: ContentHash::new([0; 32]),
                size: 10,
                paths: vec![
                    "scan/a/one".into(),
                    "scan/a/two".into(),
                    "scan/b/three".into(),
                ],
                wasted_bytes: 20,
            }],
            total_duplicate_size: 30,
            total_wasted_space: 20,
            files_analyzed: 3,
            files_with_duplicates: 3,
            group_count: 1,
            groups_omitted: 0,
        })
    }

    #[test]
    fn sorted_scope_ranges_match_linear_component_filtering() {
        let mut paths: Vec<PathBuf> = [
            "",
            ".",
            "..",
            "../a",
            "scan",
            "scan/a",
            "scan/a/one",
            "scan/a/two",
            "scan/a-",
            "scan/ab/one",
            "scan/b/../a",
            "scan/b/one",
            "/scan/a",
            "/scan/a/two",
            "scan/é/one",
            "scan/界/two",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            paths.push(PathBuf::from(std::ffi::OsStr::from_bytes(b"scan/\xff/one")));
        }
        paths.sort();
        let roots: Vec<_> = paths.iter().flat_map(|path| path.ancestors()).collect();
        for root in roots
            .into_iter()
            .chain([Path::new("scan/missing"), Path::new("scan/a/")])
        {
            assert_eq!(
                sorted_scope_count(&paths, root),
                paths.iter().filter(|path| path.starts_with(root)).count(),
                "{root:?}"
            );
        }
    }

    #[test]
    fn unsorted_report_falls_back_without_incorrect_scope_counts() {
        let mut report = report();
        Arc::get_mut(&mut report).unwrap().groups[0]
            .paths
            .swap(0, 2);
        let root = Path::new("scan");
        let prepared = prepare_counts(&report, root, &CancellationToken::new()).unwrap();
        assert_eq!(prepared, [(3, false)]);
        let cache = DuplicateFilter::default();
        cache.seed(&report, root.into(), prepared);
        assert_eq!(
            cache.get(&report, Path::new("scan/a"), root).groups,
            [(0, 2)]
        );
        assert_eq!(
            cache.get(&report, Path::new("scan/b"), root).groups,
            [(0, 1)]
        );
    }

    #[test]
    fn preparation_checks_order_across_chunk_boundaries() {
        let mut report = report();
        let group = &mut Arc::get_mut(&mut report).unwrap().groups[0];
        group.paths = (0..2048)
            .map(|index| PathBuf::from(format!("scan/{index:04}")))
            .collect();
        assert_eq!(
            prepare_counts(&report, Path::new("scan"), &CancellationToken::new()).unwrap(),
            [(2048, true)]
        );
        Arc::get_mut(&mut report).unwrap().groups[0]
            .paths
            .swap(1023, 1024);
        assert_eq!(
            prepare_counts(&report, Path::new("scan"), &CancellationToken::new()).unwrap(),
            [(2048, false)]
        );
    }

    #[test]
    fn prepared_counts_match_lazy_cache_and_preserve_scope_invalidation() {
        let report = report();
        let root = Path::new("scan");
        let counts = prepare_counts(&report, root, &CancellationToken::new()).unwrap();
        assert_eq!(counts, [(3, true)]);
        let cache = DuplicateFilter::default();
        cache.seed(&report, root.into(), counts);
        {
            // Holding this borrow makes any unexpected cache rebuild fail.
            let seeded = cache.0.borrow();
            let ready = cache.get(&report, root, root);
            assert_eq!(ready.groups, seeded.as_ref().unwrap().groups);
            assert_eq!(ready.wasted, 20);
        }
        assert_eq!(
            cache.get(&report, Path::new("scan/a"), root).groups,
            [(0, 2)]
        );
        assert_eq!(cache.get(&report, Path::new("scan/a"), root).wasted, 10);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(prepare_counts(&report, root, &cancelled).is_none());
    }

    #[test]
    fn repeated_scope_reuses_cache_and_directory_changes_refresh_counts() {
        let report = report();
        let cache = DuplicateFilter::default();
        let root = Path::new("scan/a");
        let scan = Path::new("scan");
        {
            let first = cache.get(&report, root, scan);
            let repeated = cache.get(&report, root, scan);
            assert_eq!(first.groups.as_ptr(), repeated.groups.as_ptr());
            assert_eq!(first.groups, [(0, 2)]);
            assert_eq!(first.wasted, 10);
        }
        assert_eq!(
            cache.get(&report, Path::new("scan/b"), scan).groups,
            [(0, 1)]
        );
        assert_eq!(cache.get(&report, Path::new("scan/b"), scan).wasted, 0);
        assert!(
            cache
                .get(&report, Path::new("elsewhere"), scan)
                .groups
                .is_empty()
        );
        assert_eq!(cache.get(&report, scan, scan).wasted, 20);
    }

    #[test]
    fn replacement_report_refreshes_cache_without_retaining_old_report() {
        let report = report();
        let weak = Arc::downgrade(&report);
        let cache = DuplicateFilter::default();
        let root = Path::new("scan/a");
        let scan = Path::new("scan");
        assert_eq!(cache.get(&report, root, scan).wasted, 10);
        let mut replacement = report.as_ref().clone();
        replacement.groups[0].paths.pop();
        replacement.groups[0].size = 20;
        let replacement = Arc::new(replacement);
        drop(report);
        assert!(weak.upgrade().is_none());
        assert_eq!(cache.get(&replacement, root, scan).wasted, 20);
        // Changing the scan root changes whole-report accounting semantics too.
        assert_eq!(
            cache
                .get(&replacement, Path::new("scan/b"), Path::new("scan/b"))
                .groups,
            [(0, 0)]
        );
    }
}
