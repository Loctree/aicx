fn main() {
    // Windows defaults the main-thread stack to 1 MiB. The aicx clap command
    // tree is large enough that building it during argument parsing overflows
    // that stack on startup — every binary invocation (even `aicx --version`)
    // aborts with "thread 'main' has overflowed its stack" before any command
    // runs, which fails every integration test that shells out to the binary.
    // Raise the linked stack reservation to 8 MiB to match the Unix default.
    // Scoped to the MSVC target; a no-op everywhere else.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS");
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV");
    if target_os.as_deref() == Ok("windows") && target_env.as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
