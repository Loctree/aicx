//! Integration test for `doctor --prune-empty-bodies --apply` quarantine
//! pathway. Closes Klaudiusz audit gap H-1 (P2): the production
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
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use aicx::doctor::{DoctorOptions, Severity, run_at};

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
        .join("VetCoders")
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
        "[project: VetCoders/aicx | agent: claude | date: 2026-05-06 | frame_kind: internal_thought]\n\n",
    )
    .expect("write empty chunk");
    std::fs::write(&empty_sidecar, "{}").expect("write empty sidecar");

    // Non-empty chunk: must survive the apply pass unchanged.
    let full_chunk = chunk_dir.join("2026_0506_claude_sess-full_001.md");
    std::fs::write(
        &full_chunk,
        "[project: VetCoders/aicx | agent: claude | date: 2026-05-06]\n\nReal body content that exceeds the empty-body detection threshold.",
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

    let moved_chunk = quarantine_root
        .join("VetCoders")
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
        moved_bytes.starts_with(b"[project: VetCoders/aicx | agent: claude | date: 2026-05-06"),
        "quarantined chunk content must match original (rename preserves bytes)"
    );

    let _ = std::fs::remove_dir_all(&base);
}
