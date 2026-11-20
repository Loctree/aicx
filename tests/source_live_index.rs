//! Publication of stable sessions while other catalog sources change.
#![cfg(feature = "app")]

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aicx::progress::{NoopReporter, Phase, PhaseOutcome, Reporter};
use serde_json::json;

struct AtPublish {
    path: PathBuf,
    phase: &'static str,
    replacement: bool,
    bytes: Vec<u8>,
    fired: AtomicBool,
}

impl Reporter for AtPublish {
    fn phase_start(&self, phase: &Phase) {
        if phase.name != self.phase || self.fired.swap(true, Ordering::SeqCst) {
            return;
        }
        if self.replacement {
            let replacement = self.path.with_extension("replacement");
            std::fs::write(&replacement, &self.bytes).unwrap();
            std::fs::rename(replacement, &self.path).unwrap();
        } else {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .unwrap();
            file.write_all(&self.bytes).unwrap();
        }
    }
    fn phase_tick(&self, _: &Phase, _: u64) {}
    fn phase_finish(&self, _: &Phase, _: &PhaseOutcome) {}
}

#[test]
fn live_sources_are_deferred_then_indexed_and_policy_changes_refuse_publication() {
    let root = std::env::temp_dir().join(format!(
        "aicx-live-index-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    let previous = std::env::var_os("AICX_HOME");
    unsafe {
        std::env::set_var("AICX_HOME", &root);
    }
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut catalog = String::new();
    let mut sources = Vec::new();
    for n in 1..=2 {
        let sid = format!("00000000-0000-4000-8000-{n:012}");
        let path = root.join(format!("{sid}.jsonl"));
        let rows = [
            json!({"timestamp":"2026-10-01T10:00:00Z","type":"session_meta","payload":{"id":sid,"cwd":cwd,"timestamp":"2026-10-01T10:00:00Z"}}),
            json!({"timestamp":"2026-10-01T10:00:01Z","type":"turn_context","payload":{"turn_id":"t1","cwd":cwd}}),
            json!({"timestamp":"2026-10-01T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":format!("Original message {n}")}}),
        ];
        std::fs::write(
            &path,
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        catalog.push_str(&format!("{}\n", json!({"schema":"aicx.catalog.session.v1","agent":"codex","session_id":sid,"source_path":path,"project":"loctree/aicx","cwd":cwd,"date":"2026-10-01"})));
        sources.push(path);
    }
    std::fs::write(root.join("catalog/sessions.jsonl"), catalog).unwrap();
    let appended = format!(
        "{}\n",
        json!({"timestamp":"2026-10-01T10:00:03Z","type":"event_msg","payload":{"type":"user_message","message":"fresh appended message"}})
    );
    let changing = Arc::new(AtPublish {
        path: sources[1].clone(),
        phase: "index_publish",
        replacement: false,
        bytes: appended.into_bytes(),
        fired: AtomicBool::new(false),
    });
    let first =
        aicx::source_index::build_with_reporter(&root, &[], false, false, false, changing).unwrap();
    assert_eq!(first.sources_deferred, 1);
    assert_eq!(first.lexical_docs, 1);
    let chunks = aicx::vector_index::current_lexical_chunks().unwrap();
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].text.contains("Original message 1"));
    let second = aicx::source_index::build_with_reporter(
        &root,
        &[],
        false,
        false,
        false,
        Arc::new(NoopReporter),
    )
    .unwrap();
    assert!(!second.unchanged);
    assert_eq!(second.sources_deferred, 0);
    assert_eq!(second.lexical_docs, 2);
    assert!(
        aicx::vector_index::current_lexical_chunks()
            .unwrap()
            .iter()
            .any(|chunk| chunk.text.contains("fresh appended message"))
    );

    let replacement = Arc::new(AtPublish {
        path: sources[1].clone(),
        phase: "index_admission",
        replacement: true,
        bytes: std::fs::read(&sources[1]).unwrap(),
        fired: AtomicBool::new(false),
    });
    let third =
        aicx::source_index::build_with_reporter(&root, &[], false, true, false, replacement)
            .unwrap();
    assert_eq!(
        third.sources_deferred, 1,
        "physical replacement must be deferred"
    );
    assert_eq!(third.lexical_docs, 1);
    let fourth = aicx::source_index::build_with_reporter(
        &root,
        &[],
        false,
        false,
        false,
        Arc::new(NoopReporter),
    )
    .unwrap();
    assert_eq!(fourth.lexical_docs, 2);

    let current_before = aicx::vector_index::hybrid_manifest_path(None).unwrap();
    let policy = Arc::new(AtPublish {
        path: root.join(".aicxignore"),
        phase: "index_admission",
        replacement: false,
        bytes: b"/private-example\n".to_vec(),
        fired: AtomicBool::new(false),
    });
    let err = aicx::source_index::build_with_reporter(&root, &[], false, true, false, policy)
        .unwrap_err();
    assert!(format!("{err:#}").contains("privacy policy changed"));
    assert_eq!(
        aicx::vector_index::hybrid_manifest_path(None).unwrap(),
        current_before
    );
    match previous {
        Some(value) => unsafe { std::env::set_var("AICX_HOME", value) },
        None => unsafe { std::env::remove_var("AICX_HOME") },
    }
    std::fs::remove_dir_all(root).unwrap();
}
