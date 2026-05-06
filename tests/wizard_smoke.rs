use std::process::Command;

#[test]
fn wizard_smoke_boots_renders_and_quits() {
    let bin = env!("CARGO_BIN_EXE_aicx");
    let output = Command::new(bin)
        .arg("wizard")
        .arg("--smoke-test")
        .output()
        .expect("run aicx wizard smoke");

    assert!(
        output.status.success(),
        "wizard smoke failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("aicx wizard smoke"),
        "unexpected stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}
