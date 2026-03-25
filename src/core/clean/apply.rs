use std::io;

use crate::core::git;
use crate::core::graph::BranchGraph;
use crate::core::restack;
use crate::core::store::{
    BranchArchiveReason, open_initialized, record_branch_archived,
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
        });
    }

    let mut session = open_initialized("dig is not initialized; run 'dig init' first")?;
    workflow::ensure_ready_for_operation(&session.repo, "clean")?;
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

    for candidate in &plan.candidates {
        let Some(node) = session.state.find_branch_by_id(candidate.node_id).cloned() else {
            continue;
        };

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

        let restack_outcome = workflow::apply_restack_actions(
            &mut session,
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
                RestackExecutionEvent::Completed(action) => {
                    reporter(CleanEvent::RebaseCompleted {
                        branch_name: action.branch_name.clone(),
                        onto_branch: action.new_base_branch_name.clone(),
                    })
                }
            },
        )?;
        if !restack_outcome.status.success() {
            return Ok(CleanApplyOutcome {
                status: restack_outcome.status,
                switched_to_trunk_from,
                restored_original_branch: None,
                deleted_branches,
                restacked_branches,
                failure_output: restack_outcome.failure_output,
            });
        }
        restacked_branches.extend(restack_outcome.restacked_branches);

        reporter(CleanEvent::DeleteStarted {
            branch_name: node.branch_name.clone(),
        })?;
        let status = git::delete_branch_force(&node.branch_name)?;
        if !status.success() {
            return Ok(CleanApplyOutcome {
                status,
                switched_to_trunk_from,
                restored_original_branch: None,
                deleted_branches,
                restacked_branches,
                failure_output: None,
            });
        }

        record_branch_archived(
            &mut session,
            node.id,
            node.branch_name.clone(),
            BranchArchiveReason::IntegratedIntoParent {
                parent_branch: parent_branch_name.clone(),
            },
        )?;

        reporter(CleanEvent::DeleteCompleted {
            branch_name: node.branch_name.clone(),
        })?;
        deleted_branches.push(node.branch_name);
        last_status = status;
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
            });
        }

        restored_original_branch = Some(outcome.restored_branch);
        last_status = outcome.status;
    }

    Ok(CleanApplyOutcome {
        status: last_status,
        switched_to_trunk_from,
        restored_original_branch,
        deleted_branches,
        restacked_branches,
        failure_output: None,
    })
}
