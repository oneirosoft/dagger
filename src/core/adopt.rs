use std::io;
use std::process::ExitStatus;

use uuid::Uuid;

use crate::core::branch;
use crate::core::git;
use crate::core::store::{
    BranchAdoptedEvent, BranchNode, DigEvent, ParentRef, append_event, dig_paths, initialize_store,
    load_config, load_state, now_unix_timestamp_secs, save_state,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptOptions {
    pub branch_name: Option<String>,
    pub parent_branch_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptPlan {
    pub trunk_branch: String,
    pub original_branch: String,
    pub branch_name: String,
    pub parent_branch_name: String,
    pub parent: ParentRef,
    pub old_upstream_oid: String,
    pub requires_rebase: bool,
}

#[derive(Debug)]
pub struct AdoptOutcome {
    pub status: ExitStatus,
    pub branch_name: String,
    pub parent_branch_name: String,
    pub restacked: bool,
    pub restored_original_branch: Option<String>,
    pub failure_output: Option<String>,
}

pub fn plan(options: &AdoptOptions) -> io::Result<AdoptPlan> {
    let repo = git::resolve_repo_context()?;
    let store_paths = dig_paths(&repo.git_dir);
    let original_branch = git::current_branch_name()?;
    initialize_store(&store_paths, &original_branch)?;

    let config =
        load_config(&store_paths)?.ok_or_else(|| io::Error::other("dig config is missing"))?;
    let state = load_state(&store_paths)?;
    let branch_name = resolve_branch_name(&original_branch, options.branch_name.as_deref())?;
    let parent_branch_name = options.parent_branch_name.trim();

    if parent_branch_name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "parent branch name cannot be empty",
        ));
    }

    if branch_name == config.trunk_branch {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot adopt trunk branch '{}'", config.trunk_branch),
        ));
    }

    if branch_name == parent_branch_name {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "branch cannot list itself as its parent",
        ));
    }

    if !git::branch_exists(&branch_name)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("branch '{}' does not exist", branch_name),
        ));
    }

    if state.find_branch_by_name(&branch_name).is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("branch '{}' is already tracked by dig", branch_name),
        ));
    }

    if !git::branch_exists(parent_branch_name)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("parent branch '{}' does not exist", parent_branch_name),
        ));
    }

    let parent = branch::resolve_parent_ref(&state, &config, parent_branch_name)?;
    let old_upstream_oid = git::merge_base(parent_branch_name, &branch_name)?;
    let parent_head_oid = git::ref_oid(parent_branch_name)?;

    Ok(AdoptPlan {
        trunk_branch: config.trunk_branch,
        original_branch,
        branch_name,
        parent_branch_name: parent_branch_name.to_string(),
        parent,
        old_upstream_oid: old_upstream_oid.clone(),
        requires_rebase: old_upstream_oid != parent_head_oid,
    })
}

pub fn apply(plan: &AdoptPlan) -> io::Result<AdoptOutcome> {
    let repo = git::resolve_repo_context()?;
    git::ensure_clean_worktree("adopt")?;
    git::ensure_no_in_progress_operations(&repo, "adopt")?;

    let store_paths = dig_paths(&repo.git_dir);
    let config =
        load_config(&store_paths)?.ok_or_else(|| io::Error::other("dig is not initialized"))?;
    let mut state = load_state(&store_paths)?;

    if state.find_branch_by_name(&plan.branch_name).is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("branch '{}' is already tracked by dig", plan.branch_name),
        ));
    }

    let resolved_parent = branch::resolve_parent_ref(&state, &config, &plan.parent_branch_name)?;
    if resolved_parent != plan.parent {
        return Err(io::Error::other(format!(
            "tracked parent for '{}' changed while planning adopt",
            plan.parent_branch_name
        )));
    }

    let mut status = git::success_status()?;
    let mut restacked = false;

    if plan.requires_rebase {
        let rebase_output = git::rebase_onto_with_progress(
            &plan.parent_branch_name,
            &plan.old_upstream_oid,
            &plan.branch_name,
            |_| Ok(()),
        )?;
        status = rebase_output.status;

        if !status.success() {
            abort_rebase_if_needed(&repo)?;
            let restored_original_branch =
                restore_original_branch_if_needed(&plan.original_branch)?;

            return Ok(AdoptOutcome {
                status,
                branch_name: plan.branch_name.clone(),
                parent_branch_name: plan.parent_branch_name.clone(),
                restacked: false,
                restored_original_branch,
                failure_output: Some(rebase_output.stderr),
            });
        }

        restacked = true;
    }

    let parent_head_oid = git::ref_oid(&plan.parent_branch_name)?;
    let branch_head_oid = git::ref_oid(&plan.branch_name)?;
    let adopted_node = BranchNode {
        id: Uuid::new_v4(),
        branch_name: plan.branch_name.clone(),
        parent: plan.parent.clone(),
        base_ref: plan.parent_branch_name.clone(),
        fork_point_oid: parent_head_oid,
        head_oid_at_creation: branch_head_oid,
        created_at_unix_secs: now_unix_timestamp_secs(),
        archived: false,
    };

    state.insert_branch(adopted_node.clone())?;
    save_state(&store_paths, &state)?;
    append_event(
        &store_paths,
        &DigEvent::BranchAdopted(BranchAdoptedEvent {
            occurred_at_unix_secs: now_unix_timestamp_secs(),
            node: adopted_node,
        }),
    )?;

    let restored_original_branch = restore_original_branch_if_needed(&plan.original_branch)?;

    Ok(AdoptOutcome {
        status,
        branch_name: plan.branch_name.clone(),
        parent_branch_name: plan.parent_branch_name.clone(),
        restacked,
        restored_original_branch,
        failure_output: None,
    })
}

fn resolve_branch_name(
    original_branch: &str,
    requested_branch_name: Option<&str>,
) -> io::Result<String> {
    let branch_name = requested_branch_name.unwrap_or(original_branch).trim();

    if branch_name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "branch name cannot be empty",
        ));
    }

    Ok(branch_name.to_string())
}

fn abort_rebase_if_needed(repo: &git::RepoContext) -> io::Result<()> {
    if !repo.git_dir.join("rebase-merge").exists()
        && !repo.git_dir.join("rebase-apply").exists()
        && !repo.git_dir.join("REBASE_HEAD").exists()
    {
        return Ok(());
    }

    let status = git::abort_rebase()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            "git rebase failed and 'git rebase --abort' did not succeed",
        ))
    }
}

fn restore_original_branch_if_needed(original_branch: &str) -> io::Result<Option<String>> {
    let current_branch = git::current_branch_name_if_any()?;
    if current_branch.as_deref() == Some(original_branch) {
        return Ok(None);
    }

    if !git::branch_exists(original_branch)? {
        return Ok(None);
    }

    let status = git::switch_branch(original_branch)?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "adopt completed, but failed to return to '{}'",
            original_branch
        )));
    }

    Ok(Some(original_branch.to_string()))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::io;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::Path;
    use std::process::Command;

    use uuid::Uuid;

    use super::{AdoptOptions, apply, plan, resolve_branch_name};
    use crate::core::branch::{self, BranchOptions};
    use crate::core::git;
    use crate::core::store::{DigEvent, ParentRef, dig_paths, load_state};

    #[test]
    fn resolves_requested_branch_or_current_branch() {
        assert_eq!(
            resolve_branch_name("feat/current", Some("feat/other")).unwrap(),
            "feat/other"
        );
        assert_eq!(
            resolve_branch_name("feat/current", None).unwrap(),
            "feat/current"
        );
    }

    #[test]
    fn rejects_tracked_branch_adoption() {
        with_temp_repo(|repo| {
            initialize_main_repo(repo);
            create_tracked_branch("feat/auth");

            let error = plan(&AdoptOptions {
                branch_name: Some("feat/auth".into()),
                parent_branch_name: "main".into(),
            })
            .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(
                error.to_string(),
                "branch 'feat/auth' is already tracked by dig"
            );
        });
    }

    #[test]
    fn rejects_adopting_trunk_branch() {
        with_temp_repo(|repo| {
            initialize_main_repo(repo);

            let error = plan(&AdoptOptions {
                branch_name: Some("main".into()),
                parent_branch_name: "main".into(),
            })
            .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(error.to_string(), "cannot adopt trunk branch 'main'");
        });
    }

    #[test]
    fn rejects_untracked_parent_branch() {
        with_temp_repo(|repo| {
            initialize_main_repo(repo);
            git_ok(repo, &["checkout", "-b", "feat/child"]);
            git_ok(repo, &["checkout", "main"]);

            let error = plan(&AdoptOptions {
                branch_name: Some("feat/child".into()),
                parent_branch_name: "feat/parent".into(),
            })
            .unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            assert_eq!(
                error.to_string(),
                "parent branch 'feat/parent' does not exist"
            );
        });
    }

    #[test]
    fn plans_rebase_for_sibling_branch_adoption() {
        with_temp_repo(|repo| {
            initialize_main_repo(repo);
            create_tracked_branch("feat/auth");
            commit_file(repo, "auth.txt", "auth\n", "feat: auth");
            git_ok(repo, &["checkout", "main"]);
            git_ok(repo, &["checkout", "-b", "feat/auth-ui"]);
            commit_file(repo, "ui.txt", "ui\n", "feat: auth ui");
            git_ok(repo, &["checkout", "feat/auth"]);

            let plan = plan(&AdoptOptions {
                branch_name: Some("feat/auth-ui".into()),
                parent_branch_name: "feat/auth".into(),
            })
            .unwrap();

            assert!(plan.requires_rebase);
            assert_eq!(plan.original_branch, "feat/auth");
            assert_eq!(plan.branch_name, "feat/auth-ui");
            assert_eq!(plan.parent_branch_name, "feat/auth");
        });
    }

    #[test]
    fn adopts_branch_and_records_post_adopt_metadata() {
        with_temp_repo(|repo| {
            initialize_main_repo(repo);
            create_tracked_branch("feat/auth");
            commit_file(repo, "auth.txt", "auth\n", "feat: auth");
            git_ok(repo, &["checkout", "main"]);
            git_ok(repo, &["checkout", "-b", "feat/auth-ui"]);
            commit_file(repo, "ui.txt", "ui\n", "feat: auth ui");
            git_ok(repo, &["checkout", "feat/auth"]);

            let plan = plan(&AdoptOptions {
                branch_name: Some("feat/auth-ui".into()),
                parent_branch_name: "feat/auth".into(),
            })
            .unwrap();
            let outcome = apply(&plan).unwrap();

            assert!(outcome.status.success());
            assert!(outcome.restacked);
            assert_eq!(
                outcome.restored_original_branch.as_deref(),
                Some("feat/auth")
            );
            assert_eq!(git::current_branch_name().unwrap(), "feat/auth");

            let repo_context = git::resolve_repo_context().unwrap();
            let state = load_state(&dig_paths(&repo_context.git_dir)).unwrap();
            let parent = state.find_branch_by_name("feat/auth").unwrap();
            let adopted = state.find_branch_by_name("feat/auth-ui").unwrap();

            assert_eq!(adopted.parent, ParentRef::Branch { node_id: parent.id });
            assert_eq!(adopted.base_ref, "feat/auth");
            assert_eq!(adopted.fork_point_oid, git::ref_oid("feat/auth").unwrap());
            assert_eq!(
                adopted.head_oid_at_creation,
                git::ref_oid("feat/auth-ui").unwrap()
            );

            let events =
                fs::read_to_string(repo_context.git_dir.join("dig/events.ndjson")).unwrap();
            assert!(events.lines().any(|line| {
                serde_json::from_str::<DigEvent>(line)
                    .map(|event| matches!(event, DigEvent::BranchAdopted(_)))
                    .unwrap_or(false)
            }));
        });
    }

    fn with_temp_repo(test: impl FnOnce(&Path)) {
        let guard = crate::core::test_cwd_lock().lock().unwrap();
        let original_dir = env::current_dir().unwrap();
        let repo_dir = env::temp_dir().join(format!("dig-adopt-{}", Uuid::new_v4()));
        fs::create_dir_all(&repo_dir).unwrap();

        let result = catch_unwind(AssertUnwindSafe(|| {
            env::set_current_dir(&repo_dir).unwrap();
            test(&repo_dir);
        }));

        env::set_current_dir(original_dir).unwrap();
        fs::remove_dir_all(&repo_dir).unwrap();
        drop(guard);

        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    fn initialize_main_repo(repo: &Path) {
        git_ok(repo, &["init", "--quiet"]);
        git_ok(repo, &["checkout", "-b", "main"]);
        git_ok(repo, &["config", "user.name", "Dig Test"]);
        git_ok(repo, &["config", "user.email", "dig@example.com"]);
        git_ok(repo, &["config", "commit.gpgsign", "false"]);
        commit_file(repo, "README.md", "root\n", "chore: init");
    }

    fn create_tracked_branch(branch_name: &str) {
        branch::run(&BranchOptions {
            name: branch_name.into(),
            parent_branch_name: None,
        })
        .unwrap();
    }

    fn commit_file(repo: &Path, file_name: &str, contents: &str, message: &str) {
        fs::write(repo.join(file_name), contents).unwrap();
        git_ok(repo, &["add", file_name]);
        git_ok(
            repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        );
    }

    fn git_ok(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(repo)
            .args(args)
            .status()
            .unwrap();

        assert!(status.success(), "git {:?} failed", args);
    }
}
