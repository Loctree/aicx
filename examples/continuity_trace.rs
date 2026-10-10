//! Read-only selection trace. Prints source metadata and counts, never transcript payloads.
#[cfg(feature = "app")]
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 3,
        "usage: continuity_trace AICX_HOME PROJECT UNTIL [FULL_SESSION_ID ...]"
    );
    let until = args[2].parse::<chrono::DateTime<chrono::Utc>>()?;
    let pack = aicx::continuity::build_with_scope_at(
        std::path::Path::new(&args[0]),
        &[args[1].clone()],
        96,
        false,
        until,
    )?;
    let ids = &args[3..];
    let selection: Vec<_> = pack.selection.iter().filter(|s| ids.is_empty() || ids.contains(&s.session_id)).map(|source| {
        let records: Vec<_> = pack.records.iter().filter(|r| r.agent == source.agent && r.session_id == source.session_id).collect();
        let source_position = pack.sources.iter().position(|s| s.agent == source.agent && s.path == source.path);
        serde_json::json!({"selection":source,"retained_records":records.len(),"source_position":source_position,"source_shown":source_position.is_some_and(|n|n<20),
            "mixed_withheld":pack.mixed_scope.iter().any(|s|s.agent==source.agent&&s.session_id==source.session_id),
            "record_provenance":records.iter().map(|r| serde_json::json!({"timestamp":r.timestamp,"date":r.date,"kind":r.kind,"provenance":r.provenance,"source":r.source_chunk})).collect::<Vec<_>>()})
    }).collect();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"project":pack.project_label,"until":pack.window_end,"source_errors":pack.source_errors,"candidate_cap":pack.candidate_cap,"dropped_candidates":pack.dropped_candidates,"dropped_task_events":pack.dropped_task_events,"requested_ids":ids,"selection":selection})
        )?
    );
    Ok(())
}
#[cfg(not(feature = "app"))]
fn main() {
    eprintln!("continuity_trace requires the app feature");
}
