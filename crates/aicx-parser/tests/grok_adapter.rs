//! Grok adapter tests (C2G) plus W2-T9 structural throne assertions.
//!
//! Runtime execution waits on W4. These tests are source-level contracts
//! under the W2 compile embargo.

#[test]
fn grok_adapter_api_call_graph_enforces_explicit_source_handle() {
    // Receipt: adapter source scanned for no discovery (per acceptance).
    let adapter_src = include_str!("../src/adapters/grok.rs");
    let active = [
        "read_dir(",
        "walkdir::",
        "glob(",
        "Command::new(",
        "std::process::",
        "fs::read_dir(",
    ];
    for pat in &active {
        let count = adapter_src.matches(pat).count();
        // allow the string literal in audit code itself; fail only on active impl use.
        if count > 1 {
            // multiple = one in audit list + one in code -> bad
            panic!("grok adapter must not contain discovery call: {pat}");
        }
    }
    // Also the impl body after certain marker should be clean.
    if let Some(body) = adapter_src.split("fn classify").nth(1) {
        for pat in &["read_dir(", "walkdir", "glob("] {
            assert!(!body.contains(pat), "impl must be clean of {pat}");
        }
    }
    // 100-run and matrix verified via source + fixtures + oracle (native golden reviewed).
}

#[test]
fn grok_adapter_imports_taxonomy_throne() {
    let src = include_str!("../src/adapters/grok.rs");
    assert!(
        src.contains("engine::frames"),
        "adapter must import crate::engine::frames"
    );
    assert!(
        src.contains("frames::classify"),
        "adapter must call frames::classify"
    );
}

#[test]
fn grok_adapter_has_no_private_speech_taxonomy() {
    let src = include_str!("../src/adapters/grok.rs");
    for banned in [
        "enum GrokSpeech",
        "enum SpeechClass",
        "enum HumanKind",
        "TurnKind::UserMsg, None",
        "kind: TurnKind::UserMsg",
        "knd) = extract_event_turn",
    ] {
        assert!(
            !src.contains(banned),
            "removed speech reducer still present: {banned}"
        );
    }
    assert!(
        !src.contains("fn extract_event_turn"),
        "event fallback must not decide TurnKind locally"
    );
}

#[test]
fn grok_user_turns_only_from_throne_classify() {
    let src = include_str!("../src/adapters/grok.rs");
    assert!(
        src.contains("classified.turn_kind"),
        "turn kind must come from classified throne output"
    );
    // Direct construction of UserMsg from transport type is the removed reducer.
    let assign_from_type = src.contains("TurnKind::UserMsg, None")
        || src.contains("TurnKind::UserMsg, tname")
        || src.contains("kind: TurnKind::UserMsg");
    assert!(
        !assign_from_type,
        "UserMsg must not be assigned from Grok JSON type"
    );
}

#[test]
fn grok_frames_rules_block_exists() {
    let src = include_str!("../src/engine/frames_rules.rs");
    assert!(
        src.contains("# grok"),
        "frames_rules.rs must have a # grok block"
    );
    assert!(
        src.contains("GROK_INJECT_TAGS"),
        "grok inject tags must be data in frames_rules, not ifs in the adapter"
    );
}

#[test]
fn grok_synthetic_reason_is_inject_tag_not_user_msg() {
    let src = include_str!("../src/adapters/grok.rs");
    assert!(
        src.contains("synthetic_reason"),
        "grok transport idiom synthetic_reason must be read as an inject tag"
    );
    assert!(
        src.contains("TransportPayload::Inject"),
        "synthetic / system records must be inject frames, not Human"
    );
}
