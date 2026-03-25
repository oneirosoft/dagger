use crate::core::merge::{MergeEvent, MergePlan, MergeTreeNode};

pub use super::super::operation::AnimationTerminal;
use super::super::operation::{BranchStatus, OperationSection, VisualNode, render_sections};

pub struct MergeAnimation {
    sections: Vec<OperationSection>,
}

impl MergeAnimation {
    pub fn new(plan: &MergePlan) -> Self {
        Self {
            sections: vec![OperationSection {
                root_label: plan.target_branch_name.clone(),
                root: visual_node_from_tree(&plan.tree),
                promote_children_on_deleted_root: true,
            }],
        }
    }

    pub fn apply_event(&mut self, event: &MergeEvent) -> bool {
        match event {
            MergeEvent::SwitchingToTarget { .. } | MergeEvent::SwitchedToTarget { .. } => false,
            MergeEvent::MergeStarted { source_branch, .. } => self
                .find_node_mut(source_branch)
                .map(|node| node.status = BranchStatus::start_in_flight())
                .is_some(),
            MergeEvent::MergeCompleted { source_branch, .. } => self
                .find_node_mut(source_branch)
                .map(|node| node.status = BranchStatus::Succeeded)
                .is_some(),
            MergeEvent::RebaseStarted { branch_name, .. } => self
                .find_node_mut(branch_name)
                .map(|node| node.status = BranchStatus::start_in_flight())
                .is_some(),
            MergeEvent::RebaseProgress {
                branch_name,
                current_commit,
                total_commits,
                ..
            } => self
                .find_node_mut(branch_name)
                .map(|node| {
                    node.status = node
                        .status
                        .advance_progress(*current_commit, *total_commits)
                })
                .is_some(),
            MergeEvent::RebaseCompleted { branch_name, .. } => self
                .find_node_mut(branch_name)
                .map(|node| node.status = BranchStatus::Succeeded)
                .is_some(),
        }
    }

    pub fn render_active(&self) -> String {
        render_sections(&self.sections, false)
    }

    pub fn render_final(&self) -> String {
        render_sections(&self.sections, true)
    }

    fn find_node_mut(&mut self, branch_name: &str) -> Option<&mut VisualNode> {
        for section in &mut self.sections {
            if let Some(node) = section.root.find_mut(branch_name) {
                return Some(node);
            }
        }

        None
    }
}

fn visual_node_from_tree(tree: &MergeTreeNode) -> VisualNode {
    VisualNode::new(
        tree.branch_name.clone(),
        tree.children.iter().map(visual_node_from_tree).collect(),
    )
}
