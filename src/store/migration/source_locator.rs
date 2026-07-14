use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceLocator {
    index: HashMap<String, Vec<PathBuf>>,
}

#[derive(Debug, Clone)]
pub(super) enum SourceLookupOutcome {
    Missing,
    Unique(PathBuf),
    Ambiguous(Vec<PathBuf>),
}

impl SourceLocator {
    pub(super) fn from_home() -> Self {
        let Some(home) = crate::os_user_home() else {
            return Self::default();
        };

        let mut locator = Self::default();
        locator.index_recursive(home.join(".claude").join("projects"), |path| {
            matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("jsonl" | "output")
            )
        });
        locator.index_recursive(home.join(".codex").join("sessions"), |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        });
        locator.index_file(home.join(".codex").join("history.jsonl"));

        // Grok (same rollout v1/responses jsonl format as codex, under .grok/sessions and .grok/projects)
        locator.index_recursive(home.join(".grok").join("sessions"), |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        });
        locator.index_recursive(home.join(".grok").join("projects"), |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        });
        locator.index_recursive(home.join(".gemini").join("tmp"), |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("session-"))
        });
        locator.index_recursive(
            home.join(".gemini")
                .join("antigravity")
                .join("conversations"),
            |path| path.extension().and_then(|ext| ext.to_str()) == Some("pb"),
        );
        locator.index_directories(home.join(".gemini").join("antigravity").join("brain"));
        locator
    }

    pub(super) fn lookup(&self, hint: &str) -> SourceLookupOutcome {
        let key = hint.to_ascii_lowercase();
        let Some(paths) = self.index.get(&key) else {
            return SourceLookupOutcome::Missing;
        };

        let unique: Vec<PathBuf> = paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        match unique.as_slice() {
            [] => SourceLookupOutcome::Missing,
            [only] => SourceLookupOutcome::Unique(only.clone()),
            many => SourceLookupOutcome::Ambiguous(many.to_vec()),
        }
    }

    fn index_recursive<F>(&mut self, root: PathBuf, include: F)
    where
        F: Fn(&Path) -> bool + Copy,
    {
        if !root.exists() {
            return;
        }

        let Ok(read_dir) = fs::read_dir(&root) else {
            return;
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.index_recursive(path, include);
                continue;
            }

            if include(&path) {
                self.add_path(&path);
            }
        }
    }

    fn index_directories(&mut self, root: PathBuf) {
        if !root.exists() {
            return;
        }

        let Ok(read_dir) = fs::read_dir(&root) else {
            return;
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.add_path(&path);
            }
        }
    }

    fn index_file(&mut self, path: PathBuf) {
        if path.exists() {
            self.add_path(&path);
        }
    }

    fn add_path(&mut self, path: &Path) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return;
        };

        let lower_name = name.to_ascii_lowercase();
        self.index
            .entry(lower_name.clone())
            .or_default()
            .push(path.to_path_buf());

        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            let lower_stem = stem.to_ascii_lowercase();
            if lower_stem != lower_name {
                self.index
                    .entry(lower_stem)
                    .or_default()
                    .push(path.to_path_buf());
            }
        }
    }
}
