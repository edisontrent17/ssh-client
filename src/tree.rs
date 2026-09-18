//! Cached search and flattened rows for the remote tree. This module performs no I/O.
use crate::files::Entry;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Debug)]
pub struct Directory {
    pub entries: Vec<Entry>,
    folded_names: Vec<String>,
}

#[derive(Default)]
pub struct DirectoryTree {
    directories: BTreeMap<String, Arc<Directory>>,
    errors: BTreeMap<String, Arc<String>>,
    revision: u64,
}

impl DirectoryTree {
    pub fn insert(&mut self, path: String, entries: Vec<Entry>) {
        self.errors.remove(&path);
        let folded_names = entries.iter().map(|e| e.name.to_lowercase()).collect();
        self.directories.insert(
            path,
            Arc::new(Directory {
                entries,
                folded_names,
            }),
        );
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn get(&self, path: &str) -> Option<&Vec<Entry>> {
        self.directories.get(path).map(|d| &d.entries)
    }
    pub fn contains_key(&self, path: &str) -> bool {
        self.directories.contains_key(path)
    }
    pub fn clear(&mut self) {
        self.directories.clear();
        self.errors.clear();
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn fail(&mut self, path: String, error: String) {
        self.errors.insert(path, Arc::new(error));
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn retry(&mut self, path: &str) {
        if self.errors.remove(path).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&str, &Vec<Entry>) -> bool) {
        let len = self.directories.len();
        let errors = self.errors.len();
        let empty = Vec::new();
        self.errors.retain(|path, _| {
            keep(
                path,
                self.directories
                    .get(path)
                    .map(|directory| &directory.entries)
                    .unwrap_or(&empty),
            )
        });
        self.directories
            .retain(|path, directory| keep(path, &directory.entries));
        if len != self.directories.len() || errors != self.errors.len() {
            self.revision = self.revision.wrapping_add(1);
        }
    }
    #[cfg(test)]
    pub fn remove(&mut self, path: &str) {
        if self.directories.remove(path).is_some() {
            self.revision = self.revision.wrapping_add(1);
        }
    }
}

impl std::ops::Index<&String> for DirectoryTree {
    type Output = Vec<Entry>;
    fn index(&self, path: &String) -> &Self::Output {
        self.get(path).expect("loaded directory")
    }
}

pub enum TreeRow {
    Error {
        depth: usize,
        path: String,
        error: Arc<String>,
    },
    Entry {
        directory: Arc<Directory>,
        index: usize,
        depth: usize,
        expanded: bool,
    },
    Message {
        depth: usize,
        text: &'static str,
    },
}

#[derive(Default)]
pub struct TreeCache {
    key: Option<(u64, String, String)>,
    matches: Option<Arc<BTreeSet<String>>>,
    auto_expanded: BTreeSet<String>,
    rows: Arc<Vec<TreeRow>>,
    rows_dirty: bool,
    reset_scroll: bool,
    #[cfg(test)]
    pub search_builds: usize,
    #[cfg(test)]
    pub row_builds: usize,
}

impl TreeCache {
    pub fn invalidate_rows(&mut self) {
        self.rows_dirty = true;
    }
    pub fn take_scroll_reset(&mut self) -> bool {
        std::mem::take(&mut self.reset_scroll)
    }

    pub fn matches(
        &mut self,
        tree: &DirectoryTree,
        root: &str,
        query: &str,
    ) -> Option<Arc<BTreeSet<String>>> {
        if self
            .key
            .as_ref()
            .is_some_and(|(revision, old_root, old_query)| {
                *revision == tree.revision && old_root == root && old_query == query
            })
        {
            return self.matches.clone();
        }
        self.reset_scroll |= self
            .key
            .as_ref()
            .is_none_or(|(_, old_root, old_query)| old_root != root || old_query != query);
        self.key = Some((tree.revision, root.into(), query.into()));
        self.rows_dirty = true;
        self.auto_expanded.clear();
        self.matches = if query.is_empty() {
            None
        } else {
            #[cfg(test)]
            {
                self.search_builds += 1;
            }
            let mut matches = BTreeSet::new();
            collect_matches(tree, root, query, 0, &mut matches, &mut self.auto_expanded);
            Some(Arc::new(matches))
        };
        self.matches.clone()
    }

    pub fn rows(
        &mut self,
        tree: &DirectoryTree,
        root: &str,
        query: &str,
        expanded: &BTreeSet<String>,
        overrides: &BTreeMap<String, bool>,
    ) -> Arc<Vec<TreeRow>> {
        self.matches(tree, root, query);
        if self.rows_dirty {
            let mut rows = vec![];
            if self.matches.as_ref().is_some_and(|m| m.is_empty()) {
                rows.push(TreeRow::Message {
                    depth: 0,
                    text: "No matching files or folders.",
                });
            } else {
                RowBuilder {
                    tree,
                    matches: self.matches.as_deref(),
                    auto_expanded: &self.auto_expanded,
                    expanded,
                    overrides,
                    rows: &mut rows,
                }
                .collect(root, 0, false);
            }
            self.rows = Arc::new(rows);
            self.rows_dirty = false;
            #[cfg(test)]
            {
                self.row_builds += 1;
            }
        }
        self.rows.clone()
    }
}

fn collect_matches(
    tree: &DirectoryTree,
    path: &str,
    query: &str,
    depth: usize,
    matches: &mut BTreeSet<String>,
    auto_expanded: &mut BTreeSet<String>,
) -> bool {
    if depth > 64 {
        return false;
    }
    let mut found = false;
    if let Some(directory) = tree.directories.get(path) {
        for (entry, folded) in directory.entries.iter().zip(&directory.folded_names) {
            let child_matches = entry.directory
                && collect_matches(tree, &entry.path, query, depth + 1, matches, auto_expanded);
            if child_matches {
                auto_expanded.insert(entry.path.clone());
            }
            if child_matches || folded.contains(query) {
                matches.insert(entry.path.clone());
                found = true;
            }
        }
    }
    found
}

struct RowBuilder<'a> {
    tree: &'a DirectoryTree,
    matches: Option<&'a BTreeSet<String>>,
    auto_expanded: &'a BTreeSet<String>,
    expanded: &'a BTreeSet<String>,
    overrides: &'a BTreeMap<String, bool>,
    rows: &'a mut Vec<TreeRow>,
}
impl RowBuilder<'_> {
    fn collect(&mut self, path: &str, depth: usize, show_all: bool) {
        if depth > 64 {
            return;
        }
        if let Some(error) = self.tree.errors.get(path) {
            self.rows.push(TreeRow::Error {
                depth,
                path: path.into(),
                error: error.clone(),
            });
            // Keep previously loaded entries visible if a refresh failed.
            if !self.tree.directories.contains_key(path) {
                return;
            }
        }
        let Some(directory) = self.tree.directories.get(path) else {
            self.rows.push(TreeRow::Message {
                depth,
                text: "Loading…",
            });
            return;
        };
        if directory.entries.is_empty() {
            self.rows.push(TreeRow::Message {
                depth,
                text: "Empty folder",
            });
        }
        for (index, entry) in directory.entries.iter().enumerate() {
            if !show_all && self.matches.is_some_and(|m| !m.contains(&entry.path)) {
                continue;
            }
            let expanded = entry.directory
                && if self.matches.is_some() {
                    self.overrides.get(&entry.path).copied().unwrap_or_else(|| {
                        (show_all && self.expanded.contains(&entry.path))
                            || self.auto_expanded.contains(&entry.path)
                    })
                } else {
                    self.expanded.contains(&entry.path)
                };
            self.rows.push(TreeRow::Entry {
                directory: directory.clone(),
                index,
                depth,
                expanded,
            });
            if expanded {
                let reveal_children = self.matches.is_some()
                    && (show_all || self.overrides.get(&entry.path) == Some(&true));
                self.collect(&entry.path, depth + 1, reveal_children);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_search_and_rows_are_reused_and_invalidated_by_listing_changes() {
        let mut tree = DirectoryTree::default();
        let entry = |name: &str| Entry {
            name: name.into(),
            path: format!("/root/{name}"),
            directory: false,
            file: true,
            size: 0,
        };
        tree.insert("/root".into(), vec![entry("Café.txt"), entry("other.txt")]);
        let mut cache = TreeCache::default();
        let first = cache.rows(&tree, "/root", "café", &BTreeSet::new(), &BTreeMap::new());
        for _ in 0..100 {
            let next = cache.rows(&tree, "/root", "café", &BTreeSet::new(), &BTreeMap::new());
            assert!(Arc::ptr_eq(&first, &next));
        }
        assert_eq!((cache.search_builds, cache.row_builds), (1, 1));
        tree.insert("/root".into(), vec![entry("café-new.txt")]);
        let next = cache.rows(&tree, "/root", "café", &BTreeSet::new(), &BTreeMap::new());
        assert!(!Arc::ptr_eq(&first, &next));
        let TreeRow::Entry {
            directory, index, ..
        } = &next[0]
        else {
            panic!("Expected refreshed result")
        };
        assert_eq!(directory.entries[*index].name, "café-new.txt");
        assert_eq!((cache.search_builds, cache.row_builds), (2, 2));
        cache.invalidate_rows();
        cache.rows(&tree, "/root", "café", &BTreeSet::new(), &BTreeMap::new());
        assert_eq!((cache.search_builds, cache.row_builds), (2, 3));
        tree.clear();
        let empty = cache.rows(&tree, "/root", "café", &BTreeSet::new(), &BTreeMap::new());
        assert!(matches!(
            empty[0],
            TreeRow::Message {
                text: "No matching files or folders.",
                ..
            }
        ));
    }
}
