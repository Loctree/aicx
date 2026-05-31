#[cfg(test)]
pub(super) type TestHook = std::sync::Arc<dyn Fn() + Send + Sync + 'static>;

#[cfg(test)]
pub(super) static STEER_READ_LOCK_HOOK: std::sync::OnceLock<std::sync::Mutex<Option<TestHook>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
pub(super) static STEER_REBUILD_SWAP_HOOK: std::sync::OnceLock<std::sync::Mutex<Option<TestHook>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
pub(super) fn call_steer_read_lock_hook() {
    let hook = STEER_READ_LOCK_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("steer read hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(not(test))]
pub(super) fn call_steer_read_lock_hook() {}

#[cfg(test)]
pub(super) fn call_steer_rebuild_swap_hook() {
    let hook = STEER_REBUILD_SWAP_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("steer rebuild hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(not(test))]
pub(super) fn call_steer_rebuild_swap_hook() {}
