# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_aicx_global_optspecs
    string join \n v/verbose project-fuzzy h/help V/version
end

function __fish_aicx_needs_command
    # Figure out if the current invocation already has a command.
    set -l cmd (commandline -opc)
    set -e cmd[1]
    argparse -s (__fish_aicx_global_optspecs) -- $cmd 2>/dev/null
    or return
    if set -q argv[1]
        # Also print the command, so this can be used to figure out what it is.
        echo $argv[1]
        return 1
    end
    return 0
end

function __fish_aicx_using_subcommand
    set -l cmd (__fish_aicx_needs_command)
    test -z "$cmd"
    and return 1
    contains -- $cmd[1] $argv
end

complete -c aicx -n "__fish_aicx_needs_command" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_needs_command" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_needs_command" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_needs_command" -s V -l version -d 'Print version'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "completions" -d 'Generate shell completions for the canonical CLI grammar'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "overlay" -d 'Join typed canonical intents to the current Loctree anchor catalog'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "claude" -d 'Extract Claude sessions into local reports'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "codex" -d 'Extract Codex sessions into local reports'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "all" -d 'Extract sessions from all supported agents into local reports'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "extract" -d 'Extract a single session for one agent — by session id or direct file'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "conversations" -d 'Batch-export conversation JSON files without writing to the canonical store'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "catalog" -d 'Rebuild the durable extract-era session catalog (no per-frame cards)'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "ingest" -d 'Ingest operator-owned source documents into the canonical corpus'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "list" -d 'List raw agent session sources on disk (pre-extraction inputs)'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "sources" -d 'Audit and explicitly protect raw source roots'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "sessions" -d 'Discover and list agent sessions on disk (session surface)'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "claims" -d 'Lane 2: extract agent claims (audit targets) from a session'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "results" -d 'Lane 3: collect repo evidence for a session\'s claims and verify them'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "clarify" -d 'Lane 5: generate at most 5 A/B/C decision questions from verified gaps'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "wizard" -d 'Interactive daily-driver entrypoint for corpus, doctor, intents, and store'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "refs" -d 'List chunks in the canonical store inventory'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "state" -d 'Manage extraction dedup state (watermarks and hashes)'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "dashboard" -d 'Generate a searchable HTML dashboard from the canonical store, or serve it locally'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "reports" -d 'Extract Vibecrafted workflow and marbles reports into a standalone HTML explorer'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "corpus" -d 'Audit or repair derived corpus markdown'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "reports-extractor" -d 'Deprecated compatibility shim for `aicx reports`'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "dashboard-serve" -d 'Deprecated compatibility shim for `aicx dashboard --serve`'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "intents" -d 'Extract structured intents from the canonical corpus'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "tail" -d 'Print recent intents/chunks (snapshot mode); add --follow to stream new arrivals'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "serve" -d 'Run aicx as an MCP server'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "init" -d 'Retired compatibility shim; prints migration guidance'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "search" -d 'Search the CURRENT source/extract index. Lexical-first by default; optional dense rerank with --deep. When no index exists, the only fallback is a bounded recency-ranked filesystem search'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "eval" -d 'Run local evaluation helpers for retrieval/search quality'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "index" -d 'Build the source-driven lexical index. Use `--dry-run` to preview parsing and filtering without writing extracts or publishing CURRENT'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "config" -d 'Manage `$HOME/.aicx/config.toml` for embedders and endpoints'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "read" -d 'Read one canonical chunk by path, file name, or `chunk:<id>` reference'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "open" -d 'Read one canonical chunk by path, file name, or `chunk:<id>` reference'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "steer" -d 'Retrieve chunks by steering metadata (requires --features lance)'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "migrate" -d 'Migrate legacy ~/.ai-contexters/ data into the canonical AICX store'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "migrate-intent-schema" -d 'Classify stored chunks into 11-type intent entries and report counts'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "doctor" -d 'Diagnose and optionally repair the canonical store and steer index'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "health" -d 'Emit the bounded AICX health report as JSON for automation'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "warmup" -d 'Warm/probe the configured local embedder before interactive search'
complete -c aicx -n "__fish_aicx_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand completions" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand completions" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand completions" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand overlay" -l repo -d 'Repository whose `loct anchors` catalog is the attribution target' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand overlay" -l format -d 'Machine-readable overlay contract format' -r -f -a "json\t''"
complete -c aicx -n "__fish_aicx_using_subcommand overlay" -l rebuild -d 'Re-evaluate every typed card while preserving persisted intent ids'
complete -c aicx -n "__fish_aicx_using_subcommand overlay" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand overlay" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand overlay" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -s p -l project -d 'Source cwd/project filter(s): narrows session discovery before repo segmentation' -r
complete -c aicx -n "__fish_aicx_using_subcommand claude" -s H -l hours -d 'Hours to look back (default: 48, 0 = all time)' -r
complete -c aicx -n "__fish_aicx_using_subcommand claude" -s o -l output -d 'Output directory (omit to only write to store)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand claude" -s f -l format -d 'Output format: md, json, both' -r
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l append-to -d 'Append to a single timeline file instead of creating new files' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l rotate -d 'Keep only last N output files (0 = unlimited)' -r
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l project-root -d 'Project root for loctree snapshot (defaults to cwd)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l emit -d 'What to print to stdout: paths, json, none (default: none)' -r -f -a "paths\t'Print store chunk paths (one per line)'
json\t'Print JSON report (includes `store_paths` for convenience)'
none\t'Print nothing to stdout'"
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l full-rescan -d 'Ignore the stored watermark and previously-seen hashes for this run'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l incremental -d 'Legacy no-op: incremental mode is now the default'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l user-only -d 'Only include user messages (exclude assistant + reasoning)'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l include-assistant -d 'Include assistant messages (legacy flag; now default)'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l loctree -d 'Include loctree snapshot in output'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l force -d 'Force full extraction, ignore dedup hashes'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l conversation -d 'Conversation-first mode: emit denoised user/assistant transcript only'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand claude" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -s p -l project -d 'Source cwd/project filter(s): narrows session discovery before repo segmentation' -r
complete -c aicx -n "__fish_aicx_using_subcommand codex" -s H -l hours -d 'Hours to look back (default: 48, 0 = all time)' -r
complete -c aicx -n "__fish_aicx_using_subcommand codex" -s o -l output -d 'Output directory (omit to only write to store)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand codex" -s f -l format -d 'Output format: md, json, both' -r
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l append-to -d 'Append to a single timeline file' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l rotate -d 'Keep only last N output files (0 = unlimited)' -r
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l project-root -d 'Project root for loctree snapshot' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l emit -d 'What to print to stdout: paths, json, none (default: none)' -r -f -a "paths\t'Print store chunk paths (one per line)'
json\t'Print JSON report (includes `store_paths` for convenience)'
none\t'Print nothing to stdout'"
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l full-rescan -d 'Ignore the stored watermark and previously-seen hashes for this run'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l incremental -d 'Legacy no-op: incremental mode is now the default'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l user-only -d 'Only include user messages (exclude assistant + reasoning)'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l include-assistant -d 'Include assistant messages (legacy flag; now default)'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l loctree -d 'Include loctree snapshot'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l force -d 'Force full extraction, ignore dedup hashes'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l conversation -d 'Conversation-first mode: emit denoised user/assistant transcript only'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand codex" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand all" -s p -l project -d 'Source cwd/project filter(s): narrows session discovery before repo segmentation' -r
complete -c aicx -n "__fish_aicx_using_subcommand all" -s H -l hours -d 'Hours to look back (default: 48, 0 = all time)' -r
complete -c aicx -n "__fish_aicx_using_subcommand all" -s o -l output -d 'Output directory (omit to only write to store)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand all" -l append-to -d 'Append to a single timeline file' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand all" -l rotate -d 'Keep only last N output files (0 = unlimited)' -r
complete -c aicx -n "__fish_aicx_using_subcommand all" -l project-root -d 'Project root for loctree snapshot' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand all" -l emit -d 'What to print to stdout: paths, json, none (default: none)' -r -f -a "paths\t'Print store chunk paths (one per line)'
json\t'Print JSON report (includes `store_paths` for convenience)'
none\t'Print nothing to stdout'"
complete -c aicx -n "__fish_aicx_using_subcommand all" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand all" -l full-rescan -d 'Ignore the stored watermark and previously-seen hashes for this run'
complete -c aicx -n "__fish_aicx_using_subcommand all" -l incremental -d 'Legacy no-op: incremental mode is now the default'
complete -c aicx -n "__fish_aicx_using_subcommand all" -l user-only -d 'Only include user messages (exclude assistant + reasoning)'
complete -c aicx -n "__fish_aicx_using_subcommand all" -l include-assistant -d 'Include assistant messages (legacy flag; now default)'
complete -c aicx -n "__fish_aicx_using_subcommand all" -l loctree -d 'Include loctree snapshot'
complete -c aicx -n "__fish_aicx_using_subcommand all" -l force -d 'Force full extraction, ignore dedup hashes'
complete -c aicx -n "__fish_aicx_using_subcommand all" -l conversation -d 'Conversation-first mode: emit denoised user/assistant transcript only'
complete -c aicx -n "__fish_aicx_using_subcommand all" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand all" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand all" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -l agent -d 'Removed flag grammar (pre-C7). Present only to emit a structured migration hint instead of a bare clap error' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -l format -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -l session -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -s o -l output -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -s p -l project -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -s H -l hours -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -l max-message-chars -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -l conversation
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -l user-only
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -l include-assistant
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -a "codex" -d 'OpenAI Codex CLI rollouts (~/.codex/sessions)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -a "claude" -d 'Claude Code sessions (~/.claude/projects)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -a "gemini" -d 'Gemini CLI chats (~/.gemini/tmp/<hash>/chats)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -a "grok" -d 'Grok CLI sessions (~/.grok)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -a "junie" -d 'JetBrains Junie event logs (~/.junie/sessions)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from codex claude gemini grok junie help" -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -l session -d 'Session id: source id, logical id, alias, UUID suffix (≥8 chars), or unique prefix. Resolved through the session catalog before any parse' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -l file -d 'Direct source file. Builds a source handle from this path only — no catalog scan, no global AICX state. Requires `-o/--output`' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -s o -l output -d 'Output file path. Required with `--file`; defaults to `~/.aicx/extracts/<agent>/<session_id>[_conversation][_user].md` in session mode' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -s p -l project -d 'Explicit project/repo name (overrides inference)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -l max-message-chars -d 'Maximum message characters in markdown (0 = no truncation)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -l user-only -d 'Only include user messages (exclude assistant + reasoning)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -l conversation -d 'Conversation-first mode: emit denoised user/assistant transcript only'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from codex" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -l session -d 'Session id: source id, logical id, alias, UUID suffix (≥8 chars), or unique prefix. Resolved through the session catalog before any parse' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -l file -d 'Direct source file. Builds a source handle from this path only — no catalog scan, no global AICX state. Requires `-o/--output`' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -s o -l output -d 'Output file path. Required with `--file`; defaults to `~/.aicx/extracts/<agent>/<session_id>[_conversation][_user].md` in session mode' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -s p -l project -d 'Explicit project/repo name (overrides inference)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -l max-message-chars -d 'Maximum message characters in markdown (0 = no truncation)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -l user-only -d 'Only include user messages (exclude assistant + reasoning)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -l conversation -d 'Conversation-first mode: emit denoised user/assistant transcript only'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from claude" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -l session -d 'Session id: source id, logical id, alias, UUID suffix (≥8 chars), or unique prefix. Resolved through the session catalog before any parse' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -l file -d 'Direct source file. Builds a source handle from this path only — no catalog scan, no global AICX state. Requires `-o/--output`' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -s o -l output -d 'Output file path. Required with `--file`; defaults to `~/.aicx/extracts/<agent>/<session_id>[_conversation][_user].md` in session mode' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -s p -l project -d 'Explicit project/repo name (overrides inference)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -l max-message-chars -d 'Maximum message characters in markdown (0 = no truncation)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -l user-only -d 'Only include user messages (exclude assistant + reasoning)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -l conversation -d 'Conversation-first mode: emit denoised user/assistant transcript only'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from gemini" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -l session -d 'Session id: source id, logical id, alias, UUID suffix (≥8 chars), or unique prefix. Resolved through the session catalog before any parse' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -l file -d 'Direct source file. Builds a source handle from this path only — no catalog scan, no global AICX state. Requires `-o/--output`' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -s o -l output -d 'Output file path. Required with `--file`; defaults to `~/.aicx/extracts/<agent>/<session_id>[_conversation][_user].md` in session mode' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -s p -l project -d 'Explicit project/repo name (overrides inference)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -l max-message-chars -d 'Maximum message characters in markdown (0 = no truncation)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -l user-only -d 'Only include user messages (exclude assistant + reasoning)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -l conversation -d 'Conversation-first mode: emit denoised user/assistant transcript only'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from grok" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -l session -d 'Session id: source id, logical id, alias, UUID suffix (≥8 chars), or unique prefix. Resolved through the session catalog before any parse' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -l file -d 'Direct source file. Builds a source handle from this path only — no catalog scan, no global AICX state. Requires `-o/--output`' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -s o -l output -d 'Output file path. Required with `--file`; defaults to `~/.aicx/extracts/<agent>/<session_id>[_conversation][_user].md` in session mode' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -s p -l project -d 'Explicit project/repo name (overrides inference)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -l max-message-chars -d 'Maximum message characters in markdown (0 = no truncation)' -r
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -l user-only -d 'Only include user messages (exclude assistant + reasoning)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -l conversation -d 'Conversation-first mode: emit denoised user/assistant transcript only'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from junie" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from help" -f -a "codex" -d 'OpenAI Codex CLI rollouts (~/.codex/sessions)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from help" -f -a "claude" -d 'Claude Code sessions (~/.claude/projects)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from help" -f -a "gemini" -d 'Gemini CLI chats (~/.gemini/tmp/<hash>/chats)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from help" -f -a "grok" -d 'Grok CLI sessions (~/.grok)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from help" -f -a "junie" -d 'JetBrains Junie event logs (~/.junie/sessions)'
complete -c aicx -n "__fish_aicx_using_subcommand extract; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -l agent -d 'Source agent for batch conversation export (v1: claude only)' -r -f -a "claude\t''"
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -s p -l project -d 'Source cwd/project filter(s): narrows session discovery before export' -r
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -s H -l hours -d 'Hours to look back when scanning source sessions (default: 1 year)' -r
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -l out-dir -d 'Output directory. Files are written as `<out-dir>/<agent>/<sanitized-session-id>.json`. Session ids that contain characters other than `[A-Za-z0-9._-]` are sanitized; a SipHash suffix is appended to keep distinct ids from colliding after sanitization' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -l limit -d 'Maximum number of sessions to write, after deterministic session sorting' -r
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -l dry-run -d 'Preview discovery without writing; emits a JSON envelope on stdout (sessions_discovered, by_kind, by_agent, filters_applied) and a human-readable summary banner on stderr'
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand conversations" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and not __fish_seen_subcommand_from rebuild resolve help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and not __fish_seen_subcommand_from rebuild resolve help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and not __fish_seen_subcommand_from rebuild resolve help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and not __fish_seen_subcommand_from rebuild resolve help" -f -a "rebuild" -d 'Walk all source roots and rewrite `~/.aicx/catalog/sessions.jsonl`'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and not __fish_seen_subcommand_from rebuild resolve help" -f -a "resolve" -d 'Resolve one session id from the durable catalog'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and not __fish_seen_subcommand_from rebuild resolve help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from rebuild" -l json -d 'Emit JSON report to stdout'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from rebuild" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from rebuild" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from rebuild" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from resolve" -l json -d 'Emit JSON'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from resolve" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from resolve" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from resolve" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from help" -f -a "rebuild" -d 'Walk all source roots and rewrite `~/.aicx/catalog/sessions.jsonl`'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from help" -f -a "resolve" -d 'Resolve one session id from the durable catalog'
complete -c aicx -n "__fish_aicx_using_subcommand catalog; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -l source -d 'Source adapter to ingest' -r -f -a "operator-md\t''
loct-context-pack\t''"
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -s p -l project -d 'Source cwd/project filter(s): narrows source discovery before repo segmentation' -r
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -s H -l hours -d 'Hours to look back when --since is omitted (default: 720 = 30 days, 0 = all time)' -r
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -l since -d 'Lower date bound (YYYY-MM-DD or YYYY_MMDD)' -r
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -l emit -d 'What to print to stdout: paths, json, none (default: none)' -r -f -a "paths\t'Print store chunk paths (one per line)'
json\t'Print JSON report (includes `store_paths` for convenience)'
none\t'Print nothing to stdout'"
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -l no-redact-secrets -d 'Redact secrets (tokens/keys) from outputs before writing/syncing'
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -l full-rescan -d 'Ignore the stored watermark and previously-seen hashes for this run'
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -l no-noise-filter -d 'Disable structural-noise filter'
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand ingest" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand list" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand list" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand list" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and not __fish_seen_subcommand_from protect help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and not __fish_seen_subcommand_from protect help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and not __fish_seen_subcommand_from protect help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and not __fish_seen_subcommand_from protect help" -f -a "protect" -d 'Opt in to local source-root protection'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and not __fish_seen_subcommand_from protect help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from protect" -l root -d 'Source root to protect. Must be an existing directory' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from protect" -l backend -d 'Protection backend to use' -r -f -a "git-local\t''"
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from protect" -l apply -d 'Apply the plan. Omit for a dry run'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from protect" -l initial-snapshot -d 'Create an initial local commit after git-local setup'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from protect" -l no-gitignore -d 'Do not add safe local .gitignore suggestions'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from protect" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from protect" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from protect" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from help" -f -a "protect" -d 'Opt in to local source-root protection'
complete -c aicx -n "__fish_aicx_using_subcommand sources; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and not __fish_seen_subcommand_from current list show report help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and not __fish_seen_subcommand_from current list show report help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and not __fish_seen_subcommand_from current list show report help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and not __fish_seen_subcommand_from current list show report help" -f -a "current" -d 'Print the current agent session id for commit trailers and handoffs'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and not __fish_seen_subcommand_from current list show report help" -f -a "list" -d 'List discovered agent sessions, newest first'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and not __fish_seen_subcommand_from current list show report help" -f -a "show" -d 'Show one session\'s metadata, located by id (or a unique prefix)'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and not __fish_seen_subcommand_from current list show report help" -f -a "report" -d 'Unified truth report for one session: human intents (Lane 1), agent claims + evidence verification (Lanes 2-3), contract fractures (Lane 4) and clarify decisions (Lane 5) in a single rendering'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and not __fish_seen_subcommand_from current list show report help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from current" -s j -l json -d 'Emit JSON with source metadata instead of the bare session id'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from current" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from current" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from current" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from list" -l agent -d 'Filter by agent (claude | codex | gemini | junie | grok)' -r -f -a "claude\t''
codex\t''
gemini\t''
junie\t''
grok\t''"
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from list" -l since -d 'Only sessions updated on/after this date (YYYY-MM-DD). Defaults to the last 30 days; pass --all to scan the full history' -r
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from list" -l limit -d 'Max sessions to show (0 = all)' -r
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from list" -l format -d 'Output format: table | json' -r
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from list" -l cwd -d 'Restrict to sessions whose repo/cwd matches the current directory'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from list" -l all -d 'Scan the full session history (slower) instead of the default last-30-days window'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from list" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from list" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from show" -l format -d 'Output format: markdown | json' -r
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from show" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from show" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from show" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from report" -l agent -d 'Agent: claude | codex | gemini | junie | grok. Inferred from the session id when omitted' -r
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from report" -l hours -d 'Hours to look back when locating the session (default 720)' -r
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from report" -l repo -d 'Repo root evidence is checked against (default: current directory)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from report" -l max -d 'Max clarify questions (hard-capped at 5)' -r
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from report" -l format -d 'Output format: markdown | json' -r
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from report" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from report" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from report" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from help" -f -a "current" -d 'Print the current agent session id for commit trailers and handoffs'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from help" -f -a "list" -d 'List discovered agent sessions, newest first'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from help" -f -a "show" -d 'Show one session\'s metadata, located by id (or a unique prefix)'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from help" -f -a "report" -d 'Unified truth report for one session: human intents (Lane 1), agent claims + evidence verification (Lanes 2-3), contract fractures (Lane 4) and clarify decisions (Lane 5) in a single rendering'
complete -c aicx -n "__fish_aicx_using_subcommand sessions; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and not __fish_seen_subcommand_from extract help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and not __fish_seen_subcommand_from extract help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and not __fish_seen_subcommand_from extract help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and not __fish_seen_subcommand_from extract help" -f -a "extract" -d 'Extract Unverified claims (Lane 2) from a session\'s conversation'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and not __fish_seen_subcommand_from extract help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and __fish_seen_subcommand_from extract" -l session -d 'Session id (or unique prefix)' -r
complete -c aicx -n "__fish_aicx_using_subcommand claims; and __fish_seen_subcommand_from extract" -l agent -d 'Agent: claude | codex | gemini | junie | grok. Inferred from the session id when omitted' -r
complete -c aicx -n "__fish_aicx_using_subcommand claims; and __fish_seen_subcommand_from extract" -l hours -d 'Hours to look back when locating the session (default 720)' -r
complete -c aicx -n "__fish_aicx_using_subcommand claims; and __fish_seen_subcommand_from extract" -l format -d 'Output format: json | summary' -r
complete -c aicx -n "__fish_aicx_using_subcommand claims; and __fish_seen_subcommand_from extract" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and __fish_seen_subcommand_from extract" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and __fish_seen_subcommand_from extract" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and __fish_seen_subcommand_from help" -f -a "extract" -d 'Extract Unverified claims (Lane 2) from a session\'s conversation'
complete -c aicx -n "__fish_aicx_using_subcommand claims; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand results; and not __fish_seen_subcommand_from collect help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand results; and not __fish_seen_subcommand_from collect help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand results; and not __fish_seen_subcommand_from collect help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand results; and not __fish_seen_subcommand_from collect help" -f -a "collect" -d 'Collect repo evidence (artifact existence) for a session\'s claims and fold it into verification statuses (Lane 3)'
complete -c aicx -n "__fish_aicx_using_subcommand results; and not __fish_seen_subcommand_from collect help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from collect" -l session -d 'Session id (or unique prefix)' -r
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from collect" -l agent -d 'Agent: claude | codex | gemini | junie | grok. Inferred from the session id when omitted' -r
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from collect" -l hours -d 'Hours to look back when locating the session (default 720)' -r
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from collect" -l repo -d 'Repo root evidence is checked against (default: current directory)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from collect" -l format -d 'Output format: json | summary' -r
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from collect" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from collect" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from collect" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from help" -f -a "collect" -d 'Collect repo evidence (artifact existence) for a session\'s claims and fold it into verification statuses (Lane 3)'
complete -c aicx -n "__fish_aicx_using_subcommand results; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand clarify" -l session -d 'Session id (or unique prefix)' -r
complete -c aicx -n "__fish_aicx_using_subcommand clarify" -l agent -d 'Agent: claude | codex | gemini | junie | grok. Inferred from the session id when omitted' -r
complete -c aicx -n "__fish_aicx_using_subcommand clarify" -l hours -d 'Hours to look back when locating the session (default 720)' -r
complete -c aicx -n "__fish_aicx_using_subcommand clarify" -l repo -d 'Repo root evidence is checked against (default: current directory)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand clarify" -l max -d 'Max questions (hard-capped at 5)' -r
complete -c aicx -n "__fish_aicx_using_subcommand clarify" -l format -d 'Output format: markdown | json' -r
complete -c aicx -n "__fish_aicx_using_subcommand clarify" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand clarify" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand clarify" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand wizard" -l smoke-test -d 'Render one frame and exit; used by automated smoke tests'
complete -c aicx -n "__fish_aicx_using_subcommand wizard" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand wizard" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand wizard" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand refs" -s H -l hours -d 'Hours to look back (filter by canonical chunk date)' -r
complete -c aicx -n "__fish_aicx_using_subcommand refs" -s p -l project -d 'Strict project filter: `owner/repo`, `/repo` (cross-org repo name), `owner/` (org wildcard), or a unique exact `name`. Substring matching is intentionally disabled — `-p vista` no longer leaks into `vista-portal`/`vista-datasets`' -r
complete -c aicx -n "__fish_aicx_using_subcommand refs" -l emit -d 'What to print to stdout: summary, paths (default: summary)' -r -f -a "summary\t'Print a compact per-project summary'
paths\t'Print raw file paths (one per line)'"
complete -c aicx -n "__fish_aicx_using_subcommand refs" -s s -l summary -d 'Legacy alias for `--emit summary`'
complete -c aicx -n "__fish_aicx_using_subcommand refs" -l strict -d 'Filter out low-signal noise (<15 lines, task-notifications only)'
complete -c aicx -n "__fish_aicx_using_subcommand refs" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand refs" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand refs" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand state" -s p -l project -d 'Project filter (applies to --info as well as --reset). Supports the standard shapes: `-p owner/repo`, `-p owner/`, `-p /repo`, or a bare `-p name` that must resolve uniquely' -r
complete -c aicx -n "__fish_aicx_using_subcommand state" -l reset -d 'Reset all dedup hashes'
complete -c aicx -n "__fish_aicx_using_subcommand state" -l info -d 'Show state info/statistics'
complete -c aicx -n "__fish_aicx_using_subcommand state" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand state" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand state" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l store-root -d 'Store root directory (default: ~/.aicx)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -s p -l project -d 'Exact project scope. A bare repository name must resolve uniquely' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -s H -l hours -d 'Narrow the dashboard dataset to the last N hours (omit for all time)' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -s o -l output -d 'Output HTML path (default: ~/.aicx/aicx-dashboard.html)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l host -d 'Bind host IP address (default: 127.0.0.1, server mode only)' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l port -d 'Bind TCP port (default: 9478, server mode only)' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l allow-cors-origins -d 'CORS origin policy for server mode: `local` (default), `tailscale`, `all`, or an explicit URL' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l auth-token -d 'Optional explicit auth token (overrides env / file / generated). Server mode only' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l require-auth -d 'Require Bearer auth on dashboard `/api/*` (default: true). Pass `--no-require-auth` to opt out' -r -f -a "true\t''
false\t''"
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l title -d 'Document title' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l preview-chars -d 'Max preview characters per record (0 = no truncation)' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l serve -d 'Run the live local HTTP dashboard instead of generating a static HTML file'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l generate-html -d 'Generate a standalone HTML file (default mode when no mode flag is passed)'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l no-open -d 'Suppress automatic browser open on startup (server mode only)'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l bg -d 'Detach the dashboard server into the background (`--serve` implies `--no-open`)'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l allow-no-origin -d 'Allow mutating dashboard API calls without Origin or Referer (tooling escape hatch)'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l artifacts-root -d 'Vibecrafted artifact root (default: ~/.vibecrafted/artifacts)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l org -d 'Artifact organization bucket' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l repo -d 'Repository bucket (defaults to the current directory name)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l workflow -d 'Workflow filter (matches workflow label, skill code, run/prompt IDs, lane, and title)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l date-from -d 'Inclusive start date (YYYY-MM-DD or YYYY_MMDD)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l date-to -d 'Inclusive end date (YYYY-MM-DD or YYYY_MMDD)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports" -s o -l output -d 'Output HTML path (default: ~/.aicx/aicx-reports.html)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l bundle-output -d 'Optional JSON bundle output path for later import/merge' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l title -d 'Document title' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l preview-chars -d 'Max preview characters per record (0 = no truncation)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l force -d 'Overwrite existing HTML/bundle outputs. Without this flag, the command refuses to clobber a pre-existing file at either output path'
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l deterministic -d 'Derive `generated_at` from the latest record timestamp instead of `Utc::now()`. Also enabled via `AICX_REPORTS_DETERMINISTIC=1` env var'
complete -c aicx -n "__fish_aicx_using_subcommand reports" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand reports" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand reports" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and not __fish_seen_subcommand_from audit repair validate-cards help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and not __fish_seen_subcommand_from audit repair validate-cards help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and not __fish_seen_subcommand_from audit repair validate-cards help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and not __fish_seen_subcommand_from audit repair validate-cards help" -f -a "audit" -d 'Audit derived markdown corpora for Claude signature/thinking leakage and tool JSON noise'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and not __fish_seen_subcommand_from audit repair validate-cards help" -f -a "repair" -d 'Repair derived markdown without inventing or summarizing semantic content'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and not __fish_seen_subcommand_from audit repair validate-cards help" -f -a "validate-cards" -d 'Validate card schema v1/v2 sidecars, headers, hashes, and signal parity'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and not __fish_seen_subcommand_from audit repair validate-cards help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from audit" -l root -d 'Corpus root(s) to scan. Defaults to $HOME/.aicx, $HOME/.ai-contexters, and optional $HOME/.xcia' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from audit" -l emit -d 'Output format: text or json' -r -f -a "text\t'Print a readable text report'
json\t'Print compact JSON'"
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from audit" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from audit" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from audit" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from repair" -l root -d 'Corpus root(s) to scan. Defaults to $HOME/.aicx, $HOME/.ai-contexters, and optional $HOME/.xcia' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from repair" -l manifest -d 'Write the repair manifest to an explicit path, including dry-run previews' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from repair" -l emit -d 'Output format: text or json' -r -f -a "text\t'Print a readable text report'
json\t'Print compact JSON'"
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from repair" -l dry-run -d 'Scan and report changes without modifying files. This is the default when --apply is omitted'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from repair" -l apply -d 'Apply deterministic markdown repairs'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from repair" -l backup -d 'Write backups before applying repairs'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from repair" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from repair" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from repair" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from validate-cards" -l strict -d 'Exit non-zero when hard validation errors are present'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from validate-cards" -l json -d 'Emit compact JSON instead of readable text'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from validate-cards" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from validate-cards" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from validate-cards" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from help" -f -a "audit" -d 'Audit derived markdown corpora for Claude signature/thinking leakage and tool JSON noise'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from help" -f -a "repair" -d 'Repair derived markdown without inventing or summarizing semantic content'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from help" -f -a "validate-cards" -d 'Validate card schema v1/v2 sidecars, headers, hashes, and signal parity'
complete -c aicx -n "__fish_aicx_using_subcommand corpus; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l artifacts-root -d 'Vibecrafted artifact root (default: ~/.vibecrafted/artifacts)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l org -d 'Artifact organization bucket' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l repo -d 'Repository bucket (defaults to the current directory name)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l workflow -d 'Workflow filter (matches workflow label, skill code, run/prompt IDs, lane, and title)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l date-from -d 'Inclusive start date (YYYY-MM-DD or YYYY_MMDD)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l date-to -d 'Inclusive end date (YYYY-MM-DD or YYYY_MMDD)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -s o -l output -d 'Output HTML path (default: ~/.aicx/aicx-reports.html)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l bundle-output -d 'Optional JSON bundle output path for later import/merge' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l title -d 'Document title' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l preview-chars -d 'Max preview characters per record (0 = no truncation)' -r
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l force -d 'Overwrite existing HTML/bundle outputs. Without this flag, the command refuses to clobber a pre-existing file at either output path'
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l deterministic -d 'Derive `generated_at` from the latest record timestamp instead of `Utc::now()`. Also enabled via `AICX_REPORTS_DETERMINISTIC=1` env var'
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand reports-extractor" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -l store-root -d 'Store root directory (default: ~/.aicx)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -l host -d 'Bind host IP address (loopback only; example: 127.0.0.1)' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -l port -d 'Bind TCP port' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -l artifact -d 'Legacy compatibility path retained for status surfaces; not written in server mode' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -l title -d 'Document title' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -l preview-chars -d 'Max preview characters per record (0 = no truncation)' -r
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -l no-open -d 'Suppress automatic browser open on startup'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand dashboard-serve" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand intents" -s p -l project -d 'Repo or store-bucket filters. Omit to scan all projects. Repeated `-p` flags or comma list (`-p a,b`) form a union' -r
complete -c aicx -n "__fish_aicx_using_subcommand intents" -s H -l hours -d 'Hours to look back (default: 720 = 30 days)' -r
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l limit -d 'Maximum number of results to return. Default is command-specific: search/steer 10, tail 20, intents unlimited (full roadmap)' -r
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l sort -d 'Sort order applied after filtering. Default: command-specific' -r -f -a "newest\t''
oldest\t''
score\t''"
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l score -d 'Minimum score threshold (0-100; semantic match confidence)' -r
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l agent -d 'Agent name filter: claude | codex | gemini | junie | codescribe' -r
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l since -d 'Lower date bound: YYYY-MM-DD or relative (e.g., 2026-04-23..)' -r
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l until -d 'Upper date bound: YYYY-MM-DD' -r
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l frame-kind -d 'Frame channel filter: user_msg | agent_reply | internal_thought | tool_call' -r -f -a "user_msg\t''
agent_reply\t''
internal_thought\t''
tool_call\t''"
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l unresolved-mode -d 'Mode for filtering unresolved entries: session (default) or intent' -r -f -a "session\t''
intent\t''"
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l emit -d 'Output format: markdown or json (json includes oracle_status)' -r -f -a "markdown\t''
json\t''"
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l min-confidence -d 'Minimum confidence threshold (1..5) to keep (overrides --strict if both specified)' -r
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l kind -d 'Filter by kind: decision, intent, outcome, task' -r -f -a "decision\t''
intent\t''
outcome\t''
task\t''"
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l unresolved -d 'Return only intent entries without a matching outcome'
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l collapse-session -d 'Collapse multiple intents from the same session into one entry'
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l strict -d 'Only show high-confidence intents'
complete -c aicx -n "__fish_aicx_using_subcommand intents" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand intents" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand intents" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand tail" -s p -l project -d 'Repo or store-bucket filters. Omit to scan all projects. Repeated `-p` flags or comma list (`-p a,b`) form a union' -r
complete -c aicx -n "__fish_aicx_using_subcommand tail" -s H -l hours -d 'Hours to look back (default: 48)' -r
complete -c aicx -n "__fish_aicx_using_subcommand tail" -s k -l kind -d 'Filter by kind: decision, intent, outcome, task' -r
complete -c aicx -n "__fish_aicx_using_subcommand tail" -l limit -d 'Maximum number of results to return. Default is command-specific: search/steer 10, tail 20, intents unlimited (full roadmap)' -r
complete -c aicx -n "__fish_aicx_using_subcommand tail" -l sort -d 'Sort order applied after filtering. Default: command-specific' -r -f -a "newest\t''
oldest\t''
score\t''"
complete -c aicx -n "__fish_aicx_using_subcommand tail" -l score -d 'Minimum score threshold (0-100; semantic match confidence)' -r
complete -c aicx -n "__fish_aicx_using_subcommand tail" -l agent -d 'Agent name filter: claude | codex | gemini | junie | codescribe' -r
complete -c aicx -n "__fish_aicx_using_subcommand tail" -l since -d 'Lower date bound: YYYY-MM-DD or relative (e.g., 2026-04-23..)' -r
complete -c aicx -n "__fish_aicx_using_subcommand tail" -l until -d 'Upper date bound: YYYY-MM-DD' -r
complete -c aicx -n "__fish_aicx_using_subcommand tail" -l frame-kind -d 'Frame channel filter: user_msg | agent_reply | internal_thought | tool_call' -r -f -a "user_msg\t''
agent_reply\t''
internal_thought\t''
tool_call\t''"
complete -c aicx -n "__fish_aicx_using_subcommand tail" -l follow -d 'Subscribe to filesystem events and stream new entries'
complete -c aicx -n "__fish_aicx_using_subcommand tail" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand tail" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand tail" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand serve" -l transport -d 'Transport: stdio (default) or http. Legacy alias: sse' -r -f -a "stdio\t''
http\t''"
complete -c aicx -n "__fish_aicx_using_subcommand serve" -l host -d 'Bind address for streamable HTTP transport (default: 127.0.0.1)' -r
complete -c aicx -n "__fish_aicx_using_subcommand serve" -l port -d 'Port for streamable HTTP transport (default: 8044)' -r
complete -c aicx -n "__fish_aicx_using_subcommand serve" -l allowed-host -d 'Allowed HTTP Host header for streamable HTTP clients. Repeat for remote hostnames/IPs' -r
complete -c aicx -n "__fish_aicx_using_subcommand serve" -l auth-token -d 'Optional explicit auth token (overrides env / file / generated). HTTP transport only' -r
complete -c aicx -n "__fish_aicx_using_subcommand serve" -l require-auth -d 'Require Bearer auth on HTTP transport (default: true). Pass `--no-require-auth` to opt out' -r -f -a "true\t''
false\t''"
complete -c aicx -n "__fish_aicx_using_subcommand serve" -l allow-any-host -d 'Disable HTTP Host header validation. Not recommended outside trusted networks'
complete -c aicx -n "__fish_aicx_using_subcommand serve" -l no-require-auth -d 'Disable Bearer auth on HTTP transport. Only allowed on loopback binds'
complete -c aicx -n "__fish_aicx_using_subcommand serve" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand serve" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand serve" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand init" -s p -l project -d 'Project name override' -r
complete -c aicx -n "__fish_aicx_using_subcommand init" -s a -l agent -d 'Agent override: claude or codex' -r
complete -c aicx -n "__fish_aicx_using_subcommand init" -l model -d 'Model override (optional; if omitted uses agent default)' -r
complete -c aicx -n "__fish_aicx_using_subcommand init" -s H -l hours -d 'Hours to look back for context (default: 4800)' -r
complete -c aicx -n "__fish_aicx_using_subcommand init" -l max-lines -d 'Maximum lines per context section in the prompt' -r
complete -c aicx -n "__fish_aicx_using_subcommand init" -l action -d 'Action focus appended to the prompt' -r
complete -c aicx -n "__fish_aicx_using_subcommand init" -l agent-prompt -d 'Additional agent prompt appended after core rules (verbatim)' -r
complete -c aicx -n "__fish_aicx_using_subcommand init" -l agent-prompt-file -d 'Read additional agent prompt from a file (verbatim)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand init" -l user-only -d 'Only include user messages in context (exclude assistant + reasoning)'
complete -c aicx -n "__fish_aicx_using_subcommand init" -l include-assistant -d 'Include assistant messages (legacy flag; now default)'
complete -c aicx -n "__fish_aicx_using_subcommand init" -l no-run -d 'Build context/prompt only, do not run an agent'
complete -c aicx -n "__fish_aicx_using_subcommand init" -l no-confirm -d 'Skip "Run? (y)es / (n)o" confirmation'
complete -c aicx -n "__fish_aicx_using_subcommand init" -l no-gitignore -d 'Do not auto-modify `.gitignore`'
complete -c aicx -n "__fish_aicx_using_subcommand init" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand init" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand init" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand search" -s p -l project -d 'Project filter. Omit to search every project' -r
complete -c aicx -n "__fish_aicx_using_subcommand search" -s H -l hours -d 'Hours to look back (0 = all time)' -r
complete -c aicx -n "__fish_aicx_using_subcommand search" -s d -l date -d 'Filter by date: single day (2026-03-28), range (2026-03-20..2026-03-28), or open-ended (2026-03-20.. or ..2026-03-28)' -r
complete -c aicx -n "__fish_aicx_using_subcommand search" -l limit -d 'Maximum number of results to return. Default is command-specific: search/steer 10, tail 20, intents unlimited (full roadmap)' -r
complete -c aicx -n "__fish_aicx_using_subcommand search" -l sort -d 'Sort order applied after filtering. Default: command-specific' -r -f -a "newest\t''
oldest\t''
score\t''"
complete -c aicx -n "__fish_aicx_using_subcommand search" -l score -d 'Minimum score threshold (0-100; semantic match confidence)' -r
complete -c aicx -n "__fish_aicx_using_subcommand search" -l agent -d 'Agent name filter: claude | codex | gemini | junie | codescribe' -r
complete -c aicx -n "__fish_aicx_using_subcommand search" -l since -d 'Lower date bound: YYYY-MM-DD or relative (e.g., 2026-04-23..)' -r
complete -c aicx -n "__fish_aicx_using_subcommand search" -l until -d 'Upper date bound: YYYY-MM-DD' -r
complete -c aicx -n "__fish_aicx_using_subcommand search" -l frame-kind -d 'Frame channel filter: user_msg | agent_reply | internal_thought | tool_call' -r -f -a "user_msg\t''
agent_reply\t''
internal_thought\t''
tool_call\t''"
complete -c aicx -n "__fish_aicx_using_subcommand search" -l kind -d 'Filter by indexed document kind: conversations, plans, reports, other' -r -f -a "conversations\t''
conversation\t''
plans\t''
plan\t''
reports\t''
report\t''
other\t''"
complete -c aicx -n "__fish_aicx_using_subcommand search" -l no-semantic -d 'Bypass semantic vector search and run filesystem-fuzzy search'
complete -c aicx -n "__fish_aicx_using_subcommand search" -l evidence -d 'Return an evidence packet: semantic candidates re-ranked by answer/support signals, with source sections and diagnostics'
complete -c aicx -n "__fish_aicx_using_subcommand search" -s j -l json -d 'Emit compact JSON instead of plain text'
complete -c aicx -n "__fish_aicx_using_subcommand search" -l legacy-dense -d 'Use legacy NDJSON reader for dense vector search instead of versioned mmap'
complete -c aicx -n "__fish_aicx_using_subcommand search" -l deep -d 'Dense re-rank (hybrid RRF over tantivy + mmap). Default is lexical-first with a recency prior against the published `_all` CURRENT generation — sub-second answers without loading the embedder or dense vectors'
complete -c aicx -n "__fish_aicx_using_subcommand search" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand search" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand search" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and not __fish_seen_subcommand_from search-quality help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and not __fish_seen_subcommand_from search-quality help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and not __fish_seen_subcommand_from search-quality help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and not __fish_seen_subcommand_from search-quality help" -f -a "search-quality" -d 'Run/list the operator search quality seed matrix'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and not __fish_seen_subcommand_from search-quality help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -l case -d 'Only evaluate selected case ids. Repeat or pass comma-separated values' -r
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -l top -d 'Number of evidence hits inspected per case' -r
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -l limit -d 'Search limit passed to `aicx search --evidence`' -r
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -l seed -d 'Load a custom search-quality seed TOML. Defaults to the embedded curated seed' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -l aicx-bin -d 'Override the aicx binary used by --run' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -l run -d 'Execute the matrix against the active AICX runtime. Omit to only list cases'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -s j -l json -d 'Emit JSON instead of plain text'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -l strict -d 'Exit non-zero when any case fails. Useful for CI or local gates'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from search-quality" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from help" -f -a "search-quality" -d 'Run/list the operator search quality seed matrix'
complete -c aicx -n "__fish_aicx_using_subcommand eval; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -s p -l project -d 'Project filter for `--dry-run` inspection only. Persistent indexing always publishes the global `_all` catalog; `search -p` filters it' -r
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -l sample -d 'Retired compatibility flag; source indexing always scans the selected catalog' -r
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -l dry-run -d 'Preview only. Omit this flag to materialize the persistent semantic index used by `aicx search`' -r -f -a "true\t''
false\t''"
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -s j -l json -d 'Emit JSON stats instead of plain text'
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -l full-rescan -d 'Ignore the source fingerprint and rebuild the selected lexical generation from every cataloged source'
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -l cache-extracts -d 'Cache one readable conversation extract per indexed session under `~/.aicx/extracts/`. Omit to keep content only in live sources and Tantivy (zero filesystem content duplication)'
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -f -a "status" -d 'Show freshness and pending-corpus status for the semantic index'
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -f -a "derive" -d 'Derive project-scoped semantic buckets from the existing `_all` index without re-embedding'
complete -c aicx -n "__fish_aicx_using_subcommand index; and not __fish_seen_subcommand_from status derive help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from status" -s p -l project -d 'Strict project filter, repeatable. Same shapes as `aicx index`: `-p owner/repo`   strict `<owner>/<repo>` slug match `-p owner/`       all repos under that owner (org wildcard) `-p /repo`        same repo name across every owner `-p name`         name matches an owner OR a repo (cross-org)' -r
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from status" -s j -l json -d 'Emit JSON status instead of plain text'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from status" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from status" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from status" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from derive" -s p -l project -d 'Strict project filter, repeatable. Omit only with --all-projects' -r
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from derive" -l all-projects -d 'Derive buckets for every project present in the existing `_all` index'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from derive" -s j -l json -d 'Emit JSON report instead of plain text'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from derive" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from derive" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from derive" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from help" -f -a "status" -d 'Show freshness and pending-corpus status for the semantic index'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from help" -f -a "derive" -d 'Derive project-scoped semantic buckets from the existing `_all` index without re-embedding'
complete -c aicx -n "__fish_aicx_using_subcommand index; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand config; and not __fish_seen_subcommand_from init show inspect help" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand config; and not __fish_seen_subcommand_from init show inspect help" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand config; and not __fish_seen_subcommand_from init show inspect help" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand config; and not __fish_seen_subcommand_from init show inspect help" -f -a "init" -d 'Write a default `~/.aicx/config.toml` with cloud-embedder pre-selected. Bails if the file exists unless `--force`'
complete -c aicx -n "__fish_aicx_using_subcommand config; and not __fish_seen_subcommand_from init show inspect help" -f -a "show" -d 'Display the resolved embedder configuration after merging env, `embedder.toml`, `config.toml`, and built-in defaults'
complete -c aicx -n "__fish_aicx_using_subcommand config; and not __fish_seen_subcommand_from init show inspect help" -f -a "inspect" -d 'Inspect the exact running binary, install shadows, config, MCP target, embedder identity, and published index generation without changing them'
complete -c aicx -n "__fish_aicx_using_subcommand config; and not __fish_seen_subcommand_from init show inspect help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from init" -l path -d 'Write to a custom path instead of `~/.aicx/config.toml`. Useful for shared / repo-local config snapshots' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from init" -l force -d 'Overwrite the existing config file if present'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from init" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from init" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from init" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from show" -s j -l json -d 'Emit JSON instead of human-readable text'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from show" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from show" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from show" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from inspect" -l mcp-config -d 'Inspect an external MCP client/mux config for its configured AICX target. Repeat for multiple clients. Files are read only and never rewritten' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from inspect" -s j -l json -d 'Emit the stable machine-readable inspection contract'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from inspect" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from inspect" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from inspect" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "init" -d 'Write a default `~/.aicx/config.toml` with cloud-embedder pre-selected. Bails if the file exists unless `--force`'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "show" -d 'Display the resolved embedder configuration after merging env, `embedder.toml`, `config.toml`, and built-in defaults'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "inspect" -d 'Inspect the exact running binary, install shadows, config, MCP target, embedder identity, and published index generation without changing them'
complete -c aicx -n "__fish_aicx_using_subcommand config; and __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand read" -l max-chars -d 'Truncate chunk content to this many UTF-8 characters' -r
complete -c aicx -n "__fish_aicx_using_subcommand read" -s j -l json -d 'Emit compact JSON instead of readable text'
complete -c aicx -n "__fish_aicx_using_subcommand read" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand read" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand read" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand open" -l max-chars -d 'Truncate chunk content to this many UTF-8 characters' -r
complete -c aicx -n "__fish_aicx_using_subcommand open" -s j -l json -d 'Emit compact JSON instead of readable text'
complete -c aicx -n "__fish_aicx_using_subcommand open" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand open" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand open" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l run-id -d 'Filter by run_id (exact match)' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l prompt-id -d 'Filter by prompt_id (exact match)' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -s k -l kind -d 'Filter by kind: conversations, plans, reports, other' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -s p -l project -d 'Repo or store-bucket filters. Omit to search all projects. Repeated `-p` flags or comma list (`-p a,b`) form a union' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -s d -l date -d 'Filter by date: single day (2026-03-28), range (2026-03-20..2026-03-28), or open-ended (2026-03-20.. or ..2026-03-28)' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l limit -d 'Maximum number of results to return. Default is command-specific: search/steer 10, tail 20, intents unlimited (full roadmap)' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l sort -d 'Sort order applied after filtering. Default: command-specific' -r -f -a "newest\t''
oldest\t''
score\t''"
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l score -d 'Minimum score threshold (0-100; semantic match confidence)' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l agent -d 'Agent name filter: claude | codex | gemini | junie | codescribe' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l since -d 'Lower date bound: YYYY-MM-DD or relative (e.g., 2026-04-23..)' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l until -d 'Upper date bound: YYYY-MM-DD' -r
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l frame-kind -d 'Frame channel filter: user_msg | agent_reply | internal_thought | tool_call' -r -f -a "user_msg\t''
agent_reply\t''
internal_thought\t''
tool_call\t''"
complete -c aicx -n "__fish_aicx_using_subcommand steer" -s j -l json -d 'Emit compact JSON with oracle_status instead of readable text'
complete -c aicx -n "__fish_aicx_using_subcommand steer" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand steer" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand steer" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand migrate" -l legacy-root -d 'Override legacy input store root (default: ~/.ai-contexters)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand migrate" -l store-root -d 'Override AICX store root (default: ~/.aicx)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand migrate" -l cards-v2 -d 'Upgrade store cards v1 -> v2 in place (sidecar schema/honesty fields, bracket header -> YAML frontmatter; body bytes never change). Optional ROOT overrides the walked directory (default: canonical store dir). Dry-run by default; pass --apply to write' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand migrate" -l dry-run -d 'Dry run: show what would be moved without modifying files'
complete -c aicx -n "__fish_aicx_using_subcommand migrate" -l no-intent-schema -d 'Skip post-migration intent schema scan on the canonical store'
complete -c aicx -n "__fish_aicx_using_subcommand migrate" -l apply -d 'Write the cards-v2 migration (without it, --cards-v2 is a dry run)'
complete -c aicx -n "__fish_aicx_using_subcommand migrate" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand migrate" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand migrate" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand migrate-intent-schema" -s p -l project -d 'Strict project filter: `owner/repo`, `/repo` (cross-org repo name), `owner/` (org wildcard), or a unique exact `name`. Omit to scan the whole store. Substring matching is intentionally disabled' -r
complete -c aicx -n "__fish_aicx_using_subcommand migrate-intent-schema" -l store-root -d 'Override AICX store root (default: ~/.aicx)' -r -F
complete -c aicx -n "__fish_aicx_using_subcommand migrate-intent-schema" -l dry-run -d 'Dry run: show classification counts without writing sidecars'
complete -c aicx -n "__fish_aicx_using_subcommand migrate-intent-schema" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand migrate-intent-schema" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand migrate-intent-schema" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l restore-quarantine -d 'Restore files from a quarantine manifest slug' -r
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l format -d 'Output format: text (default), json' -r
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l rebuild-steer-index -d 'Delete and rebuild the steer index from the canonical store when corrupted or schema-incompatible. Narrower contract than the legacy `--fix` (which was a no-op for sidecars/index consistency/empty bodies — those have dedicated flags)'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l fix-buckets -d 'Move suspicious top-level corpus buckets to $HOME/.aicx/quarantine/. Buckets that are merely CamelCase (legitimate GitHub orgs like `LibraxisAI`, `Vetcoders`, `Loctree`, `Sampleorg`) are canonicalized in place to lowercase instead of quarantined, merging into existing lowercase buckets if present'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l dry-run -d 'With --fix-buckets, preview the planned canonicalize/quarantine actions without modifying the filesystem. Output entries are prefixed with `[dry-run]`. Use this before running `--fix-buckets` against a large store to verify the classification before commit'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l rebuild-sidecars -d 'Emit a reviewable bash script for missing sidecar backfill'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l prune-empty-bodies -d 'Emit a reviewable bash script for moving empty-body chunks to quarantine'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l migrate-identities -d 'Plan the one-time project-identity migration: persisted index.json key casing → GitHub nameWithOwner canon (lowercase, the fresh-derivation form), store/ directory normalization, historical-card alias map (annotate-only, cards never rewritten), and a typo-twin bucket report. Dry-run by default — writes migration/identity-manifest.json + identity-report.md with zero store mutation; add --apply to execute the planned renames'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l apply -d 'With --prune-empty-bodies, move empty-body chunks into recoverable quarantine. With --migrate-identities, execute the planned identity renames. Refuses to combine with --dry-run: on a store-mutating surface the preview flag must always win, so the ambiguous combination is a parse error'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -s y -l yes -d 'Assume yes on doctor cleanup prompts'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l force -d 'Skip dry-run preview and prompts; intended for CI cleanup runs'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l check-dedup -d 'Report duplicate content_sha256 groups across store and context-corpus'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -s v -l verbose -d 'Print recommendations for green checks too'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l smoke -d 'Run actual real HTTP POST / embedder tests instead of skipping them. Doctor stays fast and cheap by default; this flag exercises the AI provider'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l deep -d 'Run the full forensic pass: recursive store scans, semantic-index reconciliation, and payload-level checks. Emits progress phases with heartbeats and an explicit estimated scope; Ctrl-C cancels cleanly between operations. Without this flag (and without fix / full-scan flags, which imply it) doctor answers from the bounded fast health pass — metadata, leases, manifests, and sampled invariants only; anything it cannot prove is reported as `unknown`, never as healthy'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l oracle -d 'Report AICX Oracle readiness: ready | degraded | unsafe_for_loctree_scope.'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand doctor" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand health" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand health" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand health" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand warmup" -s j -l json -d 'Emit JSON instead of readable text'
complete -c aicx -n "__fish_aicx_using_subcommand warmup" -s v -l verbose -d 'Verbose diagnostics: echo per-file extractor warnings to stderr'
complete -c aicx -n "__fish_aicx_using_subcommand warmup" -l project-fuzzy -d 'Opt in to project-family matching. By default project filters are exact and an ambiguous bare repository name fails closed'
complete -c aicx -n "__fish_aicx_using_subcommand warmup" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "completions" -d 'Generate shell completions for the canonical CLI grammar'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "overlay" -d 'Join typed canonical intents to the current Loctree anchor catalog'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "claude" -d 'Extract Claude sessions into local reports'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "codex" -d 'Extract Codex sessions into local reports'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "all" -d 'Extract sessions from all supported agents into local reports'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "extract" -d 'Extract a single session for one agent — by session id or direct file'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "conversations" -d 'Batch-export conversation JSON files without writing to the canonical store'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "catalog" -d 'Rebuild the durable extract-era session catalog (no per-frame cards)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "ingest" -d 'Ingest operator-owned source documents into the canonical corpus'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "list" -d 'List raw agent session sources on disk (pre-extraction inputs)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "sources" -d 'Audit and explicitly protect raw source roots'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "sessions" -d 'Discover and list agent sessions on disk (session surface)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "claims" -d 'Lane 2: extract agent claims (audit targets) from a session'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "results" -d 'Lane 3: collect repo evidence for a session\'s claims and verify them'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "clarify" -d 'Lane 5: generate at most 5 A/B/C decision questions from verified gaps'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "wizard" -d 'Interactive daily-driver entrypoint for corpus, doctor, intents, and store'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "refs" -d 'List chunks in the canonical store inventory'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "state" -d 'Manage extraction dedup state (watermarks and hashes)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "dashboard" -d 'Generate a searchable HTML dashboard from the canonical store, or serve it locally'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "reports" -d 'Extract Vibecrafted workflow and marbles reports into a standalone HTML explorer'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "corpus" -d 'Audit or repair derived corpus markdown'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "reports-extractor" -d 'Deprecated compatibility shim for `aicx reports`'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "dashboard-serve" -d 'Deprecated compatibility shim for `aicx dashboard --serve`'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "intents" -d 'Extract structured intents from the canonical corpus'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "tail" -d 'Print recent intents/chunks (snapshot mode); add --follow to stream new arrivals'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "serve" -d 'Run aicx as an MCP server'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "init" -d 'Retired compatibility shim; prints migration guidance'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "search" -d 'Search the CURRENT source/extract index. Lexical-first by default; optional dense rerank with --deep. When no index exists, the only fallback is a bounded recency-ranked filesystem search'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "eval" -d 'Run local evaluation helpers for retrieval/search quality'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "index" -d 'Build the source-driven lexical index. Use `--dry-run` to preview parsing and filtering without writing extracts or publishing CURRENT'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "config" -d 'Manage `$HOME/.aicx/config.toml` for embedders and endpoints'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "read" -d 'Read one canonical chunk by path, file name, or `chunk:<id>` reference'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "steer" -d 'Retrieve chunks by steering metadata (requires --features lance)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "migrate" -d 'Migrate legacy ~/.ai-contexters/ data into the canonical AICX store'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "migrate-intent-schema" -d 'Classify stored chunks into 11-type intent entries and report counts'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "doctor" -d 'Diagnose and optionally repair the canonical store and steer index'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "health" -d 'Emit the bounded AICX health report as JSON for automation'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "warmup" -d 'Warm/probe the configured local embedder before interactive search'
complete -c aicx -n "__fish_aicx_using_subcommand help; and not __fish_seen_subcommand_from completions overlay claude codex all extract conversations catalog ingest list sources sessions claims results clarify wizard refs state dashboard reports corpus reports-extractor dashboard-serve intents tail serve init search eval index config read steer migrate migrate-intent-schema doctor health warmup help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from extract" -f -a "codex" -d 'OpenAI Codex CLI rollouts (~/.codex/sessions)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from extract" -f -a "claude" -d 'Claude Code sessions (~/.claude/projects)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from extract" -f -a "gemini" -d 'Gemini CLI chats (~/.gemini/tmp/<hash>/chats)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from extract" -f -a "grok" -d 'Grok CLI sessions (~/.grok)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from extract" -f -a "junie" -d 'JetBrains Junie event logs (~/.junie/sessions)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from catalog" -f -a "rebuild" -d 'Walk all source roots and rewrite `~/.aicx/catalog/sessions.jsonl`'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from catalog" -f -a "resolve" -d 'Resolve one session id from the durable catalog'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from sources" -f -a "protect" -d 'Opt in to local source-root protection'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from sessions" -f -a "current" -d 'Print the current agent session id for commit trailers and handoffs'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from sessions" -f -a "list" -d 'List discovered agent sessions, newest first'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from sessions" -f -a "show" -d 'Show one session\'s metadata, located by id (or a unique prefix)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from sessions" -f -a "report" -d 'Unified truth report for one session: human intents (Lane 1), agent claims + evidence verification (Lanes 2-3), contract fractures (Lane 4) and clarify decisions (Lane 5) in a single rendering'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from claims" -f -a "extract" -d 'Extract Unverified claims (Lane 2) from a session\'s conversation'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from results" -f -a "collect" -d 'Collect repo evidence (artifact existence) for a session\'s claims and fold it into verification statuses (Lane 3)'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from corpus" -f -a "audit" -d 'Audit derived markdown corpora for Claude signature/thinking leakage and tool JSON noise'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from corpus" -f -a "repair" -d 'Repair derived markdown without inventing or summarizing semantic content'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from corpus" -f -a "validate-cards" -d 'Validate card schema v1/v2 sidecars, headers, hashes, and signal parity'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from eval" -f -a "search-quality" -d 'Run/list the operator search quality seed matrix'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from index" -f -a "status" -d 'Show freshness and pending-corpus status for the semantic index'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from index" -f -a "derive" -d 'Derive project-scoped semantic buckets from the existing `_all` index without re-embedding'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from config" -f -a "init" -d 'Write a default `~/.aicx/config.toml` with cloud-embedder pre-selected. Bails if the file exists unless `--force`'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from config" -f -a "show" -d 'Display the resolved embedder configuration after merging env, `embedder.toml`, `config.toml`, and built-in defaults'
complete -c aicx -n "__fish_aicx_using_subcommand help; and __fish_seen_subcommand_from config" -f -a "inspect" -d 'Inspect the exact running binary, install shadows, config, MCP target, embedder identity, and published index generation without changing them'
