pub(crate) mod adopt;
pub(crate) mod branch;
pub(crate) mod clean;
pub(crate) mod commit;
pub(crate) mod git;
pub(crate) mod init;
pub(crate) mod merge;
pub(crate) mod restack;
pub(crate) mod store;
pub(crate) mod tree;

#[cfg(test)]
pub(crate) fn test_cwd_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};

    static CWD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    CWD_LOCK.get_or_init(|| Mutex::new(()))
}
