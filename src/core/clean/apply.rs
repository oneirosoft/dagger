use std::io;

use crate::core::git;
use crate::core::graph::BranchGraph;
use crate::core::restack;
use crate::core::store::{
    BranchArchiveReason, PendingCleanOperation, PendingOperationKind, PendingOperationState,
    open_initialized, record_branch_archived,
};
use crate::core::workflow::{self, RestackExecutionEvent};

use super::types::{CleanApplyOutcome, CleanEvent, CleanPlan};

pub(crate) fn apply(plan: &CleanPlan) -> io::Result<CleanApplyOutcome> {
    apply_with_reporter(plan, &mut |_| Ok(()))
}

pub(crate) fn apply_with_reporter<F>(
    plan: &CleanPlan,
    reporter: &mut F,
) -> io::Result<CleanApplyOutcome>
where
    F: FnMut(CleanEvent) -> io::Result<()>,
{
    if plan.candidates.is_empty() {
        return Ok(CleanApplyOutcome {
            status: git::success_status()?,
            switched_to_trunk_from: None,
            restored_original_branch: None,
            deleted_branches: Vec::new(),
            restacked_branches: Vec::new(),
            failure_output: None,
            paused: false,
        });
    }

    let mut session = open_initialized("dig is not initialized; run 'dig init' first")?;
    workflow::ensure_ready_for_operation(&session.repo, "clean")?;
    workflow::ensure_no_pending_operation(&session.paths, "clean")?;
    let current_branch = git::current_branch_name()?;
    let original_branch = current_branch.clone();

    let mut switched_to_trunk_from = None;
    if plan.targets_current_branch() && current_branch != session.config.trunk_branch {
        reporter(CleanEvent::SwitchingToTrunk {
            from_branch: current_branch.clone(),
            to_branch: session.config.trunk_branch.clone(),
        })?;
        let checkout = workflow::checkout_branch_if_needed(&session.config.trunk_branch)?;
        if !checkout.status.success() {
            return Ok(CleanApplyOutcome {
                status: checkout.status,
                switched_to_trunk_from: None,
                restored_original_branch: None,
                deleted_branches: Vec::new(),
                restacked_branches: Vec::new(),
                failure_output: None,
                paused: false,
            });
        }

        reporter(CleanEvent::SwitchedToTrunk {
            from_branch: current_branch.clone(),
            to_branch: session.config.trunk_branch.clone(),
        })?;
        switched_to_trunk_from = checkout.switched_from;
    }

    let mut deleted_branches = Vec::new();
    let mut restacked_branches = Vec::new();
    let mut last_status = git::success_status()?;
    for (index, candidate) in plan.candidates.iter().enumerate() {
        if let Some(outcome) = process_clean_candidate(
            &mut session,
            &original_branch,
            switched_to_trunk_from.clone(),
            candidate.branch_name.clone(),
            plan.candidates[index + 1..]
                .iter()
                .map(|candidate| candidate.branch_name.clone())
                .collect(),
            &mut deleted_branches,
            &mut restacked_branches,
            reporter,
        )? {
            return Ok(outcome);
        }

        last_status = git::success_status()?;
    }

    let checkout = workflow::checkout_branch_if_needed(&session.config.trunk_branch)?;
    if checkout.switched_from.is_some() {
        if !checkout.status.success() {
            return Ok(CleanApplyOutcome {
                status: checkout.status,
                switched_to_trunk_from,
                restored_original_branch: None,
                deleted_branches,
                restacked_branches,
                failure_output: None,
                paused: false,
            });
        }

        last_status = checkout.status;
    }

    let mut restored_original_branch = None;
    if let Some(outcome) = workflow::restore_original_branch_if_needed(&original_branch)? {
        if !outcome.status.success() {
            return Ok(CleanApplyOutcome {
                status: outcome.status,
                switched_to_trunk_from,
                restored_original_branch: None,
                deleted_branches,
                restacked_branches,
                failure_output: None,
                paused: false,
            });
        }

        restored_original_branch = Some(outcome.restored_branch);
        last_status = outcome.status;
    } else if original_branch == session.config.trunk_branch {
        restored_original_branch = checkout
            .switched_from
            .as_ref()
            .map(|_| original_branch.clone());
    }

    Ok(CleanApplyOutcome {
        status: last_status,
        switched_to_trunk_from,
        restored_original_branch,
        deleted_branches,
        restacked_branches,
        failure_output: None,
        paused: false,
    })
}

pub(crate) fn resume_after_sync(
    pending_operation: PendingOperationState,
    payload: PendingCleanOperation,
) -> io::Result<CleanApplyOutcome> {
    let mut session = open_initialized("dig is not initialized; run 'dig init' first")?;
    let mut deleted_branches = payload.deleted_branches;
    let mut restacked_branches = payload.restacked_branches;

    let restack_outcome = workflow::continue_resumable_restack_operation(
        &mut session,
        pending_operation,
        &mut |_| Ok(()),
    )?;
    restacked_branches.extend(restack_outcome.restacked_branches.clone());

    if restack_outcome.paused {
        return Ok(CleanApplyOutcome {
            status: restack_outcome.status,
            switched_to_trunk_from: payload.switched_to_trunk_from,
            restored_original_branch: None,
            deleted_branches,
            restacked_branches,
            failure_output: restack_outcome.failure_output,
            paused: true,
        });
    }

    delete_clean_candidate(
        &mut session,
        &payload.current_candidate_branch_name,
        &mut |_| Ok(()),
    )?;
    deleted_branches.push(payload.current_candidate_branch_name.clone());

    for index in 0..payload.remaining_branch_names.len() {
        let branch_name = payload.remaining_branch_names[index].clone();
        let remaining_branch_names = payload.remaining_branch_names[index + 1..].to_vec();

        if let Some(outcome) = process_clean_candidate(
            &mut session,
            &payload.original_branch,
            payload.switched_to_trunk_from.clone(),
            branch_name,
            remaining_branch_names,
            &mut deleted_branches,
            &mut restacked_branches,
            &mut |_| Ok(()),
        )? {
            return Ok(outcome);
        }
    }

    let mut restored_original_branch = None;
    let mut status = restack_outcome.status;
    let checkout = workflow::checkout_branch_if_needed(&payload.trunk_branch)?;
    if checkout.switched_from.is_some() {
        if !checkout.status.success() {
            return Ok(CleanApplyOutcome {
                status: checkout.status,
                switched_to_trunk_from: payload.switched_to_trunk_from,
                restored_original_branch: None,
                deleted_branches,
                restacked_branches,
                failure_output: None,
                paused: false,
            });
        }

        status = checkout.status;
    }

    if let Some(outcome) = workflow::restore_original_branch_if_needed(&payload.original_branch)? {
        if !outcome.status.success() {
            return Ok(CleanApplyOutcome {
                status: outcome.status,
                switched_to_trunk_from: payload.switched_to_trunk_from,
                restored_original_branch: None,
                deleted_branches,
                restacked_branches,
                failure_output: None,
                paused: false,
            });
        }

        restored_original_branch = Some(outcome.restored_branch);
        status = outcome.status;
    } else if payload.original_branch == payload.trunk_branch {
        restored_original_branch = checkout
            .switched_from
            .as_ref()
            .map(|_| payload.original_branch.clone());
    }

    Ok(CleanApplyOutcome {
        status,
        switched_to_trunk_from: payload.switched_to_trunk_from,
        restored_original_branch,
        deleted_branches,
        restacked_branches,
        failure_output: None,
        paused: false,
    })
}

fn process_clean_candidate<F>(
    session: &mut crate::core::store::StoreSession,
    original_branch: &str,
    switched_to_trunk_from: Option<String>,
    branch_name: String,
    remaining_branch_names: Vec<String>,
    deleted_branches: &mut Vec<String>,
    restacked_branches: &mut Vec<crate::core::restack::RestackPreview>,
    reporter: &mut F,
) -> io::Result<Option<CleanApplyOutcome>>
where
    F: FnMut(CleanEvent) -> io::Result<()>,
{
    let node = session
        .state
        .find_branch_by_name(&branch_name)
        .cloned()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("tracked branch '{}' was not found", branch_name),
            )
        })?;

    let Some(parent_branch_name) =
        BranchGraph::new(&session.state).parent_branch_name(&node, &session.config.trunk_branch)
    else {
        return Err(io::Error::other(format!(
            "tracked parent for '{}' is missing from dig",
            node.branch_name
        )));
    };

    let restack_actions = restack::plan_after_branch_detach(
        &session.state,
        node.id,
        &node.branch_name,
        &parent_branch_name,
        &node.parent,
    )?;
    let restack_outcome = workflow::execute_resumable_restack_operation(
        session,
        PendingOperationKind::Clean(PendingCleanOperation {
            trunk_branch: session.config.trunk_branch.clone(),
            original_branch: original_branch.to_string(),
            switched_to_trunk_from: switched_to_trunk_from.clone(),
            current_candidate_branch_name: node.branch_name.clone(),
            remaining_branch_names,
            deleted_branches: deleted_branches.clone(),
            restacked_branches: restacked_branches.clone(),
        }),
        &restack_actions,
        &mut |event| match event {
            RestackExecutionEvent::Started(action) => reporter(CleanEvent::RebaseStarted {
                branch_name: action.branch_name.clone(),
                onto_branch: action.new_base_branch_name.clone(),
            }),
            RestackExecutionEvent::Progress { action, progress } => {
                reporter(CleanEvent::RebaseProgress {
                    branch_name: action.branch_name.clone(),
                    onto_branch: action.new_base_branch_name.clone(),
                    current_commit: progress.current,
                    total_commits: progress.total,
                })
            }
            RestackExecutionEvent::Completed(action) => reporter(CleanEvent::RebaseCompleted {
                branch_name: action.branch_name.clone(),
                onto_branch: action.new_base_branch_name.clone(),
            }),
        },
    )?;

    restacked_branches.extend(restack_outcome.restacked_branches.clone());
    if restack_outcome.paused {
        return Ok(Some(CleanApplyOutcome {
            status: restack_outcome.status,
            switched_to_trunk_from,
            restored_original_branch: None,
            deleted_branches: deleted_branches.clone(),
            restacked_branches: restacked_branches.clone(),
            failure_output: restack_outcome.failure_output,
            paused: true,
        }));
    }

    let status = delete_clean_candidate(session, &node.branch_name, reporter)?;
    if !status.success() {
        return Ok(Some(CleanApplyOutcome {
            status,
            switched_to_trunk_from,
            restored_original_branch: None,
            deleted_branches: deleted_branches.clone(),
            restacked_branches: restacked_branches.clone(),
            failure_output: None,
            paused: false,
        }));
    }

    deleted_branches.push(node.branch_name);

    Ok(None)
}

fn delete_clean_candidate<F>(
    session: &mut crate::core::store::StoreSession,
    branch_name: &str,
    reporter: &mut F,
) -> io::Result<std::process::ExitStatus>
where
    F: FnMut(CleanEvent) -> io::Result<()>,
{
    let node = session
        .state
        .find_branch_by_name(branch_name)
        .cloned()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("tracked branch '{}' was not found", branch_name),
            )
        })?;
    let Some(parent_branch_name) =
        BranchGraph::new(&session.state).parent_branch_name(&node, &session.config.trunk_branch)
    else {
        return Err(io::Error::other(format!(
            "tracked parent for '{}' is missing from dig",
            node.branch_name
        )));
    };

    reporter(CleanEvent::DeleteStarted {
        branch_name: node.branch_name.clone(),
    })?;
    let status = git::delete_branch_force(&node.branch_name)?;
    if !status.success() {
        return Ok(status);
    }

    record_branch_archived(
        session,
        node.id,
        node.branch_name.clone(),
        BranchArchiveReason::IntegratedIntoParent {
            parent_branch: parent_branch_name,
        },
    )?;

    reporter(CleanEvent::DeleteCompleted {
        branch_name: node.branch_name,
    })?;

    Ok(status)
}
