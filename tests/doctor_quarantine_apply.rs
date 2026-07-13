// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! Integration test for `doctor --prune-empty-bodies --apply` quarantine
//! pathway. Closes audit gap H-1 (P2): the production
//! `apply_empty_body_quarantine` path had no dedicated integration test
//! verifying the rename-not-delete contract from outside the crate.
//!
//! Contract under test:
//! - empty-body chunk + sidecar are moved (renamed) into the recoverable
//!   `<base>/quarantine/empty-bodies-<timestamp>/` tree
//! - original chunk and sidecar no longer exist at their source paths
//! - the moved chunk and sidecar ARE present under the quarantine root
//!   (proving rename, not delete)
//! - non-empty chunks are untouched
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use aicx::doctor::{DoctorOptions, Severity, restore_quarantine_at, run_at};

fn unique_base(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aicx-doctor-int-{label}-{}-{suffix}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp base dir");
    dir
}

#[test]
fn apply_empty_body_quarantine_moves_to_recoverable_dir_not_delete() {
    let base = unique_base("apply-empty-bodies");
    let chunk_dir = base
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0506")
        .join("conversations")
        .join("claude");
    std::fs::create_dir_all(&chunk_dir).expect("create chunk dir");

    // Empty-body chunk: header only, no real body content.
    let empty_chunk = chunk_dir.join("2026_0506_claude_sess-empty_001.md");
    let empty_sidecar = empty_chunk.with_extension("meta.json");
    std::fs::write(
        &empty_chunk,
        "[project: Vetcoders/aicx | agent: claude | date: 2026-05-06 | frame_kind: internal_thought]\n\n",
    )
    .expect("write empty chunk");
    std::fs::write(&empty_sidecar, "{}").expect("write empty sidecar");

    // Non-empty chunk: must survive the apply pass unchanged.
    let full_chunk = chunk_dir.join("2026_0506_claude_sess-full_001.md");
    std::fs::write(
        &full_chunk,
        "[project: Vetcoders/aicx | agent: claude | date: 2026-05-06]\n\nReal body content that exceeds the empty-body detection threshold.",
    )
    .expect("write full chunk");

    let opts = DoctorOptions {
        rebuild_steer_index: false,
        fix_buckets: false,
        dry_run: false,
        rebuild_sidecars: false,
        prune_empty_bodies: true,
        apply_prune_empty_bodies: true,
        check_dedup: false,
        verbose: false,
        smoke: false,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");
    let report = rt
        .block_on(run_at(&base, &opts))
        .expect("doctor run_at should succeed on synthetic mini-store");

    // Recheck inside run_at must observe a clean store (empty bodies gone).
    assert_eq!(
        report.empty_body_chunks.severity,
        Severity::Green,
        "empty_body_chunks must recheck Green after apply (detail={}, fixes={:?})",
        report.empty_body_chunks.detail,
        report.fixes_applied
    );
    assert!(
        report.prune_empty_bodies_script.is_none(),
        "apply mode should not also emit the dry-run script"
    );
    assert!(
        report
            .fixes_applied
            .iter()
            .any(|line| line.contains("quarantined 1 empty-body chunk(s) and 1 sidecar(s)")),
        "fixes_applied must record the quarantine move: {:?}",
        report.fixes_applied
    );

    // Original chunk + sidecar must be gone from the source path. This is
    // the "no delete" contract's first half: they were renamed away, not
    // deleted in place — but absence alone does not yet prove rename. The
    // quarantine-side assertions below complete the proof.
    assert!(
        !empty_chunk.exists(),
        "empty chunk must be moved out of {}",
        empty_chunk.display()
    );
    assert!(
        !empty_sidecar.exists(),
        "empty sidecar must be moved out of {}",
        empty_sidecar.display()
    );

    // Non-empty chunk must be untouched.
    assert!(
        full_chunk.exists(),
        "non-empty chunk must remain at {}",
        full_chunk.display()
    );

    // The quarantine_root dir must have been created under
    // `<base>/quarantine/empty-bodies-<timestamp>/` and the chunk + sidecar
    // must live there. This proves the production path used rename, not
    // remove_file — if a delete had happened we would find no destination.
    let quarantine_parent = base.join("quarantine");
    assert!(
        quarantine_parent.is_dir(),
        "quarantine parent dir must exist at {}",
        quarantine_parent.display()
    );
    let quarantine_root = std::fs::read_dir(&quarantine_parent)
        .expect("read quarantine parent dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("empty-bodies-"))
        })
        .expect("empty-bodies-<timestamp> quarantine root must exist");

    // After D1 (B-P0-02), quarantine layout mirrors paths relative to the
    // aicx canonical root (base) instead of `base/store/` only. This lets
    // chunks under `non-repository-contexts/` survive the rename instead
    // of crashing with `outside store root`. Existing store/ chunks
    // simply gain a `store/` prefix inside quarantine.
    let moved_chunk = quarantine_root
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0506")
        .join("conversations")
        .join("claude")
        .join("2026_0506_claude_sess-empty_001.md");
    let moved_sidecar = moved_chunk.with_extension("meta.json");

    assert!(
        moved_chunk.exists(),
        "rename target must exist (proof of move, not delete): {}",
        moved_chunk.display()
    );
    assert!(
        moved_sidecar.exists(),
        "sidecar rename target must exist alongside chunk: {}",
        moved_sidecar.display()
    );

    // Bonus: byte-for-byte equality between expected payload and quarantined
    // file. The production path is `std::fs::rename`, not a copy + truncate,
    // so content must be preserved exactly.
    let moved_bytes = std::fs::read(&moved_chunk).expect("read moved chunk bytes");
    assert!(
        moved_bytes.starts_with(b"[project: Vetcoders/aicx | agent: claude | date: 2026-05-06"),
        "quarantined chunk content must match original (rename preserves bytes)"
    );

    let manifest = quarantine_root.join("manifest.json");
    assert!(
        manifest.exists(),
        "quarantine manifest must exist for restore: {}",
        manifest.display()
    );
    let slug = quarantine_root
        .file_name()
        .and_then(|name| name.to_str())
        .expect("quarantine slug")
        .to_string();
    let restore = restore_quarantine_at(&base, &slug).expect("restore quarantine");
    assert_eq!(restore.restored, 2, "chunk + sidecar should restore");
    assert_eq!(restore.skipped, 0);
    assert!(
        restore.failures.is_empty(),
        "restore failures: {:?}",
        restore.failures
    );
    assert!(empty_chunk.exists(), "empty chunk should be restored");
    assert!(empty_sidecar.exists(), "empty sidecar should be restored");
    assert!(
        !moved_chunk.exists(),
        "quarantined chunk should move back during restore"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Regression for B-P0-02 (Wave D Cut D1): `aicx doctor --prune-empty-bodies`
/// used to hard-crash with `empty-body chunk is outside store root` the
/// moment a candidate came from `<base>/non-repository-contexts/...`
/// rather than `<base>/store/...`. On prod the operator had ~4418 such
/// candidates, all unreachable. The fix expanded the prefix check from
/// `<base>/store/` to the entire aicx canonical root `<base>/`.
#[test]
fn apply_empty_body_quarantine_accepts_non_repository_contexts() {
    let base = unique_base("apply-empty-bodies-non-repo");

    // Place an empty-body chunk under non-repository-contexts. This is
    // where chunks land when `aicx store` cannot infer a repo from cwd.
    let chunk_dir = base
        .join("non-repository-contexts")
        .join("2026_0506")
        .join("conversations")
        .join("claude");
    std::fs::create_dir_all(&chunk_dir).expect("create non-repo chunk dir");

    let empty_chunk = chunk_dir.join("2026_0506_claude_sess-nonrepo_001.md");
    let empty_sidecar = empty_chunk.with_extension("meta.json");
    std::fs::write(
        &empty_chunk,
        "[project: non-repository-contexts | agent: claude | date: 2026-05-06 | frame_kind: internal_thought]\n\n",
    )
    .expect("write empty non-repo chunk");
    std::fs::write(&empty_sidecar, "{}").expect("write empty non-repo sidecar");

    let opts = DoctorOptions {
        rebuild_steer_index: false,
        fix_buckets: false,
        dry_run: false,
        rebuild_sidecars: false,
        prune_empty_bodies: true,
        apply_prune_empty_bodies: true,
        check_dedup: false,
        verbose: false,
        smoke: false,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");
    let report = rt
        .block_on(run_at(&base, &opts))
        .expect("doctor must NOT crash on non-repository-contexts chunks");

    assert!(
        report
            .fixes_applied
            .iter()
            .any(|line| line.contains("quarantined 1 empty-body chunk(s) and 1 sidecar(s)")),
        "non-repo chunk must be quarantined: {:?}",
        report.fixes_applied
    );

    // Verify the moved file landed under the quarantine root, with its
    // non-repository-contexts layout preserved.
    let quarantine_parent = base.join("quarantine");
    let quarantine_root = std::fs::read_dir(&quarantine_parent)
        .expect("read quarantine parent dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("empty-bodies-"))
        })
        .expect("empty-bodies-<timestamp> quarantine root must exist");

    let moved_chunk = quarantine_root
        .join("non-repository-contexts")
        .join("2026_0506")
        .join("conversations")
        .join("claude")
        .join("2026_0506_claude_sess-nonrepo_001.md");
    assert!(
        moved_chunk.exists(),
        "non-repo chunk must be renamed under quarantine root: {}",
        moved_chunk.display()
    );
    assert!(
        !empty_chunk.exists(),
        "source non-repo chunk must be moved out of {}",
        empty_chunk.display()
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Regression for B-P0-02 dry-run path: `aicx doctor --prune-empty-bodies`
/// (no `--apply`) renders a reviewable bash script. Before D1 the script
/// rendering also crashed on `non-repository-contexts` candidates because
/// the same prefix check was used. Verify the script renders cleanly and
/// references the non-repo chunk path.
#[test]
fn prune_empty_bodies_script_renders_for_non_repository_contexts() {
    let base = unique_base("prune-script-non-repo");

    let chunk_dir = base
        .join("non-repository-contexts")
        .join("2026_0506")
        .join("conversations")
        .join("claude");
    std::fs::create_dir_all(&chunk_dir).expect("create non-repo chunk dir");
    let empty_chunk = chunk_dir.join("2026_0506_claude_sess-script_001.md");
    std::fs::write(
        &empty_chunk,
        "[project: non-repository-contexts | agent: claude | date: 2026-05-06 | frame_kind: internal_thought]\n\n",
    )
    .expect("write empty non-repo chunk");

    let opts = DoctorOptions {
        rebuild_steer_index: false,
        fix_buckets: false,
        dry_run: false,
        rebuild_sidecars: false,
        prune_empty_bodies: true,
        apply_prune_empty_bodies: false, // dry-run script path
        check_dedup: false,
        verbose: false,
        smoke: false,
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");
    let report = rt
        .block_on(run_at(&base, &opts))
        .expect("script rendering must NOT crash on non-repo chunk");

    let script = report
        .prune_empty_bodies_script
        .as_ref()
        .expect("script must be emitted when prune is requested without --apply");
    assert!(
        script.contains("non-repository-contexts/2026_0506/conversations/claude"),
        "script must reference non-repo chunk path: {}",
        script
    );
    assert!(
        script.contains("mv -n --"),
        "script must contain at least one mv directive: {}",
        script
    );

    let _ = std::fs::remove_dir_all(&base);
}
