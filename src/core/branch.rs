use std::io;
use std::process::ExitStatus;

use uuid::Uuid;

use crate::core::git;
use crate::core::graph::BranchGraph;
use crate::core::graph::BranchLineageNode;
use crate::core::restack::{self, RestackPreview};
use crate::core::store::types::DaggerState;
use crate::core::store::{
    BranchArchiveReason, BranchDivergenceState, BranchNode, DaggerConfig, ParentRef,
    PendingBranchDeleteOperation, PendingOperationKind, PendingOperationState, StoreSession,
    now_unix_timestamp_secs, open_initialized, open_or_initialize, record_branch_archived,
    record_branch_created,
};
use crate::core::workflow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchOptions {
    Create(CreateBranchOptions),
    Delete(DeleteBranchOptions),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateBranchOptions {
    pub name: String,
    pub parent_branch_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteBranchOptions {
    pub branch_name: String,
}

#[derive(Debug, Clone)]
pub enum BranchOutcome {
    Created(CreateBranchOutcome),
    Deleted(DeleteBranchOutcome),
}

impl BranchOutcome {
    pub fn status(&self) -> ExitStatus {
        match self {
            Self::Created(outcome) => outcome.status,
            Self::Deleted(outcome) => outcome.status,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreateBranchOutcome {
    pub status: ExitStatus,
    pub created_node: Option<BranchNode>,
    pub lineage: Vec<BranchLineageNode>,
}

#[derive(Debug, Clone)]
pub struct DeleteBranchOutcome {
    pub status: ExitStatus,
    pub branch_name: String,
    pub parent_branch_name: String,
    pub restacked_branches: Vec<RestackPreview>,
    pub restored_original_branch: Option<String>,
    pub failure_output: Option<String>,
    pub paused: bool,
}

pub fn run(options: &BranchOptions) -> io::Result<BranchOutcome> {
    match options {
        BranchOptions::Create(options) => create_branch(options).map(BranchOutcome::Created),
        BranchOptions::Delete(options) => delete_branch(options).map(BranchOutcome::Deleted),
    }
}

fn create_branch(options: &CreateBranchOptions) -> io::Result<CreateBranchOutcome> {
    let branch_name = options.name.trim();
    if branch_name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "branch name cannot be empty",
        ));
    }

    if git::branch_exists(branch_name)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("branch '{branch_name}' already exists"),
        ));
    }

    let current_branch = git::current_branch_name()?;
    let (mut session, _) = open_or_initialize(&current_branch)?;

    if session.state.find_branch_by_name(branch_name).is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("branch '{branch_name}' is already tracked by dagger"),
        ));
    }

    let parent_branch_name =
        resolve_parent_branch_name(&current_branch, options.parent_branch_name.as_deref())?;

    if parent_branch_name == branch_name {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "branch cannot list itself as its parent",
        ));
    }

    if !git::branch_exists(&parent_branch_name)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("parent branch '{}' does not exist", parent_branch_name),
        ));
    }

    let parent = resolve_parent_ref(&session.state, &session.config, &parent_branch_name)?;
    let parent_head_oid = git::ref_oid(&parent_branch_name)?;

    let created_node = BranchNode {
        id: Uuid::new_v4(),
        branch_name: branch_name.to_string(),
        parent,
        base_ref: parent_branch_name.clone(),
        fork_point_oid: parent_head_oid.clone(),
        head_oid_at_creation: parent_head_oid.clone(),
        created_at_unix_secs: now_unix_timestamp_secs(),
        divergence_state: BranchDivergenceState::NeverDiverged {
            aligned_head_oid: parent_head_oid,
        },
        pull_request: None,
        archived: false,
    };

    let status = git::create_and_checkout_branch(branch_name, &parent_branch_name)?;

    if !status.success() {
        return Ok(CreateBranchOutcome {
            status,
            created_node: None,
            lineage: vec![BranchLineageNode {
                branch_name: branch_name.to_string(),
                pull_request_number: None,
            }],
        });
    }

    record_branch_created(&mut session, created_node.clone())?;
    let graph = BranchGraph::new(&session.state);

    Ok(CreateBranchOutcome {
        status,
        created_node: Some(created_node),
        lineage: graph.lineage(branch_name, &session.config.trunk_branch),
    })
}

fn delete_branch(options: &DeleteBranchOptions) -> io::Result<DeleteBranchOutcome> {
    workflow::ensure_no_pending_operation_for_command("branch")?;
    let branch_name = resolve_delete_branch_name(&options.branch_name)?;
    let original_branch = git::current_branch_name()?;
    let mut session = open_initialized("dagger is not initialized; run 'dgr init' first")?;
    workflow::ensure_ready_for_operation(&session.repo, "branch")?;
    workflow::ensure_no_pending_operation(&session.paths, "branch")?;

    if branch_name == session.config.trunk_branch {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cannot delete trunk branch '{}'",
                session.config.trunk_branch
            ),
        ));
    }

    if !git::branch_exists(&branch_name)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("branch '{}' does not exist", branch_name),
        ));
    }

    let node = session
        .state
        .find_branch_by_name(&branch_name)
        .cloned()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("branch '{}' is not tracked by dagger", branch_name),
            )
        })?;

    if branch_name == original_branch {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cannot delete checked-out branch '{}'; switch to another branch first",
                branch_name
            ),
        ));
    }

    let graph = BranchGraph::new(&session.state);
    let parent_branch_name = graph
        .parent_branch_name(&node, &session.config.trunk_branch)
        .ok_or_else(|| {
            io::Error::other(format!(
                "tracked parent for '{}' is missing from dagger",
                branch_name
            ))
        })?;

    if !git::branch_exists(&parent_branch_name)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("parent branch '{}' does not exist", parent_branch_name),
        ));
    }

    let missing_descendants = graph.missing_local_descendants(node.id)?;
    if !missing_descendants.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "tracked descendants of '{}' are missing locally: {}",
                branch_name,
                missing_descendants.join(", ")
            ),
        ));
    }

    let restack_actions = restack::plan_after_branch_detach(
        &session.state,
        node.id,
        &node.branch_name,
        &restack::RestackBaseTarget::local(&parent_branch_name),
        &node.parent,
    )?;
    let restack_outcome = workflow::execute_resumable_restack_operation(
        &mut session,
        PendingOperationKind::BranchDelete(PendingBranchDeleteOperation {
            original_branch: original_branch.clone(),
            branch_name: branch_name.clone(),
            parent_branch_name: parent_branch_name.clone(),
            node_id: node.id,
        }),
        &restack_actions,
        &mut |_| Ok(()),
    )?;

    if restack_outcome.paused {
        return Ok(DeleteBranchOutcome {
            status: restack_outcome.status,
            branch_name,
            parent_branch_name,
            restacked_branches: restack_outcome.restacked_branches,
            restored_original_branch: None,
            failure_output: restack_outcome.failure_output,
            paused: true,
        });
    }

    complete_delete(
        &mut session,
        node.id,
        &branch_name,
        &parent_branch_name,
        &original_branch,
        restack_outcome.restacked_branches,
        restack_outcome.status,
    )
}

pub(crate) fn resume_delete_after_sync(
    pending_operation: PendingOperationState,
    payload: PendingBranchDeleteOperation,
) -> io::Result<DeleteBranchOutcome> {
    let mut session = open_initialized("dagger is not initialized; run 'dgr init' first")?;
    let restack_outcome = workflow::continue_resumable_restack_operation(
        &mut session,
        pending_operation,
        &mut |_| Ok(()),
    )?;

    if restack_outcome.paused {
        return Ok(DeleteBranchOutcome {
            status: restack_outcome.status,
            branch_name: payload.branch_name,
            parent_branch_name: payload.parent_branch_name,
            restacked_branches: restack_outcome.restacked_branches,
            restored_original_branch: None,
            failure_output: restack_outcome.failure_output,
            paused: true,
        });
    }

    complete_delete(
        &mut session,
        payload.node_id,
        &payload.branch_name,
        &payload.parent_branch_name,
        &payload.original_branch,
        restack_outcome.restacked_branches,
        restack_outcome.status,
    )
}

fn complete_delete(
    session: &mut StoreSession,
    node_id: Uuid,
    branch_name: &str,
    parent_branch_name: &str,
    original_branch: &str,
    restacked_branches: Vec<RestackPreview>,
    restack_status: ExitStatus,
) -> io::Result<DeleteBranchOutcome> {
    let node = session
        .state
        .find_branch_by_id(node_id)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "tracked branch was not found"))?;

    let delete_status = git::delete_branch_force(&node.branch_name)?;
    if !delete_status.success() {
        return Ok(DeleteBranchOutcome {
            status: delete_status,
            branch_name: branch_name.to_string(),
            parent_branch_name: parent_branch_name.to_string(),
            restacked_branches,
            restored_original_branch: None,
            failure_output: None,
            paused: false,
        });
    }

    record_branch_archived(
        session,
        node.id,
        node.branch_name,
        BranchArchiveReason::DeletedByUser,
    )?;

    let mut final_status = restack_status;
    let mut restored_original_branch = None;
    let mut failure_output = None;

    if let Some(outcome) = workflow::restore_original_branch_if_needed(original_branch)? {
        if outcome.status.success() {
            restored_original_branch = Some(outcome.restored_branch);
            final_status = outcome.status;
        } else {
            final_status = outcome.status;
            failure_output = Some(format!(
                "branch deleted, but failed to return to '{}'",
                original_branch
            ));
        }
    }

    Ok(DeleteBranchOutcome {
        status: final_status,
        branch_name: branch_name.to_string(),
        parent_branch_name: parent_branch_name.to_string(),
        restacked_branches,
        restored_original_branch,
        failure_output,
        paused: false,
    })
}

fn resolve_delete_branch_name(requested_branch_name: &str) -> io::Result<String> {
    let branch_name = requested_branch_name.trim();

    if branch_name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "branch name cannot be empty",
        ));
    }

    Ok(branch_name.to_string())
}

fn resolve_parent_branch_name(
    current_branch: &str,
    requested_parent_branch: Option<&str>,
) -> io::Result<String> {
    let parent_branch_name = requested_parent_branch.unwrap_or(current_branch).trim();

    if parent_branch_name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "parent branch name cannot be empty",
        ));
    }

    Ok(parent_branch_name.to_string())
}

pub(crate) fn resolve_parent_ref(
    state: &DaggerState,
    config: &DaggerConfig,
    parent_branch_name: &str,
) -> io::Result<ParentRef> {
    if parent_branch_name == config.trunk_branch {
        return Ok(ParentRef::Trunk);
    }

    state
        .find_branch_by_name(parent_branch_name)
        .map(|node| ParentRef::Branch { node_id: node.id })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "parent branch '{}' is not tracked by dagger and does not match trunk '{}'",
                    parent_branch_name, config.trunk_branch
                ),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        BranchOptions, CreateBranchOptions, DeleteBranchOptions, resolve_delete_branch_name,
        resolve_parent_branch_name, resolve_parent_ref,
    };
    use crate::core::store::types::DaggerState;
    use crate::core::store::{BranchDivergenceState, BranchNode, DaggerConfig, ParentRef};
    use uuid::Uuid;

    #[test]
    fn preserves_create_branch_options() {
        let options = BranchOptions::Create(CreateBranchOptions {
            name: "feature/api".into(),
            parent_branch_name: None,
        });

        assert_eq!(
            options,
            BranchOptions::Create(CreateBranchOptions {
                name: "feature/api".into(),
                parent_branch_name: None,
            })
        );
    }

    #[test]
    fn preserves_delete_branch_options() {
        let options = BranchOptions::Delete(DeleteBranchOptions {
            branch_name: "feature/api".into(),
        });

        assert_eq!(
            options,
            BranchOptions::Delete(DeleteBranchOptions {
                branch_name: "feature/api".into(),
            })
        );
    }

    #[test]
    fn rejects_blank_delete_branch_name() {
        assert!(resolve_delete_branch_name(" ").is_err());
    }

    #[test]
    fn resolves_requested_trunk_parent() {
        let state = DaggerState::default();
        let config = DaggerConfig::new("main".into());

        assert_eq!(
            resolve_parent_ref(&state, &config, "main").unwrap(),
            ParentRef::Trunk
        );
    }

    #[test]
    fn resolves_requested_tracked_parent_branch() {
        let parent_id = Uuid::new_v4();
        let state = DaggerState {
            version: crate::core::store::types::DAGGER_STATE_VERSION,
            nodes: vec![BranchNode {
                id: parent_id,
                branch_name: "feature/base".into(),
                parent: ParentRef::Trunk,
                base_ref: "main".into(),
                fork_point_oid: "abc123".into(),
                head_oid_at_creation: "abc123".into(),
                created_at_unix_secs: 1,
                divergence_state: BranchDivergenceState::Unknown,
                pull_request: None,
                archived: false,
            }],
        };
        let config = DaggerConfig::new("main".into());

        assert_eq!(
            resolve_parent_ref(&state, &config, "feature/base").unwrap(),
            ParentRef::Branch { node_id: parent_id }
        );
    }

    #[test]
    fn resolves_parent_branch_name_override() {
        assert_eq!(
            resolve_parent_branch_name("feature/base", Some("main")).unwrap(),
            "main"
        );
    }
}
