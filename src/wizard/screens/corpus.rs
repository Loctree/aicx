use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use crate::store::{self, StoredContextFile};

#[derive(Debug, Clone)]
pub struct CorpusEntry {
    pub label: String,
    pub path: PathBuf,
    pub haystack: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusColumn {
    Orgs,
    Repos,
    Chunks,
}

#[derive(Debug)]
pub struct CorpusScreen {
    pub all_files: Vec<StoredContextFile>,
    pub entries: Vec<CorpusEntry>,
    pub selected: usize,
    pub column: CorpusColumn,
    pub search: String,
    pub status: String,
}

impl CorpusScreen {
    pub fn load() -> Self {
        match store::scan_context_files() {
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
            if let Some(repo) = &file.repo {
                orgs.insert(repo.organization.clone());
                repos.insert(repo.slug());
            } else {
                orgs.insert("non-repository-contexts".to_string());
                repos.insert(file.project.clone());
            }
            latest = Some(
                latest
                    .map(|current| current.max(file.date_iso.clone()))
                    .unwrap_or_else(|| file.date_iso.clone()),
            );
        }

        format!(
            "{} chunks - {} orgs - {} repos - last sync {}",
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
            "{} of {} visible chunks{}",
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
                file.repo
                    .as_ref()
                    .map(|repo| repo.organization.clone())
                    .unwrap_or_else(|| "non-repository-contexts".to_string()),
            );
        }
        values.into_iter().collect()
    }

    pub fn repos(&self) -> Vec<String> {
        let mut values = BTreeSet::new();
        for file in &self.all_files {
            values.insert(
                file.repo
                    .as_ref()
                    .map(|repo| repo.slug())
                    .unwrap_or_else(|| file.project.clone()),
            );
        }
        values.into_iter().collect()
    }

    pub fn selected_preview(&self) -> String {
        let Some(entry) = self.entries.get(self.selected) else {
            return "No chunk selected.".to_string();
        };
        match fs::read_to_string(&entry.path) {
            Ok(raw) => raw.lines().take(50).collect::<Vec<_>>().join("\n"),
            Err(error) => format!("Failed to read {}: {error}", entry.path.display()),
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.selected = move_index(self.selected, self.entries.len(), delta);
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

fn entry_from_file(file: &StoredContextFile) -> CorpusEntry {
    let repo = file
        .repo
        .as_ref()
        .map(|repo| repo.slug())
        .unwrap_or_else(|| file.project.clone());
    let label = format!(
        "{} / {} / {} / {} / chunk {}",
        repo, file.date_iso, file.kind, file.agent, file.chunk
    );
    CorpusEntry {
        label: label.clone(),
        path: file.path.clone(),
        haystack: format!("{} {}", label, file.path.display()).to_ascii_lowercase(),
    }
}

fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta < 0 {
        current.saturating_sub(delta.unsigned_abs()).min(len - 1)
    } else {
        current.saturating_add(delta as usize).min(len - 1)
    }
}
