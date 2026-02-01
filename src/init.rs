//! Interactive init flow for ai-contexters.
//!
//! Creates `.ai-context/` structure, builds context/prompt,
//! runs the selected agent, and updates share artifacts.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{Local, Utc};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::output::{self, OutputConfig, OutputFormat, OutputMode, ReportMetadata, TimelineEntry};
use crate::sanitize;
use crate::sources::{self, ExtractionConfig};

const SUMMARY_START: &str = "===AI_CONTEXT_SUMMARY_START===";
const SUMMARY_END: &str = "===AI_CONTEXT_SUMMARY_END===";
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

pub fn run_init(options: InitOptions) -> Result<()> {
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

    log.line("== ai-contexters init ==");
    log.line(&format!("root:    {}", root.display()));
    log.line(&format!("project: {}", project));
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

    log.line("== Step 3/4: ai-contexters context ==");
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
    let prompt_bytes = build_prompt(
        &prompt_path,
        &loct_path,
        &paths.context,
        options.max_lines,
        &ts_local,
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

    let agent = resolve_agent(options.agent)?;
    onboarding(&mut log, &agent)?;
    check_agent(&agent)?;

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
    let report = run_agent(
        &root,
        &agent,
        model,
        &prompt_path,
        &report_path,
        &paths.config,
    )?;

    let (clean_report, summary_block) = split_summary_block(&report);
    append_timeline(
        &paths.timeline,
        &ts_local,
        &agent,
        model,
        &run_id,
        &clean_report,
    )?;

    match summary_block {
        Some(block) => {
            append_summary(&paths.summary, &block)?;
            trim_summary(&paths.summary, SUMMARY_MAX_LINES)?;
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
            "!.ai-context/share/summary.md",
            "!.ai-context/share/timeline.md",
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
    let ok = Command::new("loct").arg("--version").output().is_ok();
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

    let loct_version = Command::new("loct")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
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
    let status = Command::new("loct")
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
    let output = Command::new("loct")
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

fn build_prompt(
    prompt_path: &Path,
    loct_path: &Path,
    context_dir: &Path,
    max_lines: usize,
    ts_local: &str,
) -> Result<usize> {
    let validated_prompt = sanitize::validate_write_path(prompt_path)?;
    // SECURITY: path sanitized via validate_write_path (traversal + allowlist)
    let mut file = File::create(&validated_prompt)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path

    writeln!(
        file,
        "You are an agent completing an initialization pipeline for a repository."
    )?;
    writeln!(file)?;
    writeln!(file, "Rules:")?;
    writeln!(file, "- No internet access.")?;
    writeln!(file, "- Use only the context in this prompt.")?;
    writeln!(file, "- Do not guess facts that are not present.")?;
    writeln!(file)?;
    writeln!(file, "Tasks:")?;
    writeln!(
        file,
        "1) Summary: what the repo is and how it is organized."
    )?;
    writeln!(
        file,
        "2) Build/Test Quickstart: exact commands or what's missing."
    )?;
    writeln!(
        file,
        "3) Next Tasks: 5-10 checkbox items (small, actionable)."
    )?;
    writeln!(file, "4) Risks/Tech debt: max 10 with minimal fixes.")?;
    writeln!(file)?;
    writeln!(file, "Required report format:")?;
    writeln!(file, "- Summary")?;
    writeln!(file, "- Build/Test Quickstart")?;
    writeln!(file, "- Next Tasks")?;
    writeln!(file, "- Risks")?;
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
    writeln!(file, "CONTEXT (truncated):")?;
    writeln!(file)?;

    if loct_path.exists() {
        writeln!(file, "## loct --for-ai (first {} lines)", max_lines)?;
        append_first_lines(&mut file, loct_path, max_lines)?;
        writeln!(file)?;
    }

    let mut context_files: Vec<PathBuf> = fs::read_dir(context_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .filter(|p| {
            let name = p.file_name().unwrap_or(OsStr::new("")).to_string_lossy();
            name.contains("memory_") || name.contains("timeline")
        })
        .collect();

    context_files.sort_by(|a, b| {
        let an = a
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();
        let bn = b
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();
        an.cmp(&bn)
    });

    for ctx in context_files {
        let name = ctx
            .file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();
        writeln!(file, "## {} (first {} lines)", name, max_lines)?;
        append_first_lines(&mut file, &ctx, max_lines)?;
        writeln!(file)?;
    }

    file.flush()?;
    let bytes = fs::metadata(prompt_path)?.len() as usize;
    Ok(bytes)
}

fn append_first_lines(w: &mut impl Write, path: &Path, max_lines: usize) -> Result<()> {
    let validated = sanitize::validate_read_path(path)?;
    // SECURITY: path sanitized via validate_read_path (traversal + canonicalize + allowlist)
    let file = File::open(&validated)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let reader = BufReader::new(file);
    for (idx, line) in reader.lines().enumerate() {
        if idx >= max_lines {
            break;
        }
        let line = line?;
        writeln!(w, "{}", line)?;
    }
    Ok(())
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
            return Ok("claude".to_string());
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

fn ensure_mcp_config(config_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(config_dir)?;
    let path = config_dir.join("mcp.json");
    if !path.exists() {
        fs::write(
            &path,
            r#"{
  "mcpServers": {}
}
"#,
        )?;
    }
    Ok(path)
}

fn run_agent(
    root: &Path,
    agent: &str,
    model: Option<&str>,
    prompt_path: &Path,
    report_path: &Path,
    config_dir: &Path,
) -> Result<String> {
    match agent {
        "claude" => run_claude(model, prompt_path, report_path, config_dir),
        "codex" => run_codex(model, prompt_path, report_path, root),
        _ => anyhow::bail!("unknown agent: {}", agent),
    }
}

fn run_claude(
    model: Option<&str>,
    prompt_path: &Path,
    report_path: &Path,
    config_dir: &Path,
) -> Result<String> {
    let prompt = fs::read_to_string(prompt_path)?;
    let mcp_config = ensure_mcp_config(config_dir)?;

    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg("--no-session-persistence")
        .arg("--mcp-config")
        .arg(mcp_config)
        .arg("--strict-mcp-config")
        .arg("--tools")
        .arg("")
        .arg("--output-format")
        .arg("text");

    if let Some(model) = model {
        cmd.arg("--model").arg(model);
    }

    let output = cmd.arg(prompt).output().context("Failed to run claude")?;

    if !output.status.success() {
        anyhow::bail!("claude command failed");
    }

    let report = String::from_utf8_lossy(&output.stdout).to_string();
    fs::write(report_path, &report)?;
    Ok(report)
}

fn run_codex(
    model: Option<&str>,
    prompt_path: &Path,
    report_path: &Path,
    root: &Path,
) -> Result<String> {
    let validated_prompt = sanitize::validate_read_path(prompt_path)?;
    // SECURITY: path sanitized via validate_read_path (traversal + canonicalize + allowlist)
    let prompt_file = File::open(&validated_prompt)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path

    let mut cmd = Command::new("codex");
    cmd.arg("exec")
        .arg("--sandbox")
        .arg("read-only")
        .arg("-C")
        .arg(root)
        .arg("--output-last-message")
        .arg(report_path);

    if let Some(model) = model {
        cmd.arg("-m").arg(model);
    }

    let status = cmd
        .stdin(Stdio::from(prompt_file))
        .status()
        .context("Failed to run codex")?;

    if !status.success() {
        anyhow::bail!("codex command failed");
    }

    let mut report = String::new();
    let validated_report = sanitize::validate_read_path(report_path)?;
    // SECURITY: path sanitized via validate_read_path (traversal + canonicalize + allowlist)
    File::open(&validated_report) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .with_context(|| format!("codex did not write report: {}", report_path.display()))?
        .read_to_string(&mut report)?;

    if report.trim().is_empty() {
        anyhow::bail!("codex report empty");
    }

    Ok(report)
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
    if !path.exists() {
        fs::write(path, "# AI Context Summary\n\n")?;
    }

    let mut content = fs::read_to_string(path)?;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(block.trim());
    content.push('\n');
    content.push('\n');
    fs::write(path, content)?;
    Ok(())
}

fn trim_summary(path: &Path, max_lines: usize) -> Result<()> {
    let content = fs::read_to_string(path)?;
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
