//! Wizard Search view — wraps the same retrieval path as `aicx search`.
//!
//! Truthful about who answered (CURRENT lexical vs FS fallback) and surfaces
//! index/catalog drift with an inline repair path instead of silent empty.

use std::path::PathBuf;

use crate::rank::FuzzyResult;
use crate::sanitize;
use crate::search_engine::{
    self, SemanticError, SemanticSearchFilters, SemanticSearchOutcome, try_semantic_search_filtered,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBackendKind {
    CurrentIndex,
    FilesystemFallback,
    Failed,
    NotRun,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub score: u8,
    pub agent: String,
    pub project: String,
    pub date: String,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub path: String,
    pub snippet: String,
    pub label: String,
}

impl SearchHit {
    fn from_fuzzy(result: FuzzyResult) -> Self {
        let snippet = result
            .matched_lines
            .first()
            .cloned()
            .unwrap_or_else(|| result.label.clone());
        Self {
            score: result.score,
            agent: result.agent,
            project: result.project,
            date: result.date,
            session_id: result.session_id,
            cwd: result.cwd,
            path: result.path,
            snippet,
            label: result.label,
        }
    }

    pub fn list_label(&self) -> String {
        let session = self
            .session_id
            .as_deref()
            .map(|id| {
                if id.len() > 12 {
                    format!("{}…", &id[..12])
                } else {
                    id.to_string()
                }
            })
            .unwrap_or_else(|| "—".to_string());
        format!(
            "[{:>3}] {} | {} | {} | {}",
            self.score,
            self.agent,
            truncate(&self.project, 28),
            self.date,
            session
        )
    }
}

#[derive(Debug)]
pub struct SearchScreen {
    pub query: String,
    pub project: Option<String>,
    pub agent: Option<String>,
    pub hours: u64,
    pub hits: Vec<SearchHit>,
    pub selected: usize,
    pub preview: String,
    pub status: String,
    pub backend_kind: SearchBackendKind,
    pub backend_line: String,
    pub drift_banner: Option<String>,
    pub repair_hint: Option<String>,
    pub last_error: Option<String>,
    pub scanned: usize,
}

impl Default for SearchScreen {
    fn default() -> Self {
        Self {
            query: String::new(),
            project: None,
            agent: None,
            hours: 0,
            hits: Vec::new(),
            selected: 0,
            preview: String::new(),
            status: "type / then Enter to search CURRENT".to_string(),
            backend_kind: SearchBackendKind::NotRun,
            backend_line: "backend: not run yet".to_string(),
            drift_banner: None,
            repair_hint: None,
            last_error: None,
            scanned: 0,
        }
    }
}

impl SearchScreen {
    pub fn with_filters(
        query: Option<String>,
        project: Option<String>,
        agent: Option<String>,
    ) -> Self {
        let mut screen = Self {
            query: query.unwrap_or_default(),
            project,
            agent,
            ..Self::default()
        };
        screen.refresh_drift();
        if !screen.query.trim().is_empty() {
            screen.run_search();
        } else {
            screen.status = screen
                .drift_banner
                .clone()
                .unwrap_or_else(|| "search ready — / query, Enter run, p/a/t filters".to_string());
        }
        screen
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.selected = super::move_index(self.selected, self.hits.len(), delta);
        self.preview.clear();
    }

    pub fn apply_query(&mut self, query: String) {
        self.query = query.trim().to_string();
        self.run_search();
    }

    pub fn cycle_project_filter(&mut self) {
        self.project = match self.project.as_deref() {
            None => Some("Loctree/aicx".to_string()),
            Some("Loctree/aicx") => Some("VetCoders/vibecrafted".to_string()),
            Some("VetCoders/vibecrafted") => Some("/pensieve".to_string()),
            Some(_) => None,
        };
        if !self.query.trim().is_empty() {
            self.run_search();
        } else {
            self.status = format!("project filter: {}", display_opt(self.project.as_deref()));
        }
    }

    pub fn cycle_agent_filter(&mut self) {
        self.agent = match self.agent.as_deref() {
            None => Some("claude".to_string()),
            Some("claude") => Some("codex".to_string()),
            Some("codex") => Some("grok".to_string()),
            Some("grok") => Some("gemini".to_string()),
            Some(_) => None,
        };
        if !self.query.trim().is_empty() {
            self.run_search();
        } else {
            self.status = format!("agent filter: {}", display_opt(self.agent.as_deref()));
        }
    }

    pub fn cycle_hours(&mut self) {
        self.hours = match self.hours {
            0 => 48,
            48 => 168,
            168 => 720,
            _ => 0,
        };
        if !self.query.trim().is_empty() {
            self.run_search();
        } else {
            self.status = format!(
                "time window: {}",
                if self.hours == 0 {
                    "all time".to_string()
                } else {
                    format!("{}h", self.hours)
                }
            );
        }
    }

    pub fn open_selected(&mut self) {
        let Some(hit) = self.hits.get(self.selected) else {
            self.preview = "No hit selected.".to_string();
            return;
        };
        let path = PathBuf::from(&hit.path);
        self.preview = match sanitize::read_to_string_validated(&path) {
            Ok(raw) => {
                let head: Vec<&str> = raw.lines().take(100).collect();
                format!(
                    "{} | {} | {}\nsession: {}\ncwd: {}\npath: {}\n\n{}\n\n--- source (first lines) ---\n{}",
                    hit.agent,
                    hit.project,
                    hit.date,
                    hit.session_id.as_deref().unwrap_or("—"),
                    hit.cwd.as_deref().unwrap_or("—"),
                    hit.path,
                    hit.snippet,
                    head.join("\n")
                )
            }
            Err(error) => format!(
                "{} | {} | {}\n\n{}\n\npath: {}\nread failed: {error}",
                hit.agent, hit.project, hit.date, hit.snippet, hit.path
            ),
        };
    }

    pub fn selected_preview(&self) -> String {
        if !self.preview.is_empty() {
            return self.preview.clone();
        }
        if let Some(banner) = &self.drift_banner
            && self.hits.is_empty()
        {
            return format!(
                "{banner}\n\n{}\n\n{}",
                self.backend_line,
                self.repair_hint.as_deref().unwrap_or(
                    "Run bounded refresh first: aicx catalog refresh; rebuild is only the full-census repair."
                )
            );
        }
        self.hits
            .get(self.selected)
            .map(|hit| {
                format!(
                    "{} | {} | score {}\n{}\n\nsession: {}\ncwd: {}\npath: {}",
                    hit.agent,
                    hit.project,
                    hit.score,
                    hit.snippet,
                    hit.session_id.as_deref().unwrap_or("—"),
                    hit.cwd.as_deref().unwrap_or("—"),
                    hit.path
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "{}\n\n{}",
                    self.backend_line,
                    self.last_error
                        .clone()
                        .or_else(|| self.drift_banner.clone())
                        .unwrap_or_else(|| "No results. Press / to enter a query.".to_string())
                )
            })
    }

    pub fn refresh_drift(&mut self) {
        let home = match crate::aicx_home::resolve() {
            Ok(path) => path,
            Err(error) => {
                self.drift_banner = Some(format!("AICX home unresolved: {error}"));
                self.repair_hint = Some("Set AICX_HOME or create ~/.aicx".to_string());
                return;
            }
        };

        let host = hostname().unwrap_or_else(|| "unknown-host".to_string());
        let catalog_path = crate::catalog::sessions_path_for(&home);
        let catalog_present = catalog_path.is_file();

        match crate::api::index_status_at(&home, None) {
            Ok(status) => {
                use crate::api::IndexReadiness;
                let readiness = match status.readiness {
                    IndexReadiness::Missing => "missing",
                    IndexReadiness::Pending => "pending",
                    IndexReadiness::Ready => "ready",
                    IndexReadiness::StaleChunks => "stale_chunks",
                    IndexReadiness::StaleIndex => "stale_index",
                    IndexReadiness::PendingScanTimeout => "pending_scan_timeout",
                };
                let generation = status
                    .semantic_index_path
                    .as_deref()
                    .and_then(|p| {
                        std::path::Path::new(p)
                            .parent()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "none".to_string());
                let committed = status
                    .committed_at
                    .clone()
                    .unwrap_or_else(|| "never".into());

                if !status.semantic_index_present
                    || matches!(
                        status.readiness,
                        IndexReadiness::Missing | IndexReadiness::Pending
                    )
                {
                    self.drift_banner = Some(format!(
                        "DRIFT: lexical CURRENT missing on host={host} · catalog={} · readiness={readiness}",
                        if catalog_present {
                            "present"
                        } else {
                            "missing"
                        }
                    ));
                    self.repair_hint = Some(
                        "Repair: aicx catalog rebuild && aicx index   (wizard: screen 4 → s)"
                            .to_string(),
                    );
                } else if matches!(
                    status.readiness,
                    IndexReadiness::StaleIndex
                        | IndexReadiness::StaleChunks
                        | IndexReadiness::PendingScanTimeout
                ) {
                    self.drift_banner = Some(format!(
                        "NOTE: index readiness={readiness} host={host} gen={generation} committed={committed} pending={}",
                        status.pending_chunks
                    ));
                    self.repair_hint = Some(
                        "Lexical may still work. If search fails schema/stale: aicx index"
                            .to_string(),
                    );
                } else if !catalog_present {
                    self.drift_banner = Some(format!(
                        "DRIFT: catalog missing on host={host} (index present gen={generation})"
                    ));
                    self.repair_hint = Some("Repair: aicx catalog rebuild".to_string());
                } else {
                    self.drift_banner = None;
                    self.repair_hint = None;
                    self.backend_line = format!(
                        "index CURRENT ready · host={host} · gen={generation} · sync={committed}"
                    );
                }
            }
            Err(error) => {
                self.drift_banner = Some(format!("index status failed on host={host}: {error}"));
                self.repair_hint = Some("Repair: aicx catalog rebuild && aicx index".to_string());
            }
        }
    }

    pub fn run_search(&mut self) {
        self.refresh_drift();
        self.preview.clear();
        self.hits.clear();
        self.selected = 0;
        self.last_error = None;
        self.scanned = 0;

        let query = self.query.trim().to_string();
        if query.is_empty() {
            self.status = "empty query".to_string();
            self.backend_kind = SearchBackendKind::NotRun;
            self.backend_line = "backend: not run (empty query)".to_string();
            return;
        }

        let home = match crate::aicx_home::resolve() {
            Ok(path) => path,
            Err(error) => {
                self.backend_kind = SearchBackendKind::Failed;
                self.backend_line = "backend: failed".to_string();
                self.last_error = Some(error.to_string());
                self.status = format!("search failed: {error}");
                return;
            }
        };

        let host = hostname().unwrap_or_else(|| "unknown-host".to_string());
        let limit = 20usize;
        let hours_cutoff = if self.hours > 0 {
            Some(
                (chrono::Utc::now() - chrono::Duration::hours(self.hours as i64))
                    .format("%Y-%m-%d")
                    .to_string(),
            )
        } else {
            None
        };

        let post_filters = SemanticSearchFilters {
            agent: self.agent.clone(),
            score_min: None,
            date_lo: None,
            date_hi: None,
            hours_cutoff,
            legacy_dense: false,
            deep: false,
        };

        let project_owned = self.project.clone();
        let scopes: Vec<Option<&str>> = match project_owned.as_deref() {
            Some(p) if !p.is_empty() => vec![Some(p)],
            _ => vec![None],
        };

        match try_semantic_search_filtered(&home, &query, limit, &scopes, None, None, &post_filters)
        {
            Ok(filtered) => {
                let SemanticSearchOutcome {
                    results,
                    scanned,
                    backend_label,
                    model_id: _,
                    retrieval_status: _,
                } = filtered.outcome;
                self.scanned = scanned;
                self.hits = results.into_iter().map(SearchHit::from_fuzzy).collect();
                self.backend_kind = SearchBackendKind::CurrentIndex;
                let generation = current_generation_label(&home);
                let committed = current_committed_label(&home);
                self.backend_line = format!(
                    "index CURRENT ({backend_label}) · sync: {committed} · host: {host} · gen: {generation}"
                );
                self.status = format!(
                    "{} hit(s) · scanned {scanned} · p={} a={} t={}",
                    self.hits.len(),
                    display_opt(self.project.as_deref()),
                    display_opt(self.agent.as_deref()),
                    if self.hours == 0 {
                        "all".to_string()
                    } else {
                        format!("{}h", self.hours)
                    }
                );
            }
            Err(err) => {
                if err.allows_filesystem_fallback() {
                    self.run_filesystem_fallback(&home, &query, limit, &post_filters, &host, &err);
                } else {
                    self.backend_kind = SearchBackendKind::Failed;
                    self.backend_line = format!(
                        "index FAILED kind={} · host={host} (not fallback — honest error)",
                        err.kind()
                    );
                    self.last_error = Some(format!("{}: {}", err.kind(), err.reason()));
                    self.drift_banner =
                        Some(format!("DRIFT/FAIL: {} — {}", err.kind(), err.reason()));
                    self.repair_hint = Some(err.recommendation().to_string());
                    self.status = format!("search failed: {}", err.kind());
                }
            }
        }
    }

    fn run_filesystem_fallback(
        &mut self,
        home: &std::path::Path,
        query: &str,
        limit: usize,
        post_filters: &SemanticSearchFilters,
        host: &str,
        err: &SemanticError,
    ) {
        let fetch = search_engine::fuzzy_fetch_limit(limit, post_filters.is_active());
        match search_engine::fuzzy_search_with_post_filters(
            home,
            query,
            fetch,
            &[None],
            None,
            post_filters,
        ) {
            Ok((results, scanned)) => {
                let finalized =
                    search_engine::finalize_fuzzy_results(results, None, Some("score"), limit);
                self.scanned = scanned;
                self.hits = finalized.into_iter().map(SearchHit::from_fuzzy).collect();
                self.backend_kind = SearchBackendKind::FilesystemFallback;
                self.backend_line = format!(
                    "fallback FS (bounded, recency) · host={host} · reason={} — NOT index CURRENT",
                    err.kind()
                );
                self.drift_banner = Some(format!(
                    "DRIFT: no usable CURRENT ({}) — showing bounded filesystem fallback only",
                    err.reason()
                ));
                self.repair_hint = Some(format!(
                    "Repair to restore index truth: {} (wizard Rebuild / `aicx catalog rebuild && aicx index`)",
                    err.recommendation()
                ));
                self.status = format!(
                    "{} fallback hit(s) · scanned {scanned} · repair needed for CURRENT",
                    self.hits.len()
                );
            }
            Err(fallback_err) => {
                self.backend_kind = SearchBackendKind::Failed;
                self.backend_line = format!(
                    "fallback FS also failed · host={host} · primary={}",
                    err.kind()
                );
                self.last_error = Some(format!(
                    "index: {} ({}); fallback: {fallback_err}",
                    err.kind(),
                    err.reason()
                ));
                self.drift_banner = Some(format!(
                    "DRIFT: index {} and filesystem fallback failed",
                    err.kind()
                ));
                self.repair_hint = Some(err.recommendation().to_string());
                self.status = "search failed (index + fallback)".to_string();
            }
        }
    }
}

fn display_opt(value: Option<&str>) -> String {
    value.unwrap_or("*").to_string()
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let output = std::process::Command::new("hostname").output().ok()?;
            if !output.status.success() {
                return None;
            }
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if name.is_empty() { None } else { Some(name) }
        })
}

fn current_generation_label(home: &std::path::Path) -> String {
    let pointer = home
        .join("indexed")
        .join("_all")
        .join("hybrid")
        .join("CURRENT");
    std::fs::read_to_string(pointer)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "none".to_string())
}

fn current_committed_label(home: &std::path::Path) -> String {
    crate::api::index_status_at(home, None)
        .ok()
        .and_then(|s| s.committed_at)
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_does_not_panic() {
        let mut screen = SearchScreen::default();
        screen.run_search();
        assert!(screen.hits.is_empty());
        assert_eq!(screen.backend_kind, SearchBackendKind::NotRun);
    }

    #[test]
    fn hit_label_includes_score_and_agent() {
        let hit = SearchHit {
            score: 42,
            agent: "claude".into(),
            project: "Loctree/aicx".into(),
            date: "2026-07-29".into(),
            session_id: Some("abcdefghijklmno".into()),
            cwd: None,
            path: "/tmp/x".into(),
            snippet: "hello".into(),
            label: "l".into(),
        };
        let label = hit.list_label();
        assert!(label.contains("42"));
        assert!(label.contains("claude"));
        assert!(label.contains("abcdefghijkl"));
    }
}
