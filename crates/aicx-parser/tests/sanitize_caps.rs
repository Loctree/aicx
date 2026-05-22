use aicx_parser::sanitize::{
    MAX_VALIDATED_BYTES, SanitizeError, read_line_capped, read_to_string_validated,
};
use std::fs;
use std::io::Cursor;
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
    }

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
