//! Interactive init flow for ai-contexters.
//!
//! Creates `.ai-context/` structure, builds context/prompt,
//! runs the selected agent, and updates share artifacts.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{Local, Utc};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::output::{self, OutputConfig, OutputFormat, OutputMode, ReportMetadata, TimelineEntry};
use crate::sanitize;
use crate::sources::{self, ExtractionConfig};

const SUMMARY_START: &str = "===AI_CONTEXT_SUMMARY_START===";
const SUMMARY_END: &str = "===AI_CONTEXT_SUMMARY_END===";
const CLAUDE_JQ_FILTER: &str = "if .delta?.text then .delta.text \
    elif .content_block?.text then .content_block.text \
    elif .message?.content then (.message.content[]? | select(.type==\"text\") | .text) \
    else empty end";
const SUMMARY_MAX_LINES: usize = 500;
const PROMPT_SIZE_WARN_BYTES: usize = 180_000;

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub project: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub horizon_hours: u64,
    pub max_lines: usize,
    pub include_assistant: bool,
    pub redact_secrets: bool,
    pub action: Option<String>,
    pub agent_prompt: Option<String>,
    pub agent_prompt_file: Option<PathBuf>,
    pub no_run: bool,
    pub no_confirm: bool,
    pub no_gitignore: bool,
}

struct InitPaths {
    share: PathBuf,
    local: PathBuf,
    context: PathBuf,
    prompts: PathBuf,
    logs: PathBuf,
    runs: PathBuf,
    config: PathBuf,
    summary: PathBuf,
    timeline: PathBuf,
}

struct Logger {
    file: File,
}

impl Logger {
    fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| format!("Failed to create log: {}", path.display()))?;
        Ok(Self { file })
    }

    fn line(&mut self, msg: &str) {
        println!("{}", msg);
        let _ = writeln!(self.file, "{}", msg);
    }

    fn warn(&mut self, msg: &str) {
        eprintln!("{}", msg);
        let _ = writeln!(self.file, "[WARN] {}", msg);
    }
}

pub fn run_init(mut options: InitOptions) -> Result<()> {
    let root = repo_root()?;
    let project = options.project.unwrap_or_else(sources::detect_project_name);

    let run_id = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let ts_local = Local::now().format("%Y-%m-%d %H:%M").to_string();

    let paths = init_paths(&root)?;
    ensure_dirs(&paths)?;
    if !options.no_gitignore {
        update_gitignore(&root)?;
    }

    let log_path = paths.logs.join(format!("{}.log", run_id));
    let mut log = Logger::new(&log_path)?;

    log.line("== aicx init ==");
    log.line(&format!("root:    {}", root.display()));
    log.line(&format!("project: {}", project));
    if let Some(action) = options.action.as_deref() {
        log.line(&format!("action:  {}", action));
    }
    if options.agent_prompt.is_some() {
        log.line("agent_prompt: (provided)");
    }
    if let Some(path) = options.agent_prompt_file.as_deref() {
        log.line(&format!("agent_prompt_file: {}", path.display()));
    }
    log.line(&format!("run_id:  {}", run_id));
    log.line(&format!("log:     {}", log_path.display()));
    log.line("");

    ensure_loct(&mut log)?;
    write_meta(&root, &paths, &project, &run_id)?;

    log.line("== Step 1/4: loct auto ==");
    run_loct_auto(&root)?;
    log.line("");

    let loct_path = paths.context.join(format!("{}_loct_for_ai.md", run_id));
    log.line(&format!(
        "== Step 2/4: loct --for-ai -> {} ==",
        loct_path.display()
    ));
    write_loct_for_ai(&root, &loct_path)?;
    log.line("");

    log.line("== Step 3/4: aicx context ==");
    extract_context(
        &paths.context,
        &project,
        options.horizon_hours,
        options.include_assistant,
        options.redact_secrets,
    )?;
    log.line("");

    let prompt_path = paths.prompts.join(format!("{}_prompt.md", run_id));
    log.line(&format!(
        "== Step 4/4: build prompt -> {} ==",
        prompt_path.display()
    ));
    let agent_prompt = resolve_agent_prompt(
        options.agent_prompt.take(),
        options.agent_prompt_file.take(),
    )?;
    let prompt_bytes = build_prompt(
        &prompt_path,
        &paths.context,
        &ts_local,
        options.action.as_deref(),
        agent_prompt.as_deref(),
    )?;
    if prompt_bytes > PROMPT_SIZE_WARN_BYTES {
        log.warn(&format!(
            "prompt is big ({}B). If agent fails, lower max lines (e.g., 600).",
            prompt_bytes
        ));
    }
    log.line("");

    if options.no_run {
        log.line("Init complete (no-run).");
        log.line(&format!("[prompt] {}", prompt_path.display()));
        log.line(&format!("[loct]   {}", loct_path.display()));
        log.line(&format!("[log]    {}", log_path.display()));
        return Ok(());
    }

    let agent = if options.no_confirm && options.agent.is_none() {
        "codex".to_string()
    } else {
        resolve_agent(options.agent)?
    };
    onboarding(&mut log, &agent)?;
    check_agent(&agent)?;
    if !confirm_claude_risk(&mut log, &agent)? {
        log.line("Run canceled.");
        log.line(&format!("[prompt] {}", prompt_path.display()));
        log.line(&format!("[loct]   {}", loct_path.display()));
        log.line(&format!("[log]    {}", log_path.display()));
        return Ok(());
    }

    if !options.no_confirm {
        let run = confirm_run(&mut log)?;
        if !run {
            log.line("Run canceled.");
            log.line(&format!("[prompt] {}", prompt_path.display()));
            log.line(&format!("[loct]   {}", loct_path.display()));
            log.line(&format!("[log]    {}", log_path.display()));
            return Ok(());
        }
    }

    let model = options.model.as_deref();

    let report_path = paths.runs.join(format!("{}_report.md", run_id));
    let report = run_agent(&root, &agent, model, &prompt_path, &report_path)?;

    let (clean_report, summary_block) = split_summary_block(&report);
    append_timeline(
        &paths.timeline,
        &ts_local,
        &agent,
        model,
        &run_id,
        &clean_report,
    )?;

    let mut summary_written = false;
    match summary_block {
        Some(block) => {
            append_summary(&paths.summary, &block)?;
            trim_summary(&paths.summary, SUMMARY_MAX_LINES)?;
            summary_written = true;
        }
        None => {
            log.warn("summary block missing; summary.md not updated");
        }
    }

    log.line("");
    log.line("== DONE ==");
    log.line(&format!("[report] {}", report_path.display()));
    log.line(&format!("[prompt] {}", prompt_path.display()));
    log.line(&format!("[loct]   {}", loct_path.display()));
    log.line(&format!("[summary] {}", paths.summary.display()));
    log.line(&format!("[timeline] {}", paths.timeline.display()));
    log.line(&format!("[log]    {}", log_path.display()));
    log.line("");
    log.line("== SUMMARY ==");
    log.line(&format!("Agent: {}", agent));
    log.line(&format!("Report: {}", report_path.display()));
    log.line(&format!("Timeline: {} (updated)", paths.timeline.display()));
    if summary_written {
        log.line(&format!("Summary: {} (updated)", paths.summary.display()));
    } else {
        log.line(&format!(
            "Summary: {} (not updated)",
            paths.summary.display()
        ));
    }
    log.line("Next steps:");
    log.line(&format!("1) Review the report: {}", report_path.display()));
    log.line(&format!(
        "2) Scan the timeline: {}",
        paths.timeline.display()
    ));
    log.line(&format!(
        "3) Check the summary: {}",
        paths.summary.display()
    ));

    Ok(())
}

fn repo_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output();

    if let Ok(out) = output
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return Ok(PathBuf::from(s));
        }
    }

    std::env::current_dir().context("Failed to get current dir")
}

fn init_paths(root: &Path) -> Result<InitPaths> {
    let base = root.join(".ai-context");
    let share = base.join("share");
    let local = base.join("local");
    let context = local.join("context");
    let prompts = local.join("prompts");
    let logs = local.join("logs");
    let runs = local.join("runs");
    let config = local.join("config");

    let summary = share.join("summary.md");
    let timeline = share.join("timeline.md");

    Ok(InitPaths {
        share,
        local,
        context,
        prompts,
        logs,
        runs,
        config,
        summary,
        timeline,
    })
}

fn ensure_dirs(paths: &InitPaths) -> Result<()> {
    fs::create_dir_all(&paths.share)?;
    fs::create_dir_all(&paths.context)?;
    fs::create_dir_all(&paths.prompts)?;
    fs::create_dir_all(&paths.logs)?;
    fs::create_dir_all(&paths.runs)?;
    fs::create_dir_all(&paths.config)?;
    fs::create_dir_all(paths.local.join("state"))?;
    fs::create_dir_all(paths.local.join("memex"))?;
    Ok(())
}

fn update_gitignore(root: &Path) -> Result<()> {
    let gitignore = root.join(".gitignore");
    let mut content = String::new();
    if gitignore.exists() {
        content = fs::read_to_string(&gitignore)?;
    }

    let mut needs_update = true;

    for line in content.lines() {
        if line.trim() == ".ai-context/*" {
            needs_update = false;
            break;
        }
    }

    if needs_update {
        let block = [
            "",
            "# AI Context",
            ".ai-context/*",
            "!.ai-context/share/",
            "!.ai-context/share/artifacts/",
            "!.ai-context/share/artifacts/SUMMARY.md",
            "!.ai-context/share/artifacts/TIMELINE.md",
            "!.ai-context/share/artifacts/TRIAGE.md",
            "!.ai-context/share/artifacts/prompts/",
            "!.ai-context/share/artifacts/prompts/*.md",
        ]
        .join("\n");

        let mut new_content = content;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str(&block);
        if !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        fs::write(&gitignore, new_content)?;
    }

    Ok(())
}

fn ensure_loct(log: &mut Logger) -> Result<()> {
    let loct_bin = resolve_loct_bin()?;
    let ok = Command::new(&loct_bin).arg("--version").output().is_ok();
    if !ok {
        log.warn("loct not found in PATH");
        anyhow::bail!("loct is required for init");
    }
    Ok(())
}

fn write_meta(root: &Path, paths: &InitPaths, project: &str, run_id: &str) -> Result<()> {
    let git_sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let git_dirty = Command::new("git")
        .args(["diff", "--quiet"])
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);

    let loct_version = resolve_loct_bin()
        .ok()
        .and_then(|bin| {
            Command::new(bin)
                .arg("--version")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();

    let ai_contexters_version = env!("CARGO_PKG_VERSION").to_string();

    let meta = serde_json::json!({
        "project": project,
        "root": root,
        "run_id": run_id,
        "created_at": Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "git_sha": git_sha,
        "git_dirty": git_dirty,
        "tool_versions": {
            "loct": loct_version,
            "ai_contexters": ai_contexters_version,
        }
    });

    let meta_path = paths.local.join("meta.json");
    fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;
    Ok(())
}

fn run_loct_auto(root: &Path) -> Result<()> {
    let loct_bin = resolve_loct_bin()?;
    let status = Command::new(&loct_bin)
        .arg("auto")
        .current_dir(root)
        .status()
        .context("Failed to run loct auto")?;
    if !status.success() {
        anyhow::bail!("loct auto failed");
    }
    Ok(())
}

fn write_loct_for_ai(root: &Path, out_path: &Path) -> Result<()> {
    let loct_bin = resolve_loct_bin()?;
    let output = Command::new(&loct_bin)
        .arg("--for-ai")
        .current_dir(root)
        .output()
        .context("Failed to run loct --for-ai")?;

    if !output.status.success() {
        anyhow::bail!("loct --for-ai failed");
    }

    fs::write(out_path, output.stdout)?;
    Ok(())
}

fn resolve_loct_bin() -> Result<PathBuf> {
    if let Ok(val) = std::env::var("LOCT_BIN") {
        let path = PathBuf::from(val);
        if path.exists() {
            return Ok(path);
        }
    }

    let path_var = std::env::var_os("PATH").ok_or_else(|| anyhow::anyhow!("PATH is not set"))?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("loct");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(anyhow::anyhow!("loct not found in PATH"))
}

fn extract_context(
    out_dir: &Path,
    project: &str,
    hours: u64,
    include_assistant: bool,
    redact_secrets: bool,
) -> Result<Vec<TimelineEntry>> {
    let cutoff = Utc::now() - chrono::Duration::hours(hours as i64);
    let config = ExtractionConfig {
        project_filter: vec![project.to_string()],
        cutoff,
        include_assistant,
        watermark: None,
    };

    let entries = sources::extract_all(&config)?;
    let output_entries: Vec<TimelineEntry> = entries
        .iter()
        .map(|e| TimelineEntry {
            timestamp: e.timestamp,
            agent: e.agent.clone(),
            session_id: e.session_id.clone(),
            role: e.role.clone(),
            message: if redact_secrets {
                crate::redact::redact_secrets(&e.message)
            } else {
                e.message.clone()
            },
            branch: e.branch.clone(),
            cwd: e.cwd.clone(),
        })
        .collect();

    let mut sessions: Vec<String> = entries.iter().map(|e| e.session_id.clone()).collect();
    sessions.sort();
    sessions.dedup();

    let metadata = ReportMetadata {
        generated_at: Utc::now(),
        project_filter: Some(project.to_string()),
        hours_back: hours,
        total_entries: output_entries.len(),
        sessions,
    };

    let out_config = OutputConfig {
        dir: out_dir.to_path_buf(),
        format: OutputFormat::Both,
        mode: OutputMode::NewFile,
        max_files: 0,
        max_message_chars: 0,
        include_loctree: false,
        project_root: None,
    };

    output::write_report(&out_config, &output_entries, &metadata)?;
    Ok(output_entries)
}

fn resolve_agent_prompt(inline: Option<String>, file: Option<PathBuf>) -> Result<Option<String>> {
    let mut merged = inline.filter(|s| !s.trim().is_empty());

    if let Some(path) = file {
        let content = sanitize::read_to_string_validated(&path)
            .with_context(|| format!("Failed to read agent prompt file: {}", path.display()))?;
        if !content.trim().is_empty() {
            merged = Some(match merged {
                Some(mut existing) => {
                    existing.push_str("\n\n");
                    existing.push_str(&content);
                    existing
                }
                None => content,
            });
        }
    }

    Ok(merged.filter(|s| !s.trim().is_empty()))
}

fn build_prompt(
    prompt_path: &Path,
    context_dir: &Path,
    ts_local: &str,
    action: Option<&str>,
    agent_prompt: Option<&str>,
) -> Result<usize> {
    let mut file = sanitize::create_file_validated(prompt_path)?;

    let share_dir = context_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("share"));
    let artifacts_root = share_dir
        .clone()
        .map(|d| d.join("artifacts"))
        .unwrap_or_else(|| PathBuf::from(".ai-context/share/artifacts"));
    let summary_path = artifacts_root.join("SUMMARY.md");
    let timeline_path = artifacts_root.join("TIMELINE.md");
    let triage_path = artifacts_root.join("TRIAGE.md");
    let prompts_dir = artifacts_root.join("prompts");

    writeln!(file, "You are an agent initializing a repository.")?;
    writeln!(file)?;
    if let Some(action) = action {
        writeln!(file, "Action focus (must address):")?;
        writeln!(file, "- {}", action)?;
        writeln!(file)?;
    }
    if let Some(agent_prompt) = agent_prompt {
        writeln!(file, "Additional agent prompt:")?;
        writeln!(file)?;
        file.write_all(agent_prompt.as_bytes())?;
        if !agent_prompt.ends_with('\n') {
            writeln!(file)?;
        }
        writeln!(file)?;
    }
    writeln!(file, "Rules:")?;
    writeln!(
        file,
        "- You MUST use tools; do not answer without running tools."
    )?;
    writeln!(
        file,
        "- Explore ai-contexters artifacts (paths listed below) newest-first until you can form a coherent picture of work on this repo."
    )?;
    writeln!(
        file,
        "- While exploring, append entries to TIMELINE.md (newest on top)."
    )?;
    writeln!(
        file,
        "- Use the loctree MCP tool for repo structure; do not rely on injected `loct --for-ai` output."
    )?;
    writeln!(
        file,
        "- After identifying red flags and hot spots, you may read repo code to confirm."
    )?;
    writeln!(file, "- Do not hallucinate technical facts.")?;
    writeln!(file, "- Language: English. Tone: direct and concise.")?;
    writeln!(file)?;
    writeln!(file, "Exploration schedule (required):")?;
    writeln!(
        file,
        "1) ai-contexters artifacts (newest-first) + update TIMELINE.md."
    )?;
    writeln!(file, "2) Identify red flags / hot spots.")?;
    writeln!(
        file,
        "3) Selective verification in repo code (confirm only)."
    )?;
    writeln!(file, "4) TRIAGE.md (unfinished implementations, P0-P2).")?;
    writeln!(file, "5) Generate task prompts (\"Emil Kurier\" format).")?;
    writeln!(file)?;
    writeln!(file, "Artifacts to produce (write to):")?;
    writeln!(
        file,
        "- SUMMARY.md: {} (max 120 lines, concise but complete; add a section \"Recent PRs (default branch)\").",
        summary_path.display()
    )?;
    writeln!(
        file,
        "- TIMELINE.md: {} (newest on top; append during exploration; no length limit).",
        timeline_path.display()
    )?;
    writeln!(
        file,
        "- TRIAGE.md: {} (unfinished implementations with triage P0/P1/P2 + rationale and sources).",
        triage_path.display()
    )?;
    writeln!(
        file,
        "- PROMPTS/: {} (one file per task group named `%TIMESTAMP_PROMPT_<TITLE>_P0.md` / `_P1.md` / `_P2.md`).",
        prompts_dir.display()
    )?;
    writeln!(file)?;
    writeln!(file, "Quality gate:")?;
    writeln!(
        file,
        "- If evidence is insufficient, do NOT generate prompts for that task group."
    )?;
    writeln!(
        file,
        "- In that case, write a short \"Missing Info\" note into TRIAGE.md with what is needed."
    )?;
    writeln!(file, "- Before finalizing, run:")?;
    writeln!(file, "  - `cargo clippy -- -D warnings`")?;
    writeln!(file, "  - `semgrep scan --config auto`")?;
    writeln!(
        file,
        "  - project tests (if missing, state what should be run)."
    )?;
    writeln!(file)?;
    writeln!(file, "Task prompt requirements (\"Emil Kurier\" format):")?;
    writeln!(
        file,
        "- Each file is a ready-to-paste prompt for another agent (no pre/post commentary)."
    )?;
    writeln!(file, "- Preserve intent 1:1; do not ask for more details.")?;
    writeln!(file, "- Always includes:")?;
    writeln!(file, "  1) Task description from the current transcript")?;
    writeln!(file, "  2) Initial context")?;
    writeln!(file, "  3) Actionable todo list with [ ] / [x] blocks:")?;
    writeln!(file, "     - investigate code")?;
    writeln!(file, "     - implement")?;
    writeln!(file, "     - verify integrity (format, lint)")?;
    writeln!(file, "     - tests (if missing, you MUST write them)")?;
    writeln!(file, "  4) Expected deliverables")?;
    writeln!(file, "  5) Report expectation")?;
    writeln!(
        file,
        "  6) Appendix: tooling (loctree: `loct auto`, `loct --for-ai`, `loct find <...>`, clippy, etc.) + anti-technical-debt rules (e.g. \"remove all legacy leftovers after refactor\")."
    )?;
    writeln!(
        file,
        "  7) Call to action and a cheesy joke + kaomoji (no emoji). Both must be in English, unique, and the kaomoji must be creative (not a common/recycled one)."
    )?;
    writeln!(
        file,
        "- Tone: tolerant, motivating, empathetic. Kaomoji ALWAYS. No emoji anywhere."
    )?;
    writeln!(file)?;
    writeln!(file, "TRIAGE.md requirements:")?;
    writeln!(
        file,
        "- Each item MUST include a source pointer: artifact path + section/line if available (e.g. `path#L12` or `path:SectionName`)."
    )?;
    writeln!(
        file,
        "- Group items by domain (e.g. Auth Context, Loctree Refactor, Vista UI), then assign P0/P1/P2 within each group."
    )?;
    writeln!(file)?;
    writeln!(
        file,
        "Additionally, output a curated summary block for summary.md in EXACT format:"
    )?;
    writeln!(file, "{}", SUMMARY_START)?;
    writeln!(file, "## {}", ts_local)?;
    writeln!(file, "- Goal: ...")?;
    writeln!(file, "- Done: ...")?;
    writeln!(file, "- Decisions: ...")?;
    writeln!(file, "- Risks: ...")?;
    writeln!(file, "{}", SUMMARY_END)?;
    writeln!(file, "Keep bullets short and factual.")?;
    writeln!(file)?;
    writeln!(file, "ARTIFACTS (paths only, newest first):")?;
    writeln!(file)?;

    let mut artifacts: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();

    if context_dir.exists() {
        for entry in sanitize::read_dir_validated(context_dir)?.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file()
                && !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains("loct_for_ai"))
                && let Ok(meta) = path.metadata()
                && let Ok(mtime) = meta.modified()
            {
                artifacts.push((path, mtime));
            }
        }
    }

    // Also include shared artifacts if present.
    let share_dir = context_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("share"));
    if let Some(dir) = share_dir
        && dir.exists()
    {
        for entry in sanitize::read_dir_validated(&dir)?.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file()
                && let Ok(meta) = path.metadata()
                && let Ok(mtime) = meta.modified()
            {
                artifacts.push((path, mtime));
            }
        }
    }

    artifacts.sort_by(|a, b| b.1.cmp(&a.1));

    if artifacts.is_empty() {
        writeln!(file, "(no artifacts found)")?;
    } else {
        for (path, mtime) in &artifacts {
            let ts = chrono::DateTime::<Local>::from(*mtime).format("%Y-%m-%d %H:%M:%S");
            writeln!(file, "- {} (mtime: {})", path.display(), ts)?;
        }
    }
    writeln!(file)?;

    writeln!(file, "Notes:")?;
    writeln!(
        file,
        "- Do not rely on injected context blocks; open artifacts directly using tools."
    )?;
    writeln!(
        file,
        "- Use loctree MCP tool (repo-view/tree/focus/slice/find/impact) to build structure context."
    )?;
    writeln!(file)?;

    file.flush()?;
    let bytes = fs::metadata(prompt_path)?.len() as usize;
    Ok(bytes)
}

fn resolve_agent(agent: Option<String>) -> Result<String> {
    if let Some(a) = agent {
        let a = normalize_agent(&a)?;
        return Ok(a);
    }

    println!("AI Contexters is pure rust cli tool to retrieve and manage the agentic codebases.");
    println!("It is free and local only, but it requires a compatible coding agent to run.");
    println!("Pick the run agent:");
    println!("(c)laude | (o)dex");

    loop {
        print!("> ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim();
        if input.is_empty() {
            return Ok("codex".to_string());
        }
        match normalize_agent(input) {
            Ok(agent) => return Ok(agent),
            Err(_) => println!("Invalid choice, use c/claude or o/codex."),
        }
    }
}

fn normalize_agent(agent: &str) -> Result<String> {
    let lower = agent.to_lowercase();
    match lower.as_str() {
        "c" | "claude" => Ok("claude".to_string()),
        "o" | "codex" => Ok("codex".to_string()),
        _ => anyhow::bail!("unknown agent"),
    }
}

fn onboarding(log: &mut Logger, agent: &str) -> Result<()> {
    log.line("");
    log.line("[spinner]...");
    log.line("checking...");
    log.line(&format!("agent: {}", agent));
    log.line("Configuration ok!");
    Ok(())
}

fn confirm_claude_risk(log: &mut Logger, agent: &str) -> Result<bool> {
    if agent != "claude" {
        return Ok(true);
    }
    log.line("Memory bloat possible - do you accept the risk? (y)es / (n)o");
    print!("> ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();
    Ok(input == "y" || input == "yes")
}

fn check_agent(agent: &str) -> Result<()> {
    let safe = sanitize::safe_agent_name(agent)?;
    // SECURITY: agent name validated via safe_agent_name allowlist (claude, codex only)
    let ok = Command::new(safe).arg("--version").output().is_ok(); // nosemgrep: rust.actix.command-injection.rust-actix-command-injection.rust-actix-command-injection
    if !ok {
        anyhow::bail!("{} not found in PATH", safe);
    }
    Ok(())
}

fn confirm_run(log: &mut Logger) -> Result<bool> {
    log.line("Run? (y)es / (n)o");
    print!("> ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();
    if input.is_empty() || input == "y" || input == "yes" {
        Ok(true)
    } else {
        Ok(false)
    }
}

fn run_agent(
    root: &Path,
    agent: &str,
    model: Option<&str>,
    prompt_path: &Path,
    report_path: &Path,
) -> Result<String> {
    match agent {
        "claude" => run_claude(model, prompt_path, report_path),
        "codex" => run_codex(model, prompt_path, report_path, root),
        _ => anyhow::bail!("unknown agent: {}", agent),
    }
}

fn run_claude(model: Option<&str>, prompt_path: &Path, report_path: &Path) -> Result<String> {
    let validated_prompt = sanitize::validate_read_path(prompt_path)?;
    let bootstrap = bootstrap_prompt(&validated_prompt);
    if should_dispatch_terminal() {
        let validated_report = sanitize::validate_write_path(report_path)?;

        let mut cmd = String::from(
            "claude --dangerously-skip-permissions -p --output-format stream-json --include-partial-messages --verbose",
        );
        if let Some(model) = model {
            cmd.push_str(" --model ");
            cmd.push_str(&shell_escape_str(model));
        }
        cmd.push(' ');
        cmd.push_str(&shell_escape_str(&bootstrap));
        cmd.push_str(" | jq -r ");
        cmd.push_str(&shell_escape_str(CLAUDE_JQ_FILTER));
        cmd.push_str(" | awk '1' > ");
        cmd.push_str(&shell_escape(&validated_report));

        dispatch_terminal(&cmd)?;
        wait_for_report(&validated_report, std::time::Duration::from_secs(60 * 60))?;
    } else {
        let mut cmd = Command::new("claude");
        cmd.arg("--dangerously-skip-permissions")
            .arg("-p")
            .arg("--output-format")
            .arg("text");

        if let Some(model) = model {
            cmd.arg("--model").arg(model);
        }

        let output = cmd
            .arg(bootstrap)
            .output()
            .context("Failed to run claude")?;

        if !output.status.success() {
            anyhow::bail!("claude command failed");
        }

        let report = String::from_utf8_lossy(&output.stdout).to_string();
        fs::write(report_path, &report)?;
    }

    let mut report = String::new();
    sanitize::open_file_validated(report_path)
        .with_context(|| format!("claude did not write report: {}", report_path.display()))?
        .read_to_string(&mut report)?;

    if report.trim().is_empty() {
        anyhow::bail!("claude report empty");
    }

    Ok(report)
}

fn run_codex(
    _model: Option<&str>,
    prompt_path: &Path,
    report_path: &Path,
    root: &Path,
) -> Result<String> {
    let validated_prompt = sanitize::validate_read_path(prompt_path)?;
    let bootstrap_path = write_bootstrap_prompt(&validated_prompt)?;

    if should_dispatch_terminal() {
        let validated_report = sanitize::validate_write_path(report_path)?;
        let cmd = format!(
            "codex --dangerously-bypass-approvals-and-sandbox exec -C {} --output-last-message {} < {}",
            shell_escape(root),
            shell_escape(&validated_report),
            shell_escape(&bootstrap_path)
        );
        dispatch_terminal(&cmd)?;
        wait_for_report(&validated_report, std::time::Duration::from_secs(60 * 60))?;
    } else {
        let prompt_file = sanitize::open_file_validated(&bootstrap_path)?;
        let mut cmd = Command::new("codex");
        cmd.arg("--dangerously-bypass-approvals-and-sandbox")
            .arg("exec")
            .arg("-C")
            .arg(root)
            .arg("--output-last-message")
            .arg(report_path);

        let status = cmd
            .stdin(Stdio::from(prompt_file))
            .status()
            .context("Failed to run codex")?;

        if !status.success() {
            anyhow::bail!("codex command failed");
        }
    }

    let mut report = String::new();
    sanitize::open_file_validated(report_path)
        .with_context(|| format!("codex did not write report: {}", report_path.display()))?
        .read_to_string(&mut report)?;

    if report.trim().is_empty() {
        anyhow::bail!("codex report empty");
    }

    Ok(report)
}

fn should_dispatch_terminal() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    match std::env::var("AI_CTX_DISPATCH_TERMINAL") {
        Ok(val) => {
            let v = val.trim().to_lowercase();
            !(v.is_empty() || v == "0" || v == "false" || v == "no")
        }
        Err(_) => true,
    }
}

fn dispatch_terminal(command: &str) -> Result<()> {
    let escaped = escape_osascript(command);
    let status = Command::new("osascript")
        .args([
            "-e",
            "tell application \"Terminal\" to activate",
            "-e",
            &format!("tell application \"Terminal\" to do script \"{}\"", escaped),
        ])
        .status()
        .context("Failed to dispatch command via osascript")?;
    if !status.success() {
        anyhow::bail!("osascript failed to dispatch Terminal command");
    }
    Ok(())
}

fn shell_escape(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn shell_escape_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn bootstrap_prompt(prompt_path: &Path) -> String {
    format!(
        "First, read the full prompt from this file: {}. Use tools to open it. Do not proceed until you have read it. Then follow its instructions exactly.",
        prompt_path.display()
    )
}

fn write_bootstrap_prompt(prompt_path: &Path) -> Result<PathBuf> {
    let bootstrap_path = prompt_path.with_extension("bootstrap.txt");
    let validated = sanitize::validate_write_path(&bootstrap_path)?;
    fs::write(&validated, bootstrap_prompt(prompt_path))?;
    Ok(validated)
}

fn escape_osascript(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    for ch in command.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
}

fn wait_for_report(path: &Path, timeout: std::time::Duration) -> Result<()> {
    let start = std::time::Instant::now();
    loop {
        if let Ok(meta) = fs::metadata(path)
            && meta.len() > 0
        {
            return Ok(());
        }
        if start.elapsed() > timeout {
            anyhow::bail!(
                "Timed out waiting for report to be written: {}",
                path.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

fn split_summary_block(report: &str) -> (String, Option<String>) {
    if let Some(start_idx) = report.find(SUMMARY_START)
        && let Some(end_idx) = report[start_idx..].find(SUMMARY_END)
    {
        let end_idx = start_idx + end_idx;
        let block = report[start_idx + SUMMARY_START.len()..end_idx]
            .trim()
            .to_string();
        let mut clean = String::new();
        clean.push_str(report[..start_idx].trim_end());
        clean.push('\n');
        clean.push_str(report[end_idx + SUMMARY_END.len()..].trim_start());
        return (clean.trim().to_string(), Some(block));
    }
    (report.trim().to_string(), None)
}

fn append_summary(path: &Path, block: &str) -> Result<()> {
    let validated = sanitize::validate_write_path(path)?;
    if !validated.exists() {
        fs::write(&validated, "# AI Context Summary\n\n")?;
    }

    let mut content = sanitize::read_to_string_validated(&validated)?;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(block.trim());
    content.push('\n');
    content.push('\n');
    fs::write(&validated, content)?;
    Ok(())
}

fn trim_summary(path: &Path, max_lines: usize) -> Result<()> {
    let content = sanitize::read_to_string_validated(path)?;
    let parts: Vec<&str> = content.split("\n## ").collect();
    if parts.len() <= 1 {
        return Ok(());
    }

    let header = parts[0].trim_end();
    let mut sections: Vec<String> = parts[1..]
        .iter()
        .map(|s| format!("## {}", s.trim_end()))
        .collect();

    let header_lines = header.lines().count();
    let mut total_lines: usize =
        header_lines + sections.iter().map(|s| s.lines().count()).sum::<usize>();

    while total_lines > max_lines && !sections.is_empty() {
        let removed = sections.remove(0);
        total_lines = total_lines.saturating_sub(removed.lines().count());
    }

    let mut rebuilt = String::new();
    rebuilt.push_str(header);
    rebuilt.push('\n');
    rebuilt.push('\n');
    for section in &sections {
        rebuilt.push_str(section);
        rebuilt.push('\n');
        rebuilt.push('\n');
    }

    fs::write(path, rebuilt.trim_end().to_string() + "\n")?;
    Ok(())
}

fn append_timeline(
    path: &Path,
    ts: &str,
    agent: &str,
    model: Option<&str>,
    run_id: &str,
    report: &str,
) -> Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;

    if fs::metadata(path)?.len() == 0 {
        writeln!(file, "# AI Context Timeline")?;
        writeln!(file)?;
    }

    let model_label = model.unwrap_or("default");
    writeln!(file, "## {} | {} | {} | {}", ts, agent, model_label, run_id)?;
    writeln!(file)?;
    writeln!(file, "{}", report.trim())?;
    writeln!(file)?;
    writeln!(file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{FileTime, set_file_mtime};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_ROOT_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new() -> Self {
            let mut dir = std::env::temp_dir();
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let seq = TEMP_ROOT_COUNTER.fetch_add(1, Ordering::Relaxed);
            dir.push(format!(
                "ai-ctx-init-test-{}-{}-{}",
                std::process::id(),
                nanos,
                seq
            ));
            fs::create_dir_all(&dir).unwrap();
            Self { path: dir }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn setup_dirs() -> (TempRoot, PathBuf, PathBuf, PathBuf) {
        let root = TempRoot::new();
        let context_dir = root
            .path()
            .join(".ai-context")
            .join("local")
            .join("context");
        let share_dir = root.path().join(".ai-context").join("share");
        let prompts_dir = root
            .path()
            .join(".ai-context")
            .join("local")
            .join("prompts");
        fs::create_dir_all(&context_dir).unwrap();
        fs::create_dir_all(&share_dir).unwrap();
        fs::create_dir_all(&prompts_dir).unwrap();
        let prompt_path = prompts_dir.join("prompt.md");
        (root, context_dir, share_dir, prompt_path)
    }

    fn build_and_read_prompt(
        prompt_path: &Path,
        context_dir: &Path,
        action: Option<&str>,
        agent_prompt: Option<&str>,
    ) -> String {
        build_prompt(
            prompt_path,
            context_dir,
            "2026-02-03 12:00",
            action,
            agent_prompt,
        )
        .unwrap();
        fs::read_to_string(prompt_path).unwrap()
    }

    #[test]
    fn test_build_prompt_includes_core_rules() {
        let (_root, context_dir, _share_dir, prompt_path) = setup_dirs();
        let prompt = build_and_read_prompt(&prompt_path, &context_dir, None, None);

        assert!(prompt.contains("Exploration schedule (required):"));
        assert!(prompt.contains("Quality gate:"));
        assert!(prompt.contains("cargo clippy -- -D warnings"));
        assert!(prompt.contains("semgrep scan --config auto"));
        assert!(prompt.contains("TIMELINE.md:"));
        assert!(prompt.contains("TRIAGE.md:"));
        assert!(prompt.contains("PROMPTS/:"));
        assert!(!prompt.contains("CONTEXT (truncated):"));
    }

    #[test]
    fn test_build_prompt_includes_action() {
        let (_root, context_dir, _share_dir, prompt_path) = setup_dirs();
        let prompt = build_and_read_prompt(
            &prompt_path,
            &context_dir,
            Some("Zaimplementuj tę flagę ziom teraz"),
            None,
        );

        assert!(prompt.contains("Action focus (must address):"));
        assert!(prompt.contains("Zaimplementuj tę flagę ziom teraz"));
    }

    #[test]
    fn test_build_prompt_includes_agent_prompt() {
        let (_root, context_dir, _share_dir, prompt_path) = setup_dirs();
        let prompt = build_and_read_prompt(
            &prompt_path,
            &context_dir,
            None,
            Some("Extra rule 1\nExtra rule 2"),
        );

        assert!(prompt.contains("Additional agent prompt:"));
        assert!(prompt.contains("Extra rule 1"));
        assert!(prompt.contains("Extra rule 2"));
    }

    #[test]
    fn test_build_prompt_lists_artifacts_newest_first() {
        let (_root, context_dir, share_dir, prompt_path) = setup_dirs();

        let old = context_dir.join("a_old.md");
        let new = context_dir.join("b_new.md");
        let mid = share_dir.join("c_mid.md");

        fs::write(&old, "old").unwrap();
        fs::write(&new, "new").unwrap();
        fs::write(&mid, "mid").unwrap();

        set_file_mtime(&old, FileTime::from_unix_time(1_600_000_000, 0)).unwrap();
        set_file_mtime(&mid, FileTime::from_unix_time(1_600_000_100, 0)).unwrap();
        set_file_mtime(&new, FileTime::from_unix_time(1_600_000_200, 0)).unwrap();

        let prompt = build_and_read_prompt(&prompt_path, &context_dir, None, None);

        let mut in_section = false;
        let mut listed = Vec::new();
        for line in prompt.lines() {
            if line.starts_with("ARTIFACTS (paths only, newest first):") {
                in_section = true;
                continue;
            }
            if in_section && line.starts_with("Notes:") {
                break;
            }
            if in_section
                && let Some(rest) = line.strip_prefix("- ")
                && let Some((path, _)) = rest.split_once(" (mtime:")
            {
                listed.push(path.to_string());
            }
        }

        assert!(listed.len() >= 3);
        assert!(listed[0].ends_with("b_new.md"));
        assert!(listed[1].ends_with("c_mid.md"));
        assert!(listed[2].ends_with("a_old.md"));
    }

    #[test]
    fn test_build_prompt_excludes_loct_for_ai() {
        let (_root, context_dir, _share_dir, prompt_path) = setup_dirs();
        let loct = context_dir.join("x_loct_for_ai.md");
        fs::write(&loct, "should not appear").unwrap();
        set_file_mtime(&loct, FileTime::from_unix_time(1_700_000_000, 0)).unwrap();

        let prompt = build_and_read_prompt(&prompt_path, &context_dir, None, None);
        assert!(!prompt.contains("x_loct_for_ai.md"));
    }

    #[test]
    fn test_build_prompt_includes_share_artifacts() {
        let (_root, context_dir, share_dir, prompt_path) = setup_dirs();
        let shared = share_dir.join("shared.md");
        fs::write(&shared, "shared").unwrap();
        set_file_mtime(&shared, FileTime::from_unix_time(1_700_000_000, 0)).unwrap();

        let prompt = build_and_read_prompt(&prompt_path, &context_dir, None, None);
        assert!(prompt.contains("shared.md"));
    }

    #[test]
    fn test_build_prompt_no_content_injection() {
        let (_root, context_dir, _share_dir, prompt_path) = setup_dirs();
        let artifact = context_dir.join("artifact.md");
        fs::write(&artifact, "SENSITIVE_TEST_TOKEN").unwrap();

        let prompt = build_and_read_prompt(&prompt_path, &context_dir, None, None);
        assert!(!prompt.contains("SENSITIVE_TEST_TOKEN"));
    }

    #[test]
    fn test_build_prompt_empty_artifacts() {
        let (_root, context_dir, _share_dir, prompt_path) = setup_dirs();
        let prompt = build_and_read_prompt(&prompt_path, &context_dir, None, None);
        assert!(prompt.contains("(no artifacts found)"));
    }
}
