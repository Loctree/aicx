use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::legacy_archive;
use crate::sanitize;

#[derive(Debug, Clone)]
pub struct CorpusEntry {
    pub label: String,
    pub path: PathBuf,
    pub haystack: String,
}

#[derive(Debug, Clone)]
pub struct CorpusItem {
    pub project: String,
    pub agent: String,
    pub date: String,
    pub kind: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusColumn {
    Orgs,
    Repos,
    Chunks,
}

#[derive(Debug)]
pub struct CorpusScreen {
    pub all_files: Vec<CorpusItem>,
    pub entries: Vec<CorpusEntry>,
    pub selected: usize,
    pub column: CorpusColumn,
    pub search: String,
    pub status: String,
}

impl CorpusScreen {
    pub fn load() -> Self {
        match load_corpus_items() {
            Ok(files) => {
                let entries = files.iter().map(entry_from_file).collect::<Vec<_>>();
                let mut screen = Self {
                    all_files: files,
                    entries,
                    selected: 0,
                    column: CorpusColumn::Chunks,
                    search: String::new(),
                    status: String::new(),
                };
                screen.status = screen.status_line();
                screen
            }
            Err(error) => Self {
                all_files: Vec::new(),
                entries: Vec::new(),
                selected: 0,
                column: CorpusColumn::Chunks,
                search: String::new(),
                status: format!("failed to scan corpus: {error}"),
            },
        }
    }

    pub fn stats_line(&self) -> String {
        let mut orgs = BTreeSet::new();
        let mut repos = BTreeSet::new();
        let mut latest = None::<String>;
        for file in &self.all_files {
            let (organization, _) = file.project.split_once('/').unwrap_or(("_", &file.project));
            orgs.insert(organization.to_string());
            repos.insert(file.project.clone());
            latest = Some(
                latest
                    .map(|current| current.max(file.date.clone()))
                    .unwrap_or_else(|| file.date.clone()),
            );
        }

        format!(
            "{} sessions - {} orgs - {} repos - latest {}",
            self.all_files.len(),
            orgs.len(),
            repos.len(),
            latest.unwrap_or_else(|| "never".to_string())
        )
    }

    pub fn status_line(&self) -> String {
        if self.entries.is_empty() {
            return self.status.clone();
        }
        format!(
            "{} of {} visible sessions{}",
            self.selected.saturating_add(1),
            self.entries.len(),
            if self.search.is_empty() {
                String::new()
            } else {
                format!(" matching '{}'", self.search)
            }
        )
    }

    pub fn orgs(&self) -> Vec<String> {
        let mut values = BTreeSet::new();
        for file in &self.all_files {
            values.insert(
                file.project
                    .split_once('/')
                    .map(|(organization, _)| organization.to_string())
                    .unwrap_or_else(|| "_".to_string()),
            );
        }
        values.into_iter().collect()
    }

    pub fn repos(&self) -> Vec<String> {
        let mut values = BTreeSet::new();
        for file in &self.all_files {
            values.insert(file.project.clone());
        }
        values.into_iter().collect()
    }

    pub fn selected_preview(&self) -> String {
        let Some(entry) = self.entries.get(self.selected) else {
            return "No chunk selected.".to_string();
        };
        match sanitize::read_to_string_validated(&entry.path) {
            Ok(raw) => raw.lines().take(50).collect::<Vec<_>>().join("\n"),
            Err(error) => format!("Failed to read {}: {error}", entry.path.display()),
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.selected = super::move_index(self.selected, self.entries.len(), delta);
    }

    pub fn move_column(&mut self, delta: isize) {
        self.column = match (self.column, delta.signum()) {
            (CorpusColumn::Orgs, 1) => CorpusColumn::Repos,
            (CorpusColumn::Repos, 1) => CorpusColumn::Chunks,
            (CorpusColumn::Chunks, -1) => CorpusColumn::Repos,
            (CorpusColumn::Repos, -1) => CorpusColumn::Orgs,
            (column, _) => column,
        };
    }

    pub fn apply_search(&mut self, query: String) {
        self.search = query.trim().to_string();
        if self.search.is_empty() {
            self.entries = self.all_files.iter().map(entry_from_file).collect();
        } else {
            let needle = self.search.to_ascii_lowercase();
            self.entries = self
                .all_files
                .iter()
                .map(entry_from_file)
                .filter(|entry| entry.haystack.contains(&needle))
                .collect();
        }
        self.selected = 0;
        self.status = self.status_line();
    }
}

fn entry_from_file(file: &CorpusItem) -> CorpusEntry {
    let label = format!(
        "{} / {} / {} / {}",
        file.project, file.date, file.kind, file.agent
    );
    CorpusEntry {
        label: label.clone(),
        path: file.path.clone(),
        haystack: format!("{} {}", label, file.path.display()).to_ascii_lowercase(),
    }
}

fn load_corpus_items() -> anyhow::Result<Vec<CorpusItem>> {
    let home = crate::aicx_home::resolve()?;
    if crate::catalog::sessions_path_for(&home).is_file() {
        return Ok(crate::catalog::read_entries_at(&home)?
            .into_iter()
            .filter_map(|entry| {
                Some(CorpusItem {
                    project: entry.project?,
                    agent: entry.agent,
                    date: entry.date.unwrap_or_default(),
                    kind: "session".to_string(),
                    path: PathBuf::from(entry.source_path),
                })
            })
            .collect());
    }
    Ok(legacy_archive::scan_context_files()?
        .into_iter()
        .map(|file| CorpusItem {
            project: file.project,
            agent: file.agent,
            date: file.date_iso,
            kind: file.kind.dir_name().to_string(),
            path: file.path,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_dir(name: &str) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("aicx-corpus-{name}-{id}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn selected_preview_rejects_oversized_chunk_file() {
        let dir = unique_test_dir("oversized");
        let path = dir.join("chunk.md");
        File::create(&path)
            .unwrap()
            .set_len((sanitize::MAX_VALIDATED_BYTES + 1) as u64)
            .unwrap();
        let screen = CorpusScreen {
            all_files: Vec::new(),
            entries: vec![CorpusEntry {
                label: "oversized".to_string(),
                path: path.clone(),
                haystack: String::new(),
            }],
            selected: 0,
            column: CorpusColumn::Chunks,
            search: String::new(),
            status: String::new(),
        };

        let preview = screen.selected_preview();

        assert!(preview.contains("Failed to read"));
        assert!(preview.contains("exceeds validated read cap"));
        let _ = fs::remove_dir_all(&dir);
    }
}
