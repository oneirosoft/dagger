use std::fs;
use std::io;
use std::path::Path;
use std::process::ExitStatus;

use uuid::Uuid;

use crate::core::git::{self, CommitMetadata};
use crate::core::restack::{self, RestackPreview};
use crate::core::store::types::DigState;
use crate::core::store::{
    BranchArchiveReason, BranchArchivedEvent, BranchReparentedEvent, DigEvent, append_event,
    dig_paths, load_config, load_state, now_unix_timestamp_secs, save_state,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMode {
    Normal,
    Squash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOptions {
    pub branch_name: String,
    pub mode: MergeMode,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergePlan {
    pub trunk_branch: String,
    pub current_branch: String,
    pub source_branch_name: String,
    pub target_branch_name: String,
    pub source_node_id: Uuid,
    pub mode: MergeMode,
    pub messages: Vec<String>,
    pub tree: MergeTreeNode,
    pub restack_plan: Vec<RestackPreview>,
}

impl MergePlan {
    pub fn requires_target_checkout(&self) -> bool {
        self.current_branch != self.target_branch_name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeTreeNode {
    pub branch_name: String,
    pub children: Vec<MergeTreeNode>,
}

#[derive(Debug)]
pub struct MergeOutcome {
    pub status: ExitStatus,
    pub switched_to_target_from: Option<String>,
    pub restacked_branches: Vec<RestackPreview>,
    pub failure_output: Option<String>,
}

#[derive(Debug)]
pub struct DeleteMergedBranchOutcome {
    pub status: ExitStatus,
    pub deleted_branch_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeEvent {
    SwitchingToTarget {
        from_branch: String,
        to_branch: String,
    },
    SwitchedToTarget {
        from_branch: String,
        to_branch: String,
    },
    MergeStarted {
        source_branch: String,
        target_branch: String,
        mode: MergeMode,
    },
    MergeCompleted {
        source_branch: String,
        target_branch: String,
        mode: MergeMode,
    },
    RebaseStarted {
        branch_name: String,
        onto_branch: String,
    },
    RebaseProgress {
        branch_name: String,
        onto_branch: String,
        current_commit: usize,
        total_commits: usize,
    },
    RebaseCompleted {
        branch_name: String,
        onto_branch: String,
    },
}

pub fn plan(options: &MergeOptions) -> io::Result<MergePlan> {
    let branch_name = options.branch_name.trim();
    if branch_name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "branch name cannot be empty",
        ));
    }

    let messages = options
        .messages
        .iter()
        .map(|message| message.trim())
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    if options.mode != MergeMode::Squash && !messages.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--message can only be used with --squash",
        ));
    }

    let repo = git::resolve_repo_context()?;
    let store_paths = dig_paths(&repo.git_dir);
    let config = load_config(&store_paths)?
        .ok_or_else(|| io::Error::other("dig is not initialized; run 'dig init' first"))?;
    let state = load_state(&store_paths)?;
    let current_branch = git::current_branch_name()?;

    let node = state.find_branch_by_name(branch_name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("'{}' is not tracked by dig", branch_name),
        )
    })?;

    if !git::branch_exists(&node.branch_name)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "tracked branch '{}' no longer exists locally",
                node.branch_name
            ),
        ));
    }

    let target_branch_name = state
        .resolve_parent_branch_name(node, &config.trunk_branch)
        .ok_or_else(|| {
            io::Error::other(format!(
                "tracked parent for '{}' is missing from dig",
                node.branch_name
            ))
        })?;

    if !git::branch_exists(&target_branch_name)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "target branch '{}' does not exist locally",
                target_branch_name
            ),
        ));
    }

    let missing_descendants = missing_local_descendants(&state, node.id)?;
    if !missing_descendants.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "tracked descendants are missing locally: {}",
                missing_descendants.join(", ")
            ),
        ));
    }

    let restack_actions = restack::plan_after_branch_detach(
        &state,
        node.id,
        &node.branch_name,
        &target_branch_name,
        &node.parent,
    )?;

    Ok(MergePlan {
        trunk_branch: config.trunk_branch,
        current_branch,
        source_branch_name: node.branch_name.clone(),
        target_branch_name,
        source_node_id: node.id,
        mode: options.mode,
        messages,
        tree: build_merge_tree(&state, node.id)?,
        restack_plan: restack::previews_for_actions(&restack_actions),
    })
}

pub fn apply(plan: &MergePlan) -> io::Result<MergeOutcome> {
    apply_with_reporter(plan, &mut |_| Ok(()))
}

pub fn apply_with_reporter<F>(plan: &MergePlan, reporter: &mut F) -> io::Result<MergeOutcome>
where
    F: FnMut(MergeEvent) -> io::Result<()>,
{
    let repo = git::resolve_repo_context()?;
    git::ensure_clean_worktree("merge")?;
    git::ensure_no_in_progress_operations(&repo, "merge")?;

    let store_paths = dig_paths(&repo.git_dir);
    let config = load_config(&store_paths)?
        .ok_or_else(|| io::Error::other("dig is not initialized; run 'dig init' first"))?;
    let mut state = load_state(&store_paths)?;
    let current_branch = git::current_branch_name()?;
    let source_commits = if plan.mode == MergeMode::Squash {
        git::commit_metadata_in_range(&format!(
            "{}..{}",
            plan.target_branch_name, plan.source_branch_name
        ))?
    } else {
        Vec::new()
    };

    let mut switched_to_target_from = None;
    if current_branch != plan.target_branch_name {
        reporter(MergeEvent::SwitchingToTarget {
            from_branch: current_branch.clone(),
            to_branch: plan.target_branch_name.clone(),
        })?;
        let status = git::switch_branch(&plan.target_branch_name)?;
        if !status.success() {
            return Ok(MergeOutcome {
                status,
                switched_to_target_from: None,
                restacked_branches: Vec::new(),
                failure_output: None,
            });
        }

        reporter(MergeEvent::SwitchedToTarget {
            from_branch: current_branch.clone(),
            to_branch: plan.target_branch_name.clone(),
        })?;
        switched_to_target_from = Some(current_branch);
    }

    reporter(MergeEvent::MergeStarted {
        source_branch: plan.source_branch_name.clone(),
        target_branch: plan.target_branch_name.clone(),
        mode: plan.mode,
    })?;

    let merge_output = match plan.mode {
        MergeMode::Normal => git::merge_branch(&plan.source_branch_name)?,
        MergeMode::Squash => run_squash_merge(plan, &repo.git_dir, &source_commits)?,
    };

    if !merge_output.status.success() {
        return Ok(MergeOutcome {
            status: merge_output.status,
            switched_to_target_from,
            restacked_branches: Vec::new(),
            failure_output: Some(merge_output.combined_output()),
        });
    }

    reporter(MergeEvent::MergeCompleted {
        source_branch: plan.source_branch_name.clone(),
        target_branch: plan.target_branch_name.clone(),
        mode: plan.mode,
    })?;

    let node = state
        .find_branch_by_id(plan.source_node_id)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "tracked branch was not found"))?;
    let restack_actions = restack::plan_after_branch_detach(
        &state,
        node.id,
        &node.branch_name,
        &plan.target_branch_name,
        &node.parent,
    )?;

    let mut restacked_branches = Vec::new();
    let mut last_status = merge_output.status;

    for action in &restack_actions {
        reporter(MergeEvent::RebaseStarted {
            branch_name: action.branch_name.clone(),
            onto_branch: action.new_base_branch_name.clone(),
        })?;
        let outcome = restack::apply_action(&mut state, action, |progress| {
            reporter(MergeEvent::RebaseProgress {
                branch_name: action.branch_name.clone(),
                onto_branch: action.new_base_branch_name.clone(),
                current_commit: progress.current,
                total_commits: progress.total,
            })
        })?;

        if !outcome.status.success() {
            return Ok(MergeOutcome {
                status: outcome.status,
                switched_to_target_from,
                restacked_branches,
                failure_output: Some(outcome.stderr),
            });
        }

        reporter(MergeEvent::RebaseCompleted {
            branch_name: action.branch_name.clone(),
            onto_branch: action.new_base_branch_name.clone(),
        })?;

        if let Some(parent_change) = outcome.parent_change {
            save_state(&store_paths, &state)?;
            append_event(
                &store_paths,
                &DigEvent::BranchReparented(BranchReparentedEvent {
                    occurred_at_unix_secs: now_unix_timestamp_secs(),
                    branch_id: parent_change.branch_id,
                    branch_name: parent_change.branch_name,
                    old_parent: parent_change.old_parent,
                    new_parent: parent_change.new_parent,
                    old_base_ref: parent_change.old_base_ref,
                    new_base_ref: parent_change.new_base_ref,
                }),
            )?;
        }

        restacked_branches.push(RestackPreview {
            branch_name: outcome.branch_name,
            onto_branch: outcome.onto_branch,
            parent_changed: action.new_parent.is_some(),
        });
        last_status = outcome.status;
    }

    let current_branch = git::current_branch_name()?;
    if current_branch != plan.target_branch_name {
        let status = git::switch_branch(&plan.target_branch_name)?;
        if !status.success() {
            return Ok(MergeOutcome {
                status,
                switched_to_target_from,
                restacked_branches,
                failure_output: None,
            });
        }

        last_status = status;
    }

    let _ = config;

    Ok(MergeOutcome {
        status: last_status,
        switched_to_target_from,
        restacked_branches,
        failure_output: None,
    })
}

pub fn delete_merged_branch(plan: &MergePlan) -> io::Result<DeleteMergedBranchOutcome> {
    let repo = git::resolve_repo_context()?;
    let store_paths = dig_paths(&repo.git_dir);
    let mut state = load_state(&store_paths)?;
    let node = state
        .find_branch_by_id(plan.source_node_id)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "tracked branch was not found"))?;

    let status = git::delete_branch_force(&node.branch_name)?;
    if !status.success() {
        return Ok(DeleteMergedBranchOutcome {
            status,
            deleted_branch_name: None,
        });
    }

    state.archive_branch(node.id)?;
    save_state(&store_paths, &state)?;
    append_event(
        &store_paths,
        &DigEvent::BranchArchived(BranchArchivedEvent {
            occurred_at_unix_secs: now_unix_timestamp_secs(),
            branch_id: node.id,
            branch_name: node.branch_name.clone(),
            reason: BranchArchiveReason::IntegratedIntoParent {
                parent_branch: plan.target_branch_name.clone(),
            },
        }),
    )?;

    Ok(DeleteMergedBranchOutcome {
        status,
        deleted_branch_name: Some(node.branch_name),
    })
}

fn run_squash_merge(
    plan: &MergePlan,
    git_dir: &Path,
    source_commits: &[CommitMetadata],
) -> io::Result<git::GitCommandOutput> {
    let merge_output = git::squash_merge_branch(&plan.source_branch_name)?;
    if !merge_output.status.success() {
        return Ok(merge_output);
    }

    if !git::has_staged_changes()? {
        return Ok(merge_output);
    }

    let message = build_squash_commit_message(
        &plan.source_branch_name,
        &plan.target_branch_name,
        &plan.messages,
        source_commits,
    );
    let message_path = git_dir.join("DIG_MERGE_MSG");
    fs::write(&message_path, message)?;
    let commit_output = git::commit_with_message_file(&message_path);
    let remove_result = fs::remove_file(&message_path);
    let commit_output = commit_output?;
    if let Err(err) = remove_result {
        return Err(err);
    }

    Ok(commit_output)
}

fn build_squash_commit_message(
    source_branch_name: &str,
    target_branch_name: &str,
    messages: &[String],
    source_commits: &[CommitMetadata],
) -> String {
    let mut sections = Vec::new();

    if messages.is_empty() {
        sections.push(format!(
            "merge {} into {}",
            source_branch_name, target_branch_name
        ));
    } else {
        sections.push(messages.join("\n\n"));
    }

    if !source_commits.is_empty() {
        let commit_listing = source_commits
            .iter()
            .map(|commit| format!("commit {}\n    {}", commit.sha, commit.subject))
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.push(commit_listing);
    }

    sections.join("\n\n")
}

fn build_merge_tree(state: &DigState, node_id: Uuid) -> io::Result<MergeTreeNode> {
    let node = state
        .find_branch_by_id(node_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "tracked branch was not found"))?;

    let mut children = Vec::new();
    for child_id in state.active_children_ids(node_id) {
        children.push(build_merge_tree(state, child_id)?);
    }

    Ok(MergeTreeNode {
        branch_name: node.branch_name.clone(),
        children,
    })
}

fn missing_local_descendants(state: &DigState, node_id: Uuid) -> io::Result<Vec<String>> {
    let mut missing = Vec::new();

    for descendant_id in state.active_descendant_ids(node_id) {
        let Some(descendant) = state.find_branch_by_id(descendant_id) else {
            continue;
        };

        if !git::branch_exists(&descendant.branch_name)? {
            missing.push(descendant.branch_name.clone());
        }
    }

    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::{MergeMode, MergeOptions, build_squash_commit_message, delete_merged_branch, plan};
    use crate::core::branch::{self, BranchOptions};
    use crate::core::git;
    use crate::core::store::{ParentRef, dig_paths, load_state};
    use std::env;
    use std::fs;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::Path;
    use std::process::Command;
    use std::sync::MutexGuard;
    use uuid::Uuid;

    #[test]
    fn builds_default_squash_commit_message_with_commit_listing() {
        let message = build_squash_commit_message(
            "feat/auth",
            "main",
            &[],
            &[
                crate::core::git::CommitMetadata {
                    sha: "abc123".into(),
                    subject: "feat: auth".into(),
                    body: String::new(),
                },
                crate::core::git::CommitMetadata {
                    sha: "def456".into(),
                    subject: "feat: auth api".into(),
                    body: String::new(),
                },
            ],
        );

        assert_eq!(
            message,
            concat!(
                "merge feat/auth into main\n\n",
                "commit abc123\n",
                "    feat: auth\n\n",
                "commit def456\n",
                "    feat: auth api"
            )
        );
    }

    #[test]
    fn appends_commit_listing_after_user_supplied_squash_message() {
        let message = build_squash_commit_message(
            "feat/auth",
            "main",
            &["custom subject".into(), "extra context".into()],
            &[crate::core::git::CommitMetadata {
                sha: "abc123".into(),
                subject: "feat: auth".into(),
                body: String::new(),
            }],
        );

        assert_eq!(
            message,
            concat!(
                "custom subject\n\n",
                "extra context\n\n",
                "commit abc123\n",
                "    feat: auth"
            )
        );
    }

    #[test]
    fn merges_child_into_parent_and_restacks_descendants() {
        with_temp_repo(|repo| {
            initialize_main_repo(repo);
            create_tracked_branch("feat/auth");
            commit_file(repo, "auth.txt", "auth\n", "feat: auth");
            create_tracked_branch("feat/auth-api");
            commit_file(repo, "auth-api.txt", "api\n", "feat: auth api");
            create_tracked_branch("feat/auth-api-tests");
            commit_file(
                repo,
                "auth-api-tests.txt",
                "tests\n",
                "feat: auth api tests",
            );

            git_ok(repo, &["checkout", "feat/auth-api"]);

            let merge_plan = plan(&MergeOptions {
                branch_name: "feat/auth-api".into(),
                mode: MergeMode::Normal,
                messages: vec![],
            })
            .unwrap();

            let outcome = super::apply(&merge_plan).unwrap();

            assert!(outcome.status.success());
            assert_eq!(
                outcome.switched_to_target_from.as_deref(),
                Some("feat/auth-api")
            );
            assert_eq!(
                outcome
                    .restacked_branches
                    .iter()
                    .map(|step| format!("{}->{}", step.branch_name, step.onto_branch))
                    .collect::<Vec<_>>(),
                vec!["feat/auth-api-tests->feat/auth".to_string()]
            );
            assert_eq!(git::current_branch_name().unwrap(), "feat/auth");

            let state =
                load_state(&dig_paths(&git::resolve_repo_context().unwrap().git_dir)).unwrap();
            let restacked_child = state.find_branch_by_name("feat/auth-api-tests").unwrap();
            assert_eq!(
                restacked_child.parent,
                ParentRef::Branch {
                    node_id: state.find_branch_by_name("feat/auth").unwrap().id
                }
            );
            assert_eq!(restacked_child.base_ref, "feat/auth");

            let delete_outcome = delete_merged_branch(&merge_plan).unwrap();
            assert!(delete_outcome.status.success());
            assert!(!git::branch_exists("feat/auth-api").unwrap());
        });
    }

    #[test]
    fn squash_merges_into_trunk_and_keeps_branch_when_delete_is_declined() {
        with_temp_repo(|repo| {
            initialize_main_repo(repo);
            create_tracked_branch("feat/auth");
            commit_file(repo, "auth.txt", "auth\n", "feat: auth");
            append_file(
                repo,
                "auth.txt",
                "auth second line\n",
                "feat: auth follow-up",
            );
            create_tracked_branch("feat/auth-api");
            commit_file(repo, "auth-api.txt", "api\n", "feat: auth api");

            git_ok(repo, &["checkout", "feat/auth"]);

            let merge_plan = plan(&MergeOptions {
                branch_name: "feat/auth".into(),
                mode: MergeMode::Squash,
                messages: vec!["custom merge".into()],
            })
            .unwrap();

            let outcome = super::apply(&merge_plan).unwrap();

            assert!(outcome.status.success());
            assert_eq!(
                outcome.switched_to_target_from.as_deref(),
                Some("feat/auth")
            );
            assert_eq!(
                outcome
                    .restacked_branches
                    .iter()
                    .map(|step| format!("{}->{}", step.branch_name, step.onto_branch))
                    .collect::<Vec<_>>(),
                vec!["feat/auth-api->main".to_string()]
            );
            assert_eq!(git::current_branch_name().unwrap(), "main");
            assert!(git::branch_exists("feat/auth").unwrap());

            let log_message = git_output(repo, &["log", "-1", "--format=%B"]);
            assert!(log_message.contains("custom merge"));
            assert!(log_message.contains("commit "));
            assert!(log_message.contains("feat: auth"));

            let state =
                load_state(&dig_paths(&git::resolve_repo_context().unwrap().git_dir)).unwrap();
            let restacked_child = state.find_branch_by_name("feat/auth-api").unwrap();
            assert_eq!(restacked_child.parent, ParentRef::Trunk);
            assert_eq!(restacked_child.base_ref, "main");
            assert!(state.find_branch_by_name("feat/auth").is_some());
        });
    }

    #[test]
    fn blocks_merge_when_tracked_descendant_is_missing_locally() {
        with_temp_repo(|repo| {
            initialize_main_repo(repo);
            create_tracked_branch("feat/auth");
            commit_file(repo, "auth.txt", "auth\n", "feat: auth");
            create_tracked_branch("feat/auth-api");
            commit_file(repo, "auth-api.txt", "api\n", "feat: auth api");

            git_ok(repo, &["checkout", "main"]);
            git_ok(repo, &["branch", "-D", "feat/auth-api"]);

            let error = plan(&MergeOptions {
                branch_name: "feat/auth".into(),
                mode: MergeMode::Normal,
                messages: vec![],
            })
            .unwrap_err();

            assert!(error.to_string().contains("missing locally"));
        });
    }

    #[test]
    fn leaves_state_unchanged_when_merge_conflicts() {
        with_temp_repo(|repo| {
            initialize_main_repo(repo);
            create_tracked_branch("feat/auth");
            commit_file(repo, "shared.txt", "source branch\n", "feat: auth");
            git_ok(repo, &["checkout", "main"]);
            fs::write(repo.join("shared.txt"), "main branch\n").unwrap();
            git_ok(repo, &["add", "shared.txt"]);
            git_ok(
                repo,
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    "feat: main",
                ],
            );

            let merge_plan = plan(&MergeOptions {
                branch_name: "feat/auth".into(),
                mode: MergeMode::Normal,
                messages: vec![],
            })
            .unwrap();

            let outcome = super::apply(&merge_plan).unwrap();

            assert!(!outcome.status.success());

            let state =
                load_state(&dig_paths(&git::resolve_repo_context().unwrap().git_dir)).unwrap();
            let node = state.find_branch_by_name("feat/auth").unwrap();
            assert_eq!(node.parent, ParentRef::Trunk);
            assert!(git::branch_exists("feat/auth").unwrap());
            assert_eq!(git::current_branch_name().unwrap(), "main");
        });
    }

    fn with_temp_repo(test: impl FnOnce(&Path)) {
        let guard: MutexGuard<'_, ()> = crate::core::test_cwd_lock().lock().unwrap();
        let original_dir = env::current_dir().unwrap();
        let repo_dir = env::temp_dir().join(format!("dig-merge-{}", Uuid::new_v4()));
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

    fn append_file(repo: &Path, file_name: &str, contents: &str, message: &str) {
        let path = repo.join(file_name);
        let mut existing = fs::read_to_string(&path).unwrap();
        existing.push_str(contents);
        fs::write(&path, existing).unwrap();
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

    fn git_output(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();

        assert!(output.status.success(), "git {:?} failed", args);

        String::from_utf8(output.stdout).unwrap()
    }
}
