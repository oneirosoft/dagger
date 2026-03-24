use std::collections::{HashMap, HashSet};
use std::io;
use std::process::ExitStatus;

use uuid::Uuid;

use crate::core::git;
use crate::core::store::{dig_paths, load_config, load_state, BranchNode, ParentRef};
use crate::core::store::types::DigState;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeOptions {
    pub branch_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeLabel {
    pub branch_name: String,
    pub is_current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub branch_name: String,
    pub is_current: bool,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeView {
    pub root_label: Option<TreeLabel>,
    pub roots: Vec<TreeNode>,
}

#[derive(Debug)]
pub struct TreeOutcome {
    pub status: ExitStatus,
    pub view: TreeView,
}

pub fn run(options: &TreeOptions) -> io::Result<TreeOutcome> {
    let repo = git::resolve_repo_context()?;
    let status = git::probe_repo_status()?;
    let store_paths = dig_paths(&repo.git_dir);
    let config = load_config(&store_paths)?.ok_or_else(|| io::Error::other("dig is not initialized"))?;
    let state = load_state(&store_paths)?;
    let current_branch = git::current_branch_name_if_any()?;
    let full_view = build_tree_view(&state, &config.trunk_branch, current_branch.as_deref());
    let view = filter_tree_view(full_view, options.branch_name.as_deref())?;

    Ok(TreeOutcome {
        status,
        view,
    })
}

fn build_tree_view(state: &DigState, trunk_branch: &str, current_branch: Option<&str>) -> TreeView {
    let active_nodes = state
        .nodes
        .iter()
        .filter(|node| !node.archived)
        .collect::<Vec<_>>();
    let order_lookup = active_nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect::<HashMap<_, _>>();

    let known_ids = active_nodes.iter().map(|node| node.id).collect::<HashSet<_>>();
    let mut child_lookup = HashMap::<Uuid, Vec<&BranchNode>>::new();
    let mut root_nodes = Vec::<&BranchNode>::new();

    for node in &active_nodes {
        match node.parent {
            ParentRef::Trunk => root_nodes.push(node),
            ParentRef::Branch { node_id } if known_ids.contains(&node_id) => {
                child_lookup.entry(node_id).or_default().push(node);
            }
            ParentRef::Branch { .. } => root_nodes.push(node),
        }
    }

    sort_branch_nodes(&mut root_nodes, &order_lookup);
    for children in child_lookup.values_mut() {
        sort_branch_nodes(children, &order_lookup);
    }

    TreeView {
        root_label: Some(TreeLabel {
            branch_name: trunk_branch.to_string(),
            is_current: current_branch == Some(trunk_branch),
        }),
        roots: root_nodes
            .into_iter()
            .map(|node| build_tree_node(node, current_branch, &child_lookup))
            .collect(),
    }
}

fn filter_tree_view(view: TreeView, requested_branch: Option<&str>) -> io::Result<TreeView> {
    let Some(requested_branch) = requested_branch.map(str::trim).filter(|branch| !branch.is_empty()) else {
        return Ok(view);
    };

    let Some(root_label) = &view.root_label else {
        return Ok(view);
    };

    if requested_branch == root_label.branch_name {
        return Ok(TreeView {
            root_label: None,
            roots: view.roots,
        });
    }

    let selected_node = view
        .roots
        .iter()
        .find_map(|root| find_tree_node(root, requested_branch))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("tracked branch '{}' was not found in dig tree", requested_branch),
            )
        })?;

    Ok(TreeView {
        root_label: Some(TreeLabel {
            branch_name: selected_node.branch_name.clone(),
            is_current: selected_node.is_current,
        }),
        roots: selected_node.children.clone(),
    })
}

fn build_tree_node(
    node: &BranchNode,
    current_branch: Option<&str>,
    child_lookup: &HashMap<Uuid, Vec<&BranchNode>>,
) -> TreeNode {
    let children = child_lookup
        .get(&node.id)
        .map(|children| {
            children
                .iter()
                .map(|child| build_tree_node(child, current_branch, child_lookup))
                .collect()
        })
        .unwrap_or_default();

    TreeNode {
        branch_name: node.branch_name.clone(),
        is_current: current_branch == Some(node.branch_name.as_str()),
        children,
    }
}

fn find_tree_node<'a>(node: &'a TreeNode, branch_name: &str) -> Option<&'a TreeNode> {
    if node.branch_name == branch_name {
        return Some(node);
    }

    node.children
        .iter()
        .find_map(|child| find_tree_node(child, branch_name))
}

fn sort_branch_nodes(nodes: &mut Vec<&BranchNode>, order_lookup: &HashMap<Uuid, usize>) {
    nodes.sort_by(|left, right| {
        left.created_at_unix_secs
            .cmp(&right.created_at_unix_secs)
            .then_with(|| {
                order_lookup
                    .get(&left.id)
                    .cmp(&order_lookup.get(&right.id))
            })
            .then_with(|| left.branch_name.cmp(&right.branch_name))
    });
}

#[cfg(test)]
mod tests {
    use super::{build_tree_view, filter_tree_view, TreeLabel, TreeNode, TreeView};
    use crate::core::store::types::DIG_STATE_VERSION;
    use crate::core::store::{BranchNode, ParentRef};
    use crate::core::store::types::DigState;
    use uuid::Uuid;

    #[test]
    fn builds_tree_view_from_shared_root_graph() {
        let auth_id = Uuid::new_v4();
        let auth_api_id = Uuid::new_v4();
        let billing_id = Uuid::new_v4();
        let state = DigState {
            version: DIG_STATE_VERSION,
            nodes: vec![
                BranchNode {
                    id: auth_id,
                    branch_name: "feat/auth".into(),
                    parent: ParentRef::Trunk,
                    base_ref: "main".into(),
                    fork_point_oid: "1".into(),
                    head_oid_at_creation: "1".into(),
                    created_at_unix_secs: 1,
                    archived: false,
                },
                BranchNode {
                    id: auth_api_id,
                    branch_name: "feat/auth-api".into(),
                    parent: ParentRef::Branch { node_id: auth_id },
                    base_ref: "feat/auth".into(),
                    fork_point_oid: "2".into(),
                    head_oid_at_creation: "2".into(),
                    created_at_unix_secs: 2,
                    archived: false,
                },
                BranchNode {
                    id: billing_id,
                    branch_name: "feat/billing".into(),
                    parent: ParentRef::Trunk,
                    base_ref: "main".into(),
                    fork_point_oid: "3".into(),
                    head_oid_at_creation: "3".into(),
                    created_at_unix_secs: 3,
                    archived: false,
                },
            ],
        };

        assert_eq!(
            build_tree_view(&state, "main", Some("feat/auth-api")),
            TreeView {
                root_label: Some(TreeLabel {
                    branch_name: "main".into(),
                    is_current: false,
                }),
                roots: vec![
                    TreeNode {
                        branch_name: "feat/auth".into(),
                        is_current: false,
                        children: vec![TreeNode {
                            branch_name: "feat/auth-api".into(),
                            is_current: true,
                            children: vec![],
                        }],
                    },
                    TreeNode {
                        branch_name: "feat/billing".into(),
                        is_current: false,
                        children: vec![],
                    },
                ],
            }
        );
    }

    #[test]
    fn filters_tree_to_selected_branch_subtree() {
        let view = TreeView {
            root_label: Some(TreeLabel {
                branch_name: "main".into(),
                is_current: false,
            }),
            roots: vec![TreeNode {
                branch_name: "feat/auth".into(),
                is_current: false,
                children: vec![
                    TreeNode {
                        branch_name: "feat/auth-api".into(),
                        is_current: false,
                        children: vec![TreeNode {
                            branch_name: "feat/auth-api-tests".into(),
                            is_current: false,
                            children: vec![],
                        }],
                    },
                    TreeNode {
                        branch_name: "feat/auth-ui".into(),
                        is_current: true,
                        children: vec![],
                    },
                ],
            }],
        };

        assert_eq!(
            filter_tree_view(view, Some("feat/auth")).unwrap(),
            TreeView {
                root_label: Some(TreeLabel {
                    branch_name: "feat/auth".into(),
                    is_current: false,
                }),
                roots: vec![
                    TreeNode {
                        branch_name: "feat/auth-api".into(),
                        is_current: false,
                        children: vec![TreeNode {
                            branch_name: "feat/auth-api-tests".into(),
                            is_current: false,
                            children: vec![],
                        }],
                    },
                    TreeNode {
                        branch_name: "feat/auth-ui".into(),
                        is_current: true,
                        children: vec![],
                    },
                ],
            }
        );
    }
}
