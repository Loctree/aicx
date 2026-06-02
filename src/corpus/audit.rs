use anyhow::Result;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::corpus::inference::{
    infer_agent, infer_frame_kind, infer_path_date, system_date, system_timestamp,
};
use crate::corpus::io::{markdown_files, validate_optional_root};
use crate::corpus::noise::detect_noise_classes;
use crate::corpus::roots::default_roots;
use crate::corpus::types::{
    CorpusAuditOptions, CorpusAuditReport, CorpusAuditTotals, CorpusFileFinding, RootAuditReport,
};
use crate::sanitize;

pub fn audit(options: &CorpusAuditOptions) -> Result<CorpusAuditReport> {
    let roots = if options.roots.is_empty() {
        default_roots()?
    } else {
        options.roots.clone()
    }
    .into_iter()
    .map(validate_optional_root)
    .collect::<Result<Vec<_>>>()?;

    let mut reports = Vec::new();
    let mut totals = CorpusAuditTotals::default();

    for root in roots {
        let report = audit_root(&root)?;
        if report.present {
            totals.roots_present += 1;
        } else {
            totals.roots_missing += 1;
        }
        totals.markdown_files += report.markdown_files;
        totals.files_with_noise += report.files_with_noise;
        merge_counts(&mut totals.noise_classes, &report.noise_classes);
        merge_counts(&mut totals.agents, &report.agents);
        merge_counts(&mut totals.frame_kinds, &report.frame_kinds);
        merge_counts(&mut totals.path_dates, &report.path_dates);
        reports.push(report);
    }

    Ok(CorpusAuditReport {
        roots: reports,
        totals,
    })
}

fn audit_root(root: &Path) -> Result<RootAuditReport> {
    if !root.is_dir() {
        return Ok(RootAuditReport {
            root: root.to_path_buf(),
            present: false,
            markdown_files: 0,
            files_with_noise: 0,
            noise_classes: BTreeMap::new(),
            agents: BTreeMap::new(),
            frame_kinds: BTreeMap::new(),
            path_dates: BTreeMap::new(),
            artifact_birthtime_dates: BTreeMap::new(),
            artifact_mtime_dates: BTreeMap::new(),
            examples: Vec::new(),
        });
    }

    let mut report = RootAuditReport {
        root: root.to_path_buf(),
        present: true,
        markdown_files: 0,
        files_with_noise: 0,
        noise_classes: BTreeMap::new(),
        agents: BTreeMap::new(),
        frame_kinds: BTreeMap::new(),
        path_dates: BTreeMap::new(),
        artifact_birthtime_dates: BTreeMap::new(),
        artifact_mtime_dates: BTreeMap::new(),
        examples: Vec::new(),
    };

    for path in markdown_files(root)? {
        report.markdown_files += 1;
        let content = sanitize::read_to_string_validated(&path).unwrap_or_default();
        let classes = detect_noise_classes(&content);
        let agent = infer_agent(&path, &content);
        inc(&mut report.agents, agent.clone());
        if let Some(frame_kind) = infer_frame_kind(&path, &content) {
            inc(&mut report.frame_kinds, frame_kind);
        }
        if let Some(path_date) = infer_path_date(&path) {
            inc(&mut report.path_dates, path_date);
        }
        if let Ok(meta) = fs::metadata(&path) {
            if let Ok(created) = meta.created() {
                inc(&mut report.artifact_birthtime_dates, system_date(created));
            }
            if let Ok(modified) = meta.modified() {
                inc(&mut report.artifact_mtime_dates, system_date(modified));
            }
        }

        if !classes.is_empty() {
            report.files_with_noise += 1;
            for class in &classes {
                inc(&mut report.noise_classes, class.as_str().to_string());
            }
            if report.examples.len() < 20 {
                report.examples.push(CorpusFileFinding {
                    path: path.clone(),
                    agent,
                    frame_kind: infer_frame_kind(&path, &content),
                    path_date: infer_path_date(&path),
                    artifact_birthtime: fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.created().ok())
                        .map(system_timestamp),
                    artifact_mtime: fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(system_timestamp),
                    noise_classes: classes.iter().map(|c| c.as_str().to_string()).collect(),
                });
            }
        }
    }

    Ok(report)
}

fn merge_counts(target: &mut BTreeMap<String, usize>, source: &BTreeMap<String, usize>) {
    for (key, value) in source {
        *target.entry(key.clone()).or_default() += value;
    }
}

fn inc(map: &mut BTreeMap<String, usize>, key: String) {
    *map.entry(key).or_default() += 1;
}
