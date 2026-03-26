use std::io;

use crate::core::git::{self, RepoContext};

use super::{
    DaggerConfig, StoreInitialization, dagger_paths, initialize_store, load_config, load_state,
};
use crate::core::store::fs::DaggerPaths;
use crate::core::store::types::DaggerState;

#[derive(Debug, Clone)]
pub struct StoreSession {
    pub repo: RepoContext,
    pub paths: DaggerPaths,
    pub config: DaggerConfig,
    pub state: DaggerState,
}

pub fn open_initialized(missing_message: &str) -> io::Result<StoreSession> {
    let repo = git::resolve_repo_context()?;
    let paths = dagger_paths(&repo.git_dir);
    let config = load_config(&paths)?.ok_or_else(|| io::Error::other(missing_message))?;
    let state = load_state(&paths)?;

    Ok(StoreSession {
        repo,
        paths,
        config,
        state,
    })
}

pub fn open_or_initialize(trunk_branch: &str) -> io::Result<(StoreSession, StoreInitialization)> {
    let repo = git::resolve_repo_context()?;
    let paths = dagger_paths(&repo.git_dir);
    let store_initialization = initialize_store(&paths, trunk_branch)?;
    let config =
        load_config(&paths)?.ok_or_else(|| io::Error::other("dagger config is missing"))?;
    let state = load_state(&paths)?;

    Ok((
        StoreSession {
            repo,
            paths,
            config,
            state,
        },
        store_initialization,
    ))
}
