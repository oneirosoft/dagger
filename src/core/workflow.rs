use std::io;
use std::process::ExitStatus;

use crate::core::git::{self, RebaseProgress, RepoContext};
use crate::core::restack::{self, RestackAction, RestackPreview};
use crate::core::store::record_branch_reparented;
use crate::core::store::session::StoreSession;

#[derive(Debug)]
pub(crate) struct CheckoutBranchOutcome {
    pub status: ExitStatus,
    pub switched_from: Option<String>,
}

#[derive(Debug)]
pub(crate) struct RestoreBranchOutcome {
    pub status: ExitStatus,
    pub restored_branch: String,
}

#[derive(Debug)]
pub(crate) struct RestackExecutionOutcome {
    pub status: ExitStatus,
    pub restacked_branches: Vec<RestackPreview>,
    pub failure_output: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RestackExecutionEvent<'a> {
    Started(&'a RestackAction),
    Progress {
        action: &'a RestackAction,
        progress: RebaseProgress,
    },
    Completed(&'a RestackAction),
}

pub(crate) fn ensure_ready_for_operation(
    repo: &RepoContext,
    command_name: &str,
) -> io::Result<()> {
    git::ensure_clean_worktree(command_name)?;
    git::ensure_no_in_progress_operations(repo, command_name)
}

pub(crate) fn checkout_branch_if_needed(target_branch: &str) -> io::Result<CheckoutBranchOutcome> {
    let current_branch = git::current_branch_name_if_any()?;
    if current_branch.as_deref() == Some(target_branch) {
        return Ok(CheckoutBranchOutcome {
            status: git::success_status()?,
            switched_from: None,
        });
    }

    let status = git::switch_branch(target_branch)?;

    Ok(CheckoutBranchOutcome {
        switched_from: status.success().then_some(current_branch).flatten(),
        status,
    })
}

pub(crate) fn restore_original_branch_if_needed(
    original_branch: &str,
) -> io::Result<Option<RestoreBranchOutcome>> {
    let current_branch = git::current_branch_name_if_any()?;
    if current_branch.as_deref() == Some(original_branch) {
        return Ok(None);
    }

    if !git::branch_exists(original_branch)? {
        return Ok(None);
    }

    let status = git::switch_branch(original_branch)?;

    Ok(Some(RestoreBranchOutcome {
        status,
        restored_branch: original_branch.to_string(),
    }))
}

pub(crate) fn apply_restack_actions<F>(
    session: &mut StoreSession,
    actions: &[RestackAction],
    on_event: &mut F,
) -> io::Result<RestackExecutionOutcome>
where
    F: for<'a> FnMut(RestackExecutionEvent<'a>) -> io::Result<()>,
{
    let mut restacked_branches = Vec::new();
    let mut last_status = git::success_status()?;

    for action in actions {
        on_event(RestackExecutionEvent::Started(action))?;

        let outcome = restack::apply_action(&mut session.state, action, |progress| {
            on_event(RestackExecutionEvent::Progress { action, progress })
        })?;

        if !outcome.status.success() {
            return Ok(RestackExecutionOutcome {
                status: outcome.status,
                restacked_branches,
                failure_output: Some(outcome.stderr),
            });
        }

        on_event(RestackExecutionEvent::Completed(action))?;

        if let Some(parent_change) = outcome.parent_change {
            record_branch_reparented(
                session,
                parent_change.branch_id,
                parent_change.branch_name,
                parent_change.old_parent,
                parent_change.new_parent,
                parent_change.old_base_ref,
                parent_change.new_base_ref,
            )?;
        }

        restacked_branches.push(RestackPreview {
            branch_name: outcome.branch_name,
            onto_branch: outcome.onto_branch,
            parent_changed: action.new_parent.is_some(),
        });
        last_status = outcome.status;
    }

    Ok(RestackExecutionOutcome {
        status: last_status,
        restacked_branches,
        failure_output: None,
    })
}
