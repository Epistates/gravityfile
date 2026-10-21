//! Facets in a search query, alongside the text being searched for.
//!
//! `cache size>100mb age>30d type:f ext:log,tmp` finds files named like
//! `cache`, larger than 100 MB, unmodified for 30 days, ending in `.log` or
//! `.tmp`. A token that is not a well-formed facet is search text, so a
//! query never fails because of one.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gravityfile_core::units::{parse_age, parse_size};

/// What an index entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// One searchable entry.
#[derive(Debug, Clone)]
pub struct SearchEntry {
    pub path: PathBuf,
    /// The path as shown and matched, relative to where the search started.
    pub display: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: SystemTime,
}

impl SearchEntry {
    /// An entry with no size or age, for tests that only need names.
    #[cfg(test)]
    pub fn named(path: PathBuf, display: String, is_dir: bool) -> Self {
        Self {
            path,
            display,
            kind: if is_dir {
                EntryKind::Directory
            } else {
                EntryKind::File
            },
            size: 0,
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }
}

/// The facets of a query.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    min_size: Option<u64>,
    max_size: Option<u64>,
    older_than: Option<Duration>,
    newer_than: Option<Duration>,
    kinds: Vec<EntryKind>,
    extensions: Vec<String>,
}

impl Filter {
    /// Split a query into its facets and the remaining search text.
    pub fn parse(query: &str) -> (Self, String) {
        let mut filter = Self::default();
        let text: Vec<&str> = query
            .split_whitespace()
            .filter(|token| !filter.apply(token))
            .collect();
        (filter, text.join(" "))
    }

    /// Whether the query had any facets.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Apply `token` if it is a facet; false leaves it as search text.
    fn apply(&mut self, token: &str) -> bool {
        if let Some((key, values)) = token.split_once(':') {
            let values: Vec<String> = values
                .split(',')
                .filter(|value| !value.is_empty())
                .map(str::to_lowercase)
                .collect();
            match key {
                "type" | "t" => {
                    let kinds: Option<Vec<EntryKind>> = values
                        .iter()
                        .map(|value| match value.as_str() {
                            "f" | "file" => Some(EntryKind::File),
                            "d" | "dir" | "directory" => Some(EntryKind::Directory),
                            "l" | "link" | "symlink" => Some(EntryKind::Symlink),
                            _ => None,
                        })
                        .collect();
                    match kinds {
                        Some(kinds) if !kinds.is_empty() => self.kinds.extend(kinds),
                        _ => return false,
                    }
                }
                "ext" | "e" if !values.is_empty() => self.extensions.extend(
                    values
                        .into_iter()
                        .map(|value| value.trim_start_matches('.').to_string()),
                ),
                _ => return false,
            }
            return true;
        }
        for key in ["size", "age"] {
            let Some(rest) = token.strip_prefix(key) else {
                continue;
            };
            // `>` and `<` exclude the bound itself; `>=` and `<=` include it.
            let (greater, inclusive, value) = if let Some(value) = rest.strip_prefix(">=") {
                (true, true, value)
            } else if let Some(value) = rest.strip_prefix('>') {
                (true, false, value)
            } else if let Some(value) = rest.strip_prefix("<=") {
                (false, true, value)
            } else if let Some(value) = rest.strip_prefix('<') {
                (false, false, value)
            } else {
                return false;
            };
            if key == "size" {
                let Ok(bytes) = parse_size(value) else {
                    return false;
                };
                if greater {
                    self.min_size = Some(if inclusive {
                        bytes
                    } else {
                        bytes.saturating_add(1)
                    });
                } else if inclusive {
                    self.max_size = Some(bytes);
                } else if let Some(below) = bytes.checked_sub(1) {
                    self.max_size = Some(below);
                } else {
                    // Nothing is smaller than zero bytes.
                    self.min_size = Some(1);
                    self.max_size = Some(0);
                }
            } else {
                let Ok(age) = parse_age(value) else {
                    return false;
                };
                if greater {
                    self.older_than = Some(age);
                } else {
                    self.newer_than = Some(age);
                }
            }
            return true;
        }
        false
    }

    /// Whether `entry` passes every facet, ages measured from `now`.
    pub fn matches(&self, entry: &SearchEntry, now: SystemTime) -> bool {
        if self.min_size.is_some_and(|min| entry.size < min)
            || self.max_size.is_some_and(|max| entry.size > max)
        {
            return false;
        }
        if self.older_than.is_some() || self.newer_than.is_some() {
            // A modification time in the future counts as brand new.
            let age = now.duration_since(entry.modified).unwrap_or_default();
            if self.older_than.is_some_and(|older| age < older)
                || self.newer_than.is_some_and(|newer| age > newer)
            {
                return false;
            }
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&entry.kind) {
            return false;
        }
        if !self.extensions.is_empty() {
            let extension = Path::new(&entry.display)
                .extension()
                .map(|extension| extension.to_string_lossy().to_lowercase());
            if entry.is_dir()
                || !extension.is_some_and(|extension| self.extensions.contains(&extension))
            {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);

    fn entry(display: &str, kind: EntryKind, size: u64, days_old: u64) -> SearchEntry {
        SearchEntry {
            path: PathBuf::from(display),
            display: display.into(),
            kind,
            size,
            modified: SystemTime::UNIX_EPOCH + DAY * 1000 - DAY * days_old as u32,
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + DAY * 1000
    }

    #[test]
    fn facets_are_removed_and_the_rest_is_text() {
        let (filter, text) = Filter::parse("cache size>100mb  age>30d type:f ext:.Log,tmp build");
        assert_eq!(text, "cache build");
        assert_eq!(filter.min_size, Some(100_000_001), "strictly larger");
        assert_eq!(filter.older_than, Some(DAY * 30));
        assert_eq!(filter.kinds, [EntryKind::File]);
        assert_eq!(filter.extensions, ["log", "tmp"]);
    }

    #[test]
    fn malformed_facets_are_search_text() {
        for query in [
            "size>huge",
            "age>3m",
            "type:sock",
            "ext:",
            "colour:red",
            "size=5",
        ] {
            let (filter, text) = Filter::parse(query);
            assert!(filter.is_empty(), "{query}");
            assert_eq!(text, query);
        }
    }

    #[test]
    fn entries_must_pass_every_facet() {
        let (filter, _) = Filter::parse("size>=1kb size<1mb age<10d type:f ext:log");
        assert!(filter.matches(&entry("a/b.log", EntryKind::File, 5_000, 3), now()));
        assert!(filter.matches(&entry("a/b.LOG", EntryKind::File, 1_000, 3), now()));
        for rejected in [
            entry("a/b.log", EntryKind::File, 999, 3),
            entry("a/b.log", EntryKind::File, 1_000_000, 3),
            entry("a/b.log", EntryKind::File, 5_000, 11),
            entry("a/b.txt", EntryKind::File, 5_000, 3),
            entry("a/b.log", EntryKind::Directory, 5_000, 3),
            entry("a/b.log", EntryKind::Symlink, 5_000, 3),
        ] {
            assert!(!filter.matches(&rejected, now()), "{rejected:?}");
        }
    }

    #[test]
    fn a_future_modification_time_is_brand_new() {
        let (filter, _) = Filter::parse("age<1d");
        let mut future = entry("x", EntryKind::File, 1, 0);
        future.modified = now() + DAY;
        assert!(filter.matches(&future, now()));
    }
}
