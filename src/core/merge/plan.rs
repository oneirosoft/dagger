use std::io;

use crate::core::git;
use crate::core::graph::BranchGraph;
use crate::core::restack;
use crate::core::store::{dig_paths, load_config, load_state};
use crate::core::workflow;

use super::types::{MergeMode, MergeOptions, MergePlan};

pub(crate) fn plan(options: &MergeOptions) -> io::Result<MergePlan> {
    workflow::ensure_no_pending_operation_for_command("merge")?;
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

    let graph = BranchGraph::new(&state);
    let target_branch_name = graph
        .parent_branch_name(node, &config.trunk_branch)
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

    let missing_descendants = graph.missing_local_descendants(node.id)?;
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
        &restack::RestackBaseTarget::local(&target_branch_name),
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
        tree: graph.subtree(node.id)?,
        restack_plan: restack::previews_for_actions(&restack_actions),
    })
}
