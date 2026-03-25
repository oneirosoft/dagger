use std::process::ExitStatus;

use uuid::Uuid;

use crate::core::graph::BranchTreeNode;
use crate::core::restack::RestackPreview;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MergeMode {
    Normal,
    Squash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergeOptions {
    pub branch_name: String,
    pub mode: MergeMode,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MergePlan {
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
    pub(crate) fn requires_target_checkout(&self) -> bool {
        self.current_branch != self.target_branch_name
    }
}

pub(crate) type MergeTreeNode = BranchTreeNode;

#[derive(Debug)]
pub(crate) struct MergeOutcome {
    pub status: ExitStatus,
    pub switched_to_target_from: Option<String>,
    pub restacked_branches: Vec<RestackPreview>,
    pub failure_output: Option<String>,
}

#[derive(Debug)]
pub(crate) struct DeleteMergedBranchOutcome {
    pub status: ExitStatus,
    pub deleted_branch_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MergeEvent {
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
