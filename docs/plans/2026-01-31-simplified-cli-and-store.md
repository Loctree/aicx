# Simplified CLI & Store Redesign

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `ai-contexters -H 48` the daily-use command that auto-detects project from cwd, extracts all agents to central store with new date-grouped layout, and prints a concise summary with retrieval instructions.

**Architecture:** Add top-level `-H` flag to `Cli` struct so bare invocation (no subcommand) triggers default mode. Reuse existing `run_store()` flow but with new store layout (`<project>/<date>/HHMMSS_<agent>-context.{md,json}`). Fix sanitize bug blocking `./` paths. All existing subcommands remain untouched.

**Tech Stack:** Rust, clap 4 (derive), chrono, dirs, serde_json

---

### Task 1: Fix sanitize `./` false positive

**Files:**
- Modify: `src/sanitize.rs:22-29`
- Modify: `src/sanitize.rs:207-216` (test)

**Step 1: Fix `contains_traversal` to not reject `./`**

The current check `path_lower.contains("./")` rejects all relative paths. Only `..` is a real traversal risk. `./` is harmless because `canonicalize()` and allowlist handle it downstream.

```rust
fn contains_traversal(path: &str) -> bool {
    let path_lower = path.to_lowercase();
    path_lower.contains("..")
        || path.contains('\0')
        || path.contains('\n')
        || path.contains('\r')
}
```

**Step 2: Update test**

```rust
#[test]
fn test_contains_traversal() {
    assert!(contains_traversal("../etc/passwd"));
    assert!(contains_traversal("foo/../bar"));
    assert!(contains_traversal("path\0with\0nulls"));
    assert!(contains_traversal("line\nbreak"));
    assert!(!contains_traversal("/normal/path"));
    assert!(!contains_traversal("simple_name"));
    assert!(!contains_traversal("./relative/path")); // NEW: ./ is safe
}
```

**Step 3: Run tests**

Run: `cargo test -p ai-contexters -- sanitize`
Expected: PASS

**Step 4: Commit**

```bash
git add src/sanitize.rs
git commit -m "fix: allow ./ in path sanitization (canonicalize handles it)"
```

---

### Task 2: Extract `detect_project()` into `sources.rs`

**Files:**
- Modify: `src/sources.rs` (add public function)
- Modify: `src/init.rs:78-83,208-223` (reuse new function)

**Step 1: Add `detect_project_name()` to `sources.rs`**

After the existing `list_available_sources()` function (around line 643), add:

```rust
/// Detect project name from current working directory.
///
/// Strategy: git repo root dirname → cwd dirname → "unknown".
pub fn detect_project_name() -> String {
    // Try git repo root
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Some(name) = std::path::Path::new(&s).file_name() {
                return name.to_string_lossy().to_string();
            }
        }
    }

    // Fallback: cwd dirname
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(name) = cwd.file_name() {
            return name.to_string_lossy().to_string();
        }
    }

    "unknown".to_string()
}
```

**Step 2: Refactor `init.rs` to use it**

In `init.rs:77-83`, replace:

```rust
pub fn run_init(options: InitOptions) -> Result<()> {
    let root = repo_root()?;
    let project = options.project.unwrap_or_else(|| {
        root.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string())
    });
```

With:

```rust
pub fn run_init(options: InitOptions) -> Result<()> {
    let root = repo_root()?;
    let project = options
        .project
        .unwrap_or_else(sources::detect_project_name);
```

**Step 3: Run tests**

Run: `cargo test -p ai-contexters`
Expected: PASS (no behavior change)

**Step 4: Commit**

```bash
git add src/sources.rs src/init.rs
git commit -m "refactor: extract detect_project_name() into sources.rs"
```

---

### Task 3: Redesign store layout to date-grouped

**Files:**
- Modify: `src/store.rs:50-55` (`get_context_path`)
- Modify: `src/store.rs:156-194` (`write_context`)
- Modify: `src/store.rs` (tests)

**Step 1: Write failing test for new path layout**

Add to `src/store.rs` tests:

```rust
#[test]
fn test_get_context_path_new_layout() {
    // New layout: <project>/<date>/HHMMSS_<agent>-context.md
    if let Ok(path) = get_context_path("CodeScribe", "claude", "2026-01-22", "143005") {
        let s = path.to_string_lossy();
        assert!(s.contains("CodeScribe"));
        assert!(s.contains("2026-01-22"));
        assert!(s.ends_with("143005_claude-context.md"));
    }
}
```

Run: `cargo test -p ai-contexters -- test_get_context_path_new_layout`
Expected: FAIL (function signature mismatch)

**Step 2: Update `get_context_path` signature and implementation**

```rust
/// Full path for a specific context file.
///
/// Layout: `~/.ai-contexters/<project>/<date>/<time>_<agent>-context.md`
pub fn get_context_path(project: &str, agent: &str, date: &str, time: &str) -> Result<PathBuf> {
    let base = store_base_dir()?;
    let dir = base.join(project).join(date);
    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}_{}-context.md", time, agent)))
}

/// JSON variant of context path.
pub fn get_context_json_path(project: &str, agent: &str, date: &str, time: &str) -> Result<PathBuf> {
    let base = store_base_dir()?;
    let dir = base.join(project).join(date);
    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}_{}-context.json", time, agent)))
}
```

Remove `contexts/` from the hierarchy — it's now just `~/.ai-contexters/<project>/<date>/`.

**Step 3: Update `contexts_dir()` → remove**

Replace `contexts_dir()` with a simple helper or inline it. The old `contexts/<project>/<agent>/<date>.md` layout is gone.

```rust
/// Returns the base store directory: `~/.ai-contexters/`
/// (unchanged)
pub fn store_base_dir() -> Result<PathBuf> { /* same */ }

/// Returns the project directory: `~/.ai-contexters/<project>/`
pub fn project_dir(project: &str) -> Result<PathBuf> {
    let dir = store_base_dir()?.join(project);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}
```

**Step 4: Update `write_context` to write both md and json**

```rust
/// Write timeline entries to the central store.
///
/// Creates two files:
/// - `~/.ai-contexters/<project>/<date>/<time>_<agent>-context.md`
/// - `~/.ai-contexters/<project>/<date>/<time>_<agent>-context.json`
///
/// Returns paths of both files.
pub fn write_context(
    project: &str,
    agent: &str,
    date: &str,
    time: &str,
    entries: &[TimelineEntry],
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();

    // Markdown
    let md_path = get_context_path(project, agent, date, time)?;
    let mut md_content = String::new();
    md_content.push_str(&format!("# {} | {} | {}\n\n", project, agent, date));

    for entry in entries {
        let ts = entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC");
        md_content.push_str(&format!("### {} | {}\n", ts, entry.role));
        for line in entry.message.lines() {
            md_content.push_str(&format!("> {}\n", line));
        }
        md_content.push('\n');
    }

    let write_path = sanitize::validate_write_path(&md_path)?;
    fs::write(&write_path, &md_content)?;
    written.push(md_path);

    // JSON
    let json_path = get_context_json_path(project, agent, date, time)?;
    let json_content = serde_json::to_string_pretty(entries)?;
    let write_path = sanitize::validate_write_path(&json_path)?;
    fs::write(&write_path, &json_content)?;
    written.push(json_path);

    Ok(written)
}
```

**Step 5: Update all callers of `write_context` in `main.rs`**

The old signature was `write_context(project, agent, date, entries) -> Result<PathBuf>`.
New signature: `write_context(project, agent, date, time, entries) -> Result<Vec<PathBuf>>`.

In `main.rs:629-632`, update the dual-write block:

```rust
let now = Utc::now();
let time_str = now.format("%H%M%S").to_string();
for ((agent_name, date), group_entries) in &groups {
    let paths = store::write_context(&project_name, agent_name, date, &time_str, group_entries)?;
    store::update_index(&mut index, &project_name, agent_name, date, group_entries.len());
    for path in &paths {
        eprintln!("  store → {}", path.display());
    }
}
```

Same update in `run_store()` at `main.rs:806-808`.

**Step 6: Update `run_refs` to walk new layout**

In `main.rs:849-903`, adjust directory walking:
- Old: `contexts/<project>/<agent>/<date>.md`
- New: `<project>/<date>/*-context.md`

```rust
fn run_refs(hours: u64, project: Option<String>) -> Result<()> {
    let base = store::store_base_dir()?;

    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(hours * 3600);

    let mut files: Vec<PathBuf> = Vec::new();

    let project_dirs: Vec<_> = if let Some(ref p) = project {
        let d = base.join(p);
        if d.is_dir() { vec![d] } else { vec![] }
    } else {
        std::fs::read_dir(&base)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "memex"))
            .collect()
    };

    for proj_dir in project_dirs {
        for date_entry in std::fs::read_dir(&proj_dir)?.filter_map(|e| e.ok()) {
            let date_path = date_entry.path();
            if !date_path.is_dir() {
                continue;
            }
            for file_entry in std::fs::read_dir(&date_path)?.filter_map(|e| e.ok()) {
                let fpath = file_entry.path();
                if fpath.extension().is_some_and(|ext| ext == "md" || ext == "json")
                    && let Ok(meta) = fpath.metadata()
                    && let Ok(mtime) = meta.modified()
                    && mtime >= cutoff
                {
                    files.push(fpath);
                }
            }
        }
    }

    files.sort();

    if files.is_empty() {
        eprintln!("No context files found within last {} hours.", hours);
    } else {
        for f in &files {
            println!("{}", f.display());
        }
        eprintln!("({} files)", files.len());
    }

    Ok(())
}
```

**Step 7: Fix all store tests**

Update existing tests in `src/store.rs` for new signatures. Remove old `test_get_context_path` and `test_write_context_creates_file`, replace with new-layout equivalents.

**Step 8: Run tests**

Run: `cargo test -p ai-contexters`
Expected: PASS

**Step 9: Commit**

```bash
git add src/store.rs src/main.rs
git commit -m "feat: redesign store layout to date-grouped (<project>/<date>/<time>_<agent>-context.{md,json})"
```

---

### Task 4: Add default mode (`ai-contexters -H 48`)

**Files:**
- Modify: `src/main.rs:26-33` (Cli struct)
- Modify: `src/main.rs:281-447` (main dispatch)

**Step 1: Change Cli struct to support bare flags**

Replace the current `Cli` struct with one that has optional subcommand + top-level `-H`:

```rust
/// AI Contexters - timeline and decisions from AI sessions
#[derive(Parser)]
#[command(name = "ai-contexters")]
#[command(author = "M&K (c)2026 VetCoders")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Hours to look back (default mode, no subcommand needed)
    #[arg(short = 'H', long, global = false)]
    hours: Option<u64>,

    /// Project name override (auto-detects from cwd if omitted)
    #[arg(short, long, global = false)]
    project: Option<String>,

    /// Include assistant messages
    #[arg(long, global = false)]
    include_assistant: bool,

    /// Also sync to memex
    #[arg(long, global = false)]
    memex: bool,
}
```

**Step 2: Add default dispatch in `main()`**

```rust
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ai_contexters=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(command) => dispatch_subcommand(command)?,
        None => {
            // Default mode: ai-contexters -H <hours>
            let hours = cli.hours.unwrap_or(48);
            run_default(cli.project, hours, cli.include_assistant, cli.memex)?;
        }
    }

    Ok(())
}
```

Move existing match arms into `fn dispatch_subcommand(command: Commands) -> Result<()>`.

**Step 3: Implement `run_default()`**

This is the core new function — extract all agents, write to store, print summary:

```rust
/// Default mode: extract all agents → store → print summary.
///
/// Equivalent to `ai-contexters store` but with auto-detected project
/// and a concise summary output.
fn run_default(
    project: Option<String>,
    hours: u64,
    include_assistant: bool,
    sync_memex: bool,
) -> Result<()> {
    let project_name = project.unwrap_or_else(sources::detect_project_name);
    let cutoff = Utc::now() - chrono::Duration::hours(hours as i64);
    let now = Utc::now();
    let date = now.format("%Y-%m-%d").to_string();
    let time = now.format("%H%M%S").to_string();

    let config = ExtractionConfig {
        project_filter: Some(project_name.clone()),
        cutoff,
        include_assistant,
        watermark: None,
    };

    // Extract from all agents
    let agents = ["claude", "codex", "gemini"];
    let mut all_entries = Vec::new();
    let mut agent_counts: Vec<(&str, usize)> = Vec::new();

    for &agent in &agents {
        let entries = match agent {
            "claude" => sources::extract_claude(&config)?,
            "codex" => sources::extract_codex(&config)?,
            "gemini" => sources::extract_gemini(&config)?,
            _ => Vec::new(),
        };
        agent_counts.push((agent, entries.len()));
        eprintln!("  [{}] {} entries", agent, entries.len());
        all_entries.extend(entries);
    }

    all_entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    if all_entries.is_empty() {
        eprintln!("No entries found for '{}' in last {} hours.", project_name, hours);
        return Ok(());
    }

    // Convert to output entries
    let output_entries: Vec<output::TimelineEntry> = all_entries
        .iter()
        .map(|e| output::TimelineEntry {
            timestamp: e.timestamp,
            agent: e.agent.clone(),
            session_id: e.session_id.clone(),
            role: e.role.clone(),
            message: e.message.clone(),
            branch: e.branch.clone(),
            cwd: e.cwd.clone(),
        })
        .collect();

    // Collect unique sessions
    let mut sessions: Vec<String> = all_entries.iter().map(|e| e.session_id.clone()).collect();
    sessions.sort();
    sessions.dedup();

    // Group by agent and write to store
    let mut groups: std::collections::BTreeMap<String, Vec<output::TimelineEntry>> =
        std::collections::BTreeMap::new();
    for entry in &output_entries {
        groups.entry(entry.agent.clone()).or_default().push(entry.clone());
    }

    let mut index = store::load_index();
    let mut stored_paths: Vec<PathBuf> = Vec::new();

    for (agent_name, group_entries) in &groups {
        let paths = store::write_context(&project_name, agent_name, &date, &time, group_entries)?;
        store::update_index(&mut index, &project_name, agent_name, &date, group_entries.len());
        stored_paths.extend(paths);
    }

    store::save_index(&index)?;

    // Memex sync if requested
    if sync_memex {
        let agent_label = agents.join("+");
        let chunker_config = chunker::ChunkerConfig::default();
        let chunks = chunker::chunk_entries(&output_entries, &project_name, &agent_label, &chunker_config);
        if !chunks.is_empty() {
            let chunks_dir = store::chunks_dir()?;
            chunker::write_chunks_to_dir(&chunks, &chunks_dir)?;
            let memex_config = memex::MemexConfig::default();
            match memex::sync_new_chunks(&chunks_dir, &memex_config) {
                Ok(r) => eprintln!("  memex: {} pushed, {} skipped", r.chunks_pushed, r.chunks_skipped),
                Err(e) => eprintln!("  memex sync failed: {}", e),
            }
        }
    }

    // === Print concise summary ===
    eprintln!();
    eprintln!("=== ai-contexters | {} | {} ===", project_name, date);
    eprintln!();
    for (agent, count) in &agent_counts {
        if *count > 0 {
            eprintln!("  {:>7}: {} entries", agent, count);
        }
    }
    eprintln!("  {:>7}: {} entries across {} sessions", "total", output_entries.len(), sessions.len());
    eprintln!();
    eprintln!("Stored to:");
    for p in &stored_paths {
        eprintln!("  {}", p.display());
    }
    eprintln!();
    eprintln!("Retrieve full context:");
    eprintln!("  cat ~/.ai-contexters/{}/{}/*-context.md", project_name, date);
    eprintln!("  # or JSON:");
    eprintln!("  cat ~/.ai-contexters/{}/{}/*-context.json", project_name, date);
    eprintln!();
    eprintln!("Browse all contexts:");
    eprintln!("  ai-contexters refs -H {}", hours);

    Ok(())
}
```

**Step 4: Run `cargo check`**

Run: `cargo check`
Expected: PASS (no compile errors)

**Step 5: Manual test**

Run: `cargo run -- -H 48`
Expected: Auto-detects project as "ai-contexters", extracts all agents, writes to `~/.ai-contexters/ai-contexters/2026-01-31/`, prints summary with retrieval instructions.

**Step 6: Commit**

```bash
git add src/main.rs src/sources.rs
git commit -m "feat: add default mode (ai-contexters -H 48) with auto-detect and summary"
```

---

### Task 5: Verify and bump version

**Files:**
- Modify: `Cargo.toml` (version bump)
- Modify: `CLAUDE.md` (update usage docs)

**Step 1: Bump version**

In `Cargo.toml`, change `version = "0.2.1"` → `version = "0.3.0"` (new store layout is breaking).

**Step 2: Update CLAUDE.md usage section**

Replace the CLI Usage section to show the new default:

```markdown
## CLI Usage

```bash
# Daily use - auto-detects project from cwd:
ai-contexters -H 48                          # Extract last 48h, all agents → central store

# With options:
ai-contexters -H 24 --memex                  # + sync to vector memory
ai-contexters -H 168 -p vista-website        # Override project name

# Subcommands (advanced):
ai-contexters init                           # Interactive init
ai-contexters list                           # List available sessions
ai-contexters refs -H 48                     # List stored context files
ai-contexters claude -p <project> -H 48      # Extract Claude only (legacy output)
ai-contexters store -H 48                    # Store-only mode
```
```

**Step 3: Run full test suite**

Run: `cargo test -p ai-contexters`
Expected: ALL PASS

**Step 4: Build release**

Run: `cargo build --release`
Expected: PASS

**Step 5: Install and smoke test**

Run: `cargo install --path . && ai-contexters -H 2`
Expected: Prints summary with retrieval instructions.

**Step 6: Commit**

```bash
git add Cargo.toml CLAUDE.md
git commit -m "chore: bump to v0.3.0, update docs for simplified CLI"
```

---

## Summary of changes

| File | Change |
|------|--------|
| `src/sanitize.rs` | Remove `./` from traversal check |
| `src/sources.rs` | Add `detect_project_name()` |
| `src/init.rs` | Reuse `detect_project_name()` |
| `src/store.rs` | New layout: `<project>/<date>/<time>_<agent>-context.{md,json}` |
| `src/main.rs` | Add bare `-H` default mode, `run_default()`, update callers |
| `Cargo.toml` | Version 0.3.0 |
| `CLAUDE.md` | Updated usage docs |

## Store layout migration

Old: `~/.ai-contexters/contexts/<project>/<agent>/<date>.md`
New: `~/.ai-contexters/<project>/<date>/<HHMMSS>_<agent>-context.{md,json}`

Old files remain untouched. No migration needed — new runs write to new layout.

---

*Created by M&K (c)2026 VetCoders*
