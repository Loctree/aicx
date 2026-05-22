use aicx_parser::sanitize::{
    MAX_STATE_JSON_BYTES, MAX_VALIDATED_BYTES, SanitizeError, read_line_capped,
    read_state_json_validated, read_to_string_validated,
};
use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("aicx-parser-{prefix}-{nanos}"))
}

#[test]
fn read_to_string_validated_rejects_file_over_max_bytes() {
    let root = unique_test_dir("sanitize-cap");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("oversized.txt");
    let file = fs::File::create(&path).unwrap();
    file.set_len((MAX_VALIDATED_BYTES + 1) as u64).unwrap();
    drop(file);

    let err = read_to_string_validated(&path).unwrap_err();
    let cap = err
        .downcast_ref::<SanitizeError>()
        .expect("oversized file should return SanitizeError::FileTooLarge");

    match cap {
        SanitizeError::FileTooLarge {
            path: error_path,
            max_bytes,
            actual_bytes,
        } => {
            assert_eq!(error_path, &path.canonicalize().unwrap());
            assert_eq!(*max_bytes, MAX_VALIDATED_BYTES);
            assert_eq!(*actual_bytes, (MAX_VALIDATED_BYTES + 1) as u64);
        }
        other => panic!("expected FileTooLarge, got {:?}", other),
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn read_state_json_validated_accepts_files_above_generic_cap() {
    // PR #6 follow-up regression: long-lived AICX installs grow
    // `state.json` past the generic 8 MiB validated-read cap. The
    // dedicated state reader MUST accept these files up to
    // `MAX_STATE_JSON_BYTES`. Use a payload just over the generic cap so
    // the test is cheap but still proves we're past the old limit.
    let root = unique_test_dir("state-json-large");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("state.json");
    let payload_size = MAX_VALIDATED_BYTES + 64 * 1024;

    let mut file = fs::File::create(&path).unwrap();
    file.write_all(b"{\"padding\":\"").unwrap();
    let chunk = vec![b'x'; 64 * 1024];
    let mut written = b"{\"padding\":\"".len();
    while written + chunk.len() < payload_size - b"\"}".len() {
        file.write_all(&chunk).unwrap();
        written += chunk.len();
    }
    let remaining = payload_size - written - b"\"}".len();
    if remaining > 0 {
        file.write_all(&vec![b'x'; remaining]).unwrap();
    }
    file.write_all(b"\"}").unwrap();
    file.sync_all().unwrap();
    drop(file);

    let contents = read_state_json_validated(&path)
        .expect("state.json above generic 8 MiB cap must still load");
    assert!(contents.starts_with("{\"padding\":\""));
    assert!(contents.ends_with("\"}"));
    assert_eq!(contents.len(), payload_size);
    assert!(contents.len() > MAX_VALIDATED_BYTES);
    assert!(contents.len() < MAX_STATE_JSON_BYTES);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn read_state_json_validated_rejects_file_over_state_cap() {
    // Even the dedicated state cap is enforced — runaway state must fail
    // with a state-specific error so the operator can tell a corrupt
    // file apart from a normal long-lived install.
    let root = unique_test_dir("state-json-over");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("state.json");
    let file = fs::File::create(&path).unwrap();
    file.set_len((MAX_STATE_JSON_BYTES + 1) as u64).unwrap();
    drop(file);

    let err = read_state_json_validated(&path).unwrap_err();
    let cap = err
        .downcast_ref::<SanitizeError>()
        .expect("oversized state should return SanitizeError::StateFileTooLarge");
    match cap {
        SanitizeError::StateFileTooLarge {
            path: error_path,
            max_bytes,
            actual_bytes,
        } => {
            assert_eq!(error_path, &path.canonicalize().unwrap());
            assert_eq!(*max_bytes, MAX_STATE_JSON_BYTES);
            assert_eq!(*actual_bytes, (MAX_STATE_JSON_BYTES + 1) as u64);
        }
        other => panic!("expected StateFileTooLarge, got {:?}", other),
    }
    let display = format!("{cap}");
    assert!(
        display.contains("State file"),
        "state-specific error message must be distinct: {display}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn read_line_capped_skips_to_next_line_after_oversized() {
    let mut reader = Cursor::new(b"aaaaaaaaa\nok\n".to_vec());

    let first = read_line_capped(&mut reader, 4).unwrap().unwrap();
    assert!(first.exceeded);
    assert_eq!(first.line, "aaaa");

    let second = read_line_capped(&mut reader, 4).unwrap().unwrap();
    assert!(!second.exceeded);
    assert_eq!(second.line, "ok\n");
}

#[test]
fn read_line_capped_marks_oversized_when_limit_cuts_inside_utf8() {
    // PR #6 follow-up regression: the cap must not slice a multi-byte
    // UTF-8 sequence in the middle and surface an InvalidData IO error.
    // The reader should back off to the nearest valid UTF-8 boundary
    // and mark the line as `exceeded`, then deliver the next line
    // intact so callers can continue streaming.
    //
    // Sequence layout:
    //   - "źźźź" = four 2-byte chars (8 bytes total)
    //   - "🚀"   = one 4-byte char
    //   - "abc"  = three 1-byte chars
    // The cap below lands mid-rocket on purpose.
    let payload = "źźźź🚀abc\nok\n".as_bytes().to_vec();
    let mut reader = Cursor::new(payload);
    // 8 bytes of "źźźź" + 2 bytes into the 4-byte 🚀 sequence = 10 bytes.
    let first = read_line_capped(&mut reader, 10).unwrap().unwrap();
    assert!(
        first.exceeded,
        "cap landing inside UTF-8 must flag the line as oversized"
    );
    // The reader must back off past the partial rocket and keep only
    // the four valid źźźź characters (8 bytes, exactly).
    assert_eq!(first.line, "źźźź");

    // Next line must still be deliverable without InvalidData.
    let second = read_line_capped(&mut reader, 16).unwrap().unwrap();
    assert!(!second.exceeded);
    assert_eq!(second.line, "ok\n");
}

#[test]
fn read_line_capped_keeps_ndjson_streaming_after_invalid_utf8_line() {
    // Oversized-and-malformed line (truncated UTF-8 sequence at the
    // tail with no newline before EOF mid-record) must not poison the
    // reader for subsequent valid lines. We compose a stream that has
    // a multi-byte char chopped by the cap, followed by a clean line.
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice("ąąąąąąąą".as_bytes()); // 16 bytes total
    payload.push(b'\n');
    payload.extend_from_slice(b"clean\n");
    let mut reader = Cursor::new(payload);

    // Cap chops between byte 1 and byte 2 of the last `ą`, leaving the
    // reader with an in-flight continuation byte.
    let first = read_line_capped(&mut reader, 15).unwrap().unwrap();
    assert!(first.exceeded);
    // Back off to 14 bytes = 7 valid 2-byte chars (`ą` × 7).
    assert_eq!(first.line, "ąąąąąąą");

    let second = read_line_capped(&mut reader, 32).unwrap().unwrap();
    assert!(!second.exceeded);
    assert_eq!(second.line, "clean\n");
}
