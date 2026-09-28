use std::collections::{BTreeMap, BTreeSet};
use std::io::BufReader;
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
    /// Org/repo shown in the wizard. A bare worktree id is replaced only when
    /// the cwd proves a project slug that already exists in this corpus.
    pub project: String,
    /// Project string as stored in the catalog, before display resolution.
    pub stored_project: String,
    pub agent: String,
    pub date: String,
    pub title: String,
    pub cwd: String,
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
    pub org_selected: usize,
    pub repo_selected: usize,
    pub column: CorpusColumn,
    pub search: String,
    pub status: String,
    /// Plain-text operator ask for the selected session. Filled when the
    /// selection changes so the 100ms redraw does not re-read the file.
    pub preview: String,
}

impl CorpusScreen {
    pub fn load() -> Self {
        match load_corpus_items() {
            Ok(files) => Self::from_items(files),
            Err(error) => {
                let status = format!("failed to scan corpus: {error}");
                Self {
                    all_files: Vec::new(),
                    entries: Vec::new(),
                    selected: 0,
                    org_selected: 0,
                    repo_selected: 0,
                    column: CorpusColumn::Chunks,
                    search: String::new(),
                    status: status.clone(),
                    preview: status,
                }
            }
        }
    }

    pub fn from_items(files: Vec<CorpusItem>) -> Self {
        let files = resolve_display_projects(files);
        let entries = files.iter().map(entry_from_file).collect::<Vec<_>>();
        let mut screen = Self {
            all_files: files,
            entries,
            selected: 0,
            org_selected: 0,
            repo_selected: 0,
            column: CorpusColumn::Chunks,
            search: String::new(),
            status: String::new(),
            preview: String::new(),
        };
        screen.refresh_preview();
        screen.status = screen.status_line();
        screen
    }

    pub fn stats_line(&self) -> String {
        let mut orgs = BTreeSet::new();
        let mut repos = BTreeSet::new();
        let mut latest = None::<String>;
        for file in &self.all_files {
            orgs.insert(project_org(&file.project).to_string());
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
            let org = project_org(&file.project);
            if !org.is_empty() {
                values.insert(org.to_string());
            }
        }
        sort_labels(values.into_iter().collect())
    }

    pub fn repos(&self) -> Vec<String> {
        let mut values = BTreeSet::new();
        for file in &self.all_files {
            if !file.project.is_empty() {
                values.insert(file.project.clone());
            }
        }
        sort_labels(values.into_iter().collect())
    }

    pub fn selected_preview(&self) -> String {
        let Some(entry) = self.entries.get(self.selected) else {
            return "No chunk selected.".to_string();
        };
        match operator_ask_from_path(&entry.path) {
            Ok(ask) => ask,
            Err(error) => format!("Failed to read {}: {error}", entry.path.display()),
        }
    }

    pub fn refresh_preview(&mut self) {
        self.preview = self.selected_preview();
    }

    pub fn move_selection(&mut self, delta: isize) {
        match self.column {
            CorpusColumn::Orgs => {
                let len = self.orgs().len();
                self.org_selected = super::move_index(self.org_selected, len, delta);
            }
            CorpusColumn::Repos => {
                let len = self.repos().len();
                self.repo_selected = super::move_index(self.repo_selected, len, delta);
            }
            CorpusColumn::Chunks => {
                self.selected = super::move_index(self.selected, self.entries.len(), delta);
                self.refresh_preview();
                self.status = self.status_line();
            }
        }
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
        self.refresh_preview();
        self.status = self.status_line();
    }
}

fn entry_from_file(file: &CorpusItem) -> CorpusEntry {
    let label = chunk_label(file);
    CorpusEntry {
        label: label.clone(),
        path: file.path.clone(),
        haystack: format!(
            "{} {} {} {} {} {} {}",
            label,
            file.project,
            file.stored_project,
            file.date,
            file.title,
            file.agent,
            file.path.display()
        )
        .to_ascii_lowercase(),
    }
}

fn chunk_label(file: &CorpusItem) -> String {
    let repo = repo_name(&file.project);
    let title = usable_title(&file.title);
    match (file.date.is_empty(), title.is_empty()) {
        (true, true) => repo.to_string(),
        (true, false) => format!("{repo}  {title}"),
        (false, true) => format!("{repo}  {}", file.date),
        (false, false) => format!("{repo}  {}  {title}", file.date),
    }
}

fn load_corpus_items() -> anyhow::Result<Vec<CorpusItem>> {
    let home = crate::aicx_home::resolve()?;
    if crate::catalog::sessions_path_for(&home).is_file() {
        return Ok(crate::catalog::read_entries_at(&home)?
            .into_iter()
            .filter_map(|entry| {
                let project = entry.project?;
                Some(CorpusItem {
                    stored_project: project.clone(),
                    project,
                    agent: entry.agent,
                    date: entry.date.unwrap_or_default(),
                    title: entry.title.unwrap_or_default(),
                    cwd: entry.cwd.unwrap_or_default(),
                    path: PathBuf::from(entry.source_path),
                })
            })
            .collect());
    }
    Ok(legacy_archive::scan_context_files()?
        .into_iter()
        .map(|file| CorpusItem {
            stored_project: file.project.clone(),
            project: file.project,
            agent: file.agent,
            date: file.date_iso,
            title: String::new(),
            cwd: String::new(),
            path: file.path,
        })
        .collect())
}

fn resolve_display_projects(mut items: Vec<CorpusItem>) -> Vec<CorpusItem> {
    let mut known = BTreeMap::<String, String>::new();
    for item in &items {
        if item.stored_project.contains('/') && !is_lost_org_key(project_org(&item.stored_project))
        {
            known
                .entry(item.stored_project.to_ascii_lowercase())
                .or_insert_with(|| item.stored_project.clone());
        }
    }
    for item in &mut items {
        item.project = resolve_display_project(&item.stored_project, &item.cwd, &known);
    }
    items
}

/// Replace a worktree or artifacts id slug with an `org/repo` that this corpus
/// already stores. Anything else stays as the stored slug — including a bare
/// id when no known name is on the cwd.
pub(crate) fn resolve_display_project(
    stored: &str,
    cwd: &str,
    known: &BTreeMap<String, String>,
) -> String {
    if !is_lost_org_key(project_org(stored)) {
        return stored.to_string();
    }
    known_worktree_project(cwd, known).unwrap_or_else(|| stored.to_string())
}

fn known_worktree_project(cwd: &str, known: &BTreeMap<String, String>) -> Option<String> {
    let parts: Vec<&str> = cwd
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect();
    let idx = parts
        .iter()
        .rposition(|segment| *segment == "worktrees" || *segment == "artifacts")?;
    let org = parts.get(idx + 1)?;
    let repo = parts.get(idx + 2)?;
    if is_lost_org_key(org) || is_bare_numeric_id(repo) {
        return None;
    }
    let exact = format!("{}/{}", org.to_ascii_lowercase(), repo.to_ascii_lowercase());
    if let Some(found) = known.get(&exact) {
        return Some(found.clone());
    }
    let next = parts.get(idx + 3)?;
    if is_lost_org_key(next) || is_bare_numeric_id(next) {
        return None;
    }
    let joined = format!(
        "{}/{}-{}",
        org.to_ascii_lowercase(),
        repo.to_ascii_lowercase(),
        next.to_ascii_lowercase()
    );
    known.get(&joined).cloned()
}

fn project_org(project: &str) -> &str {
    project
        .split_once('/')
        .map(|(org, _)| org)
        .unwrap_or(project)
}

fn repo_name(project: &str) -> &str {
    project
        .rsplit_once('/')
        .map(|(_, repo)| repo)
        .unwrap_or(project)
}

/// Zero-padded task ids (`000212`) and worktree date stamps (`2026_0908`).
/// A name that merely starts with a digit (`01_deployed_libraxis_vm`) is not an id.
fn is_lost_org_key(org: &str) -> bool {
    is_bare_numeric_id(org) || is_date_stamp(org)
}

fn is_bare_numeric_id(segment: &str) -> bool {
    let len = segment.len();
    (4..=8).contains(&len) && segment.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_date_stamp(org: &str) -> bool {
    let mut parts = org.split('_');
    let year = parts.next().unwrap_or("");
    let month_day = parts.next().unwrap_or("");
    parts.next().is_none()
        && year.len() == 4
        && month_day.len() == 4
        && year.bytes().all(|byte| byte.is_ascii_digit())
        && month_day.bytes().all(|byte| byte.is_ascii_digit())
}

fn starts_with_letter(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic())
}

fn sort_labels(mut values: Vec<String>) -> Vec<String> {
    values.sort_by(|left, right| {
        let left_name = starts_with_letter(left);
        let right_name = starts_with_letter(right);
        right_name.cmp(&left_name).then_with(|| {
            left.to_ascii_lowercase()
                .cmp(&right.to_ascii_lowercase())
                .then_with(|| left.cmp(right))
        })
    });
    values.dedup();
    values
}

fn usable_title(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "---" || trimmed.eq_ignore_ascii_case("warmup") {
        return String::new();
    }
    let lower = trimmed.to_ascii_lowercase();
    const SKIP: &[&str] = &[
        "you are running under vibecrafted",
        "you are running as a supervised",
        "<recommended_plugins>",
        "runtime_run transcript",
        "<environment_context>",
        "<command-message>",
        "<command-name>",
        "# agents.md instructions",
        "<system-reminder>",
    ];
    if SKIP.iter().any(|prefix| lower.starts_with(prefix)) {
        return String::new();
    }
    let one_line = trimmed
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_words(&one_line, 64)
}

pub(crate) fn present_operator_ask(text: &str) -> String {
    let unwrapped = unwrap_tag(text, "user_query").unwrap_or(text);
    let without_stamp = strip_tag_block(unwrapped, "timestamp");
    let body = if let Some(index) = without_stamp.rfind("Operator prompt:") {
        strip_leading_fence(without_stamp[index + "Operator prompt:".len()..].trim_start())
    } else {
        without_stamp.trim()
    };
    truncate_words(&normalize_plain(body), 1600)
}

fn unwrap_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

fn strip_tag_block(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut rest = text;
    let mut out = String::new();
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        let after = start + open.len();
        if let Some(end) = rest[after..].find(&close) {
            rest = &rest[after + end + close.len()..];
        } else {
            rest = &rest[after..];
            break;
        }
    }
    out.push_str(rest);
    out
}

fn strip_leading_fence(body: &str) -> &str {
    let trimmed = body.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return trimmed;
    };
    let Some(end) = rest.find("---") else {
        return trimmed;
    };
    rest[end + 3..].trim_start()
}

fn normalize_plain(body: &str) -> String {
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs.join("\n\n")
}

fn truncate_words(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let budget = max.saturating_sub(1);
    let mut taken = 0usize;
    let mut end_byte = 0usize;
    let mut last_space = None::<usize>;
    for (index, ch) in value.char_indices() {
        if taken >= budget {
            break;
        }
        if ch == ' ' && taken > 0 {
            last_space = Some(index);
        }
        taken += 1;
        end_byte = index + ch.len_utf8();
    }
    let end = last_space
        .filter(|space| value[..*space].chars().count() >= budget / 2)
        .unwrap_or(end_byte);
    let mut out = value[..end].trim_end().to_string();
    out.push('…');
    out
}

fn operator_ask_from_path(path: &std::path::Path) -> Result<String, String> {
    let file = sanitize::open_file_validated(path).map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(file);
    let mut plain = String::new();
    let mut saw_json = false;
    for _ in 0..40 {
        let Some(capped) = sanitize::read_line_capped(&mut reader, 64 * 1024)
            .map_err(|error| error.to_string())?
        else {
            break;
        };
        let line = capped.line.trim_end_matches(['\n', '\r']).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('{') {
            saw_json = true;
            if let Some(ask) = ask_from_json_line(line) {
                return Ok(ask);
            }
            continue;
        }
        if plain.chars().count() < 2000 {
            if !plain.is_empty() {
                plain.push('\n');
            }
            plain.push_str(line);
        }
    }
    if saw_json {
        return Ok("No operator ask in the first lines of this session.".to_string());
    }
    let plain = normalize_plain(&plain);
    if plain.is_empty() {
        Ok("No operator ask in the first lines of this session.".to_string())
    } else {
        Ok(truncate_words(&plain, 1600))
    }
}

fn ask_from_json_line(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let mut texts = Vec::new();
    collect_user_texts(&value, &mut texts);
    for text in texts {
        if text.contains("Operator prompt:") {
            let ask = present_operator_ask(&text);
            if !ask.is_empty() {
                return Some(ask);
            }
            continue;
        }
        if skip_ask(&text) {
            continue;
        }
        let ask = present_operator_ask(&text);
        if !ask.is_empty() {
            return Some(ask);
        }
    }
    None
}

fn skip_ask(text: &str) -> bool {
    let head = text.trim_start();
    let lower = head.to_ascii_lowercase();
    lower.starts_with("<environment_context>")
        || lower.starts_with("<recommended_plugins>")
        || crate::extraction::is_harness_injected_noise("user", head)
}

fn collect_user_texts(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_user_texts(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            if is_user_object(map) {
                if let Some(text) = object_text(map)
                    && !text.trim().is_empty()
                {
                    out.push(text);
                }
                return;
            }
            for child in map.values() {
                collect_user_texts(child, out);
            }
        }
        _ => {}
    }
}

fn is_user_object(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    let role = map.get("role").and_then(|value| value.as_str());
    let kind = map.get("type").and_then(|value| value.as_str());
    role == Some("user") || kind == Some("user") || kind == Some("user_message")
}

fn object_text(map: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    if let Some(content) = map.get("content")
        && let Some(text) = content_to_text(content)
    {
        return Some(text);
    }
    if let Some(message) = map.get("message") {
        if let Some(text) = message.as_str() {
            return Some(text.to_string());
        }
        if let Some(nested) = message.as_object() {
            return object_text(nested);
        }
    }
    map.get("text")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn content_to_text(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(text) = item.as_str() {
                    parts.push(text.to_string());
                    continue;
                }
                let Some(object) = item.as_object() else {
                    continue;
                };
                let kind = object
                    .get("type")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                if kind == "tool_use" || kind == "tool_result" {
                    continue;
                }
                if let Some(text) = object.get("text").and_then(|value| value.as_str()) {
                    parts.push(text.to_string());
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        serde_json::Value::Object(map) => object_text(map),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
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

    fn item(project: &str, cwd: &str, title: &str, path: PathBuf) -> CorpusItem {
        CorpusItem {
            project: project.to_string(),
            stored_project: project.to_string(),
            agent: "cursor".to_string(),
            date: "2026-09-10".to_string(),
            title: title.to_string(),
            cwd: cwd.to_string(),
            path,
        }
    }

    #[test]
    fn numeric_org_resolves_to_known_worktree_project() {
        let cwd = "/Users/polyversai/vibecrafted/worktrees/vetcoders/vibecrafted/2026/0909/cursor/work/260910/000212/26389";
        let screen = CorpusScreen::from_items(vec![
            item(
                "vetcoders/vibecrafted",
                "/Users/maciejgad/vc-workspace/vetcoders/vibecrafted",
                "Keep the catalog",
                PathBuf::from("/tmp/aicx-missing-named.jsonl"),
            ),
            item(
                "000212/26389",
                cwd,
                "You are running under Vibecrafted core runtime.",
                PathBuf::from("/tmp/aicx-missing-id.jsonl"),
            ),
        ]);

        assert_eq!(screen.all_files.len(), 2);
        assert_eq!(screen.entries.len(), 2);
        assert_eq!(screen.all_files[1].project, "vetcoders/vibecrafted");
        assert_eq!(screen.all_files[1].stored_project, "000212/26389");
        let orgs = screen.orgs();
        assert_eq!(orgs, vec!["vetcoders".to_string()]);
        assert!(orgs.iter().all(|org| !is_bare_numeric_id(org)));
        assert!(
            screen.entries[1]
                .label
                .starts_with("vibecrafted  2026-09-10")
        );
        assert!(!screen.entries[1].label.contains("000212"));
    }

    #[test]
    fn unresolved_numeric_org_keeps_the_stored_slug() {
        let screen = CorpusScreen::from_items(vec![item(
            "000212/26389",
            "/tmp/not-a-worktree/000212/26389",
            "Repair the lock",
            PathBuf::from("/tmp/aicx-missing-unresolved.jsonl"),
        )]);
        assert_eq!(screen.all_files[0].project, "000212/26389");
        assert_eq!(screen.orgs(), vec!["000212".to_string()]);
    }

    #[test]
    fn named_project_is_not_rewritten_from_cwd() {
        let screen = CorpusScreen::from_items(vec![
            item(
                "vetcoders/vibecrafted",
                "/tmp/vetcoders/vibecrafted",
                "named",
                PathBuf::from("/tmp/aicx-missing-a.jsonl"),
            ),
            item(
                "loctree/aicx",
                "/Users/polyversai/.vibecrafted/worktrees/vetcoders/vibecrafted/2026_0908/task",
                "Repair the corpus column",
                PathBuf::from("/tmp/aicx-missing-b.jsonl"),
            ),
        ]);
        assert_eq!(screen.all_files[1].project, "loctree/aicx");
        assert!(
            screen.entries[1]
                .label
                .starts_with("aicx  2026-09-10  Repair")
        );
    }

    #[test]
    fn artifacts_stamp_resolves_to_the_known_project() {
        let screen = CorpusScreen::from_items(vec![
            item(
                "libraxis/vc-runtime",
                "/tmp/libraxis/vc-runtime",
                "named",
                PathBuf::from("/tmp/aicx-missing-art.jsonl"),
            ),
            item(
                "2026_0524/plans",
                "/Users/polyversai/.vibecrafted/artifacts/Libraxis/vc-runtime/2026_0524/plans",
                "Warmup",
                PathBuf::from("/tmp/aicx-missing-plans.jsonl"),
            ),
        ]);
        assert_eq!(screen.all_files[1].project, "libraxis/vc-runtime");
        assert!(!screen.orgs().iter().any(|org| org == "2026_0524"));
    }

    #[test]
    fn date_stamp_org_resolves_when_the_name_already_exists() {
        let screen = CorpusScreen::from_items(vec![
            item(
                "vetcoders/vibecrafted",
                "/tmp/vetcoders/vibecrafted",
                "named",
                PathBuf::from("/tmp/aicx-missing-c.jsonl"),
            ),
            item(
                "2026_0908/web-scaffold-shell",
                "/Users/polyversai/.vibecrafted/worktrees/vetcoders/vibecrafted/2026_0908/WEB-scaffold-shell",
                "Warmup",
                PathBuf::from("/tmp/aicx-missing-d.jsonl"),
            ),
        ]);
        assert_eq!(screen.all_files[1].project, "vetcoders/vibecrafted");
        assert_eq!(screen.all_files.len(), 2);
    }

    #[test]
    fn search_still_matches_the_stored_id_and_keeps_every_session() {
        let screen_items = vec![
            item(
                "vetcoders/vibecrafted",
                "/tmp/vetcoders/vibecrafted",
                "named",
                PathBuf::from("/tmp/aicx-missing-e.jsonl"),
            ),
            item(
                "000212/26389",
                "/Users/polyversai/vibecrafted/worktrees/vetcoders/vibecrafted/000212/26389",
                "Repair the lock",
                PathBuf::from("/tmp/aicx-missing-f.jsonl"),
            ),
        ];
        let mut screen = CorpusScreen::from_items(screen_items);
        assert_eq!(screen.all_files.len(), 2);
        screen.apply_search("000212".to_string());
        assert_eq!(screen.entries.len(), 1);
        assert_eq!(screen.all_files.len(), 2);
        assert!(screen.stats_line().starts_with("2 sessions"));
        screen.apply_search(String::new());
        assert_eq!(screen.entries.len(), 2);
    }

    #[test]
    fn letter_orgs_sort_ahead_of_dot_dirs() {
        let screen = CorpusScreen::from_items(vec![
            item(
                ".codex",
                "/Users/maciejgad/.codex",
                "dot",
                PathBuf::from("/tmp/aicx-missing-g.jsonl"),
            ),
            item(
                "vetcoders/vibecrafted",
                "/tmp/vetcoders/vibecrafted",
                "named",
                PathBuf::from("/tmp/aicx-missing-h.jsonl"),
            ),
        ]);
        assert_eq!(
            screen.orgs(),
            vec!["vetcoders".to_string(), ".codex".to_string()]
        );
    }

    #[test]
    fn present_operator_ask_is_plain_text() {
        let raw = "<timestamp>Thursday, Sep 10, 2026, 12:02 AM (UTC+2)</timestamp>\n<user_query>\nYou are running under Vibecrafted core runtime.\n\nOperator prompt:\n---\nmodel: cursor\n---\nRepair the corpus column\n</user_query>";
        let ask = present_operator_ask(raw);
        assert_eq!(ask, "Repair the corpus column");
        assert!(!ask.contains("\\n"));
        assert!(!ask.contains("\"type\""));
    }

    #[test]
    fn selected_preview_reads_operator_ask_not_json() {
        let dir = unique_test_dir("ask");
        let path = dir.join("session.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"<user_query>\nOperator prompt:\nRepair the corpus column\n</user_query>"}}]}}}}"#
        )
        .unwrap();
        file.set_len((sanitize::MAX_VALIDATED_BYTES + 1) as u64)
            .unwrap();
        let screen = CorpusScreen::from_items(vec![item(
            "vetcoders/vibecrafted",
            "/tmp/vetcoders/vibecrafted",
            "Repair the corpus column",
            path,
        )]);

        let preview = screen.selected_preview();
        assert!(preview.contains("Repair the corpus column"), "{preview}");
        assert!(!preview.contains("\"type\""));
        assert!(!preview.contains("\\n"));
        assert!(!preview.contains("exceeds validated read cap"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn many_sessions_stay_in_the_corpus() {
        let items = (0..250)
            .map(|index| {
                item(
                    "vetcoders/vibecrafted",
                    "/tmp/vetcoders/vibecrafted",
                    "named",
                    PathBuf::from(format!("/tmp/aicx-missing-{index}.jsonl")),
                )
            })
            .collect::<Vec<_>>();
        let screen = CorpusScreen::from_items(items);
        assert_eq!(screen.all_files.len(), 250);
        assert_eq!(screen.entries.len(), 250);
        assert!(screen.stats_line().starts_with("250 sessions"));
    }

    #[test]
    fn live_catalog_orgs_are_human_names() {
        let Ok(home) = crate::aicx_home::resolve() else {
            return;
        };
        if !crate::catalog::sessions_path_for(&home).is_file() {
            return;
        }
        let screen = CorpusScreen::load();
        assert!(
            screen.all_files.len() >= 16_000,
            "session count {}",
            screen.all_files.len()
        );
        assert_eq!(screen.entries.len(), screen.all_files.len());
        let orgs = screen.orgs();
        let bare = orgs
            .iter()
            .filter(|org| is_bare_numeric_id(org))
            .cloned()
            .collect::<Vec<_>>();
        assert!(bare.is_empty(), "bare numeric orgs leaked: {bare:?}");
        assert!(orgs.iter().any(|org| org == "vetcoders"), "{orgs:?}");
        assert!(
            orgs.first().is_some_and(|org| starts_with_letter(org)),
            "first org {:?}",
            orgs.first()
        );
        assert!(
            screen
                .entries
                .iter()
                .any(|entry| entry.label.contains("2026-"))
        );
    }
}
