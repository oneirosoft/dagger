use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DIG_STATE_VERSION: u32 = 1;
pub const DIG_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigConfig {
    pub version: u32,
    pub trunk_branch: String,
}

impl DigConfig {
    pub fn new(trunk_branch: String) -> Self {
        Self {
            version: DIG_CONFIG_VERSION,
            trunk_branch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigState {
    pub version: u32,
    pub nodes: Vec<BranchNode>,
}

impl Default for DigState {
    fn default() -> Self {
        Self {
            version: DIG_STATE_VERSION,
            nodes: Vec::new(),
        }
    }
}

impl DigState {
    pub fn find_branch_by_name(&self, branch_name: &str) -> Option<&BranchNode> {
        self.nodes
            .iter()
            .find(|node| !node.archived && node.branch_name == branch_name)
    }

    pub fn find_branch_by_id(&self, node_id: Uuid) -> Option<&BranchNode> {
        self.nodes
            .iter()
            .find(|node| !node.archived && node.id == node_id)
    }

    pub fn find_branch_by_id_mut(&mut self, node_id: Uuid) -> Option<&mut BranchNode> {
        self.nodes
            .iter_mut()
            .find(|node| !node.archived && node.id == node_id)
    }

    pub fn insert_branch(&mut self, node: BranchNode) -> io::Result<()> {
        if self.find_branch_by_name(&node.branch_name).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("branch '{}' is already tracked by dig", node.branch_name),
            ));
        }

        self.nodes.push(node);

        Ok(())
    }

    pub fn archive_branch(&mut self, node_id: Uuid) -> io::Result<()> {
        let node = self.find_branch_by_id_mut(node_id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "tracked branch was not found")
        })?;

        node.archived = true;

        Ok(())
    }

    pub fn reparent_branch(
        &mut self,
        node_id: Uuid,
        new_parent: ParentRef,
        new_base_ref: String,
    ) -> io::Result<(ParentRef, String)> {
        let node = self.find_branch_by_id_mut(node_id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "tracked branch was not found")
        })?;

        let old_parent = node.parent.clone();
        let old_base_ref = node.base_ref.clone();
        node.parent = new_parent;
        node.base_ref = new_base_ref;

        Ok((old_parent, old_base_ref))
    }

}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchNode {
    pub id: Uuid,
    pub branch_name: String,
    pub parent: ParentRef,
    pub base_ref: String,
    pub fork_point_oid: String,
    pub head_oid_at_creation: String,
    pub created_at_unix_secs: u64,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParentRef {
    Trunk,
    Branch { node_id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DigEvent {
    BranchCreated(BranchCreatedEvent),
    BranchAdopted(BranchAdoptedEvent),
    BranchArchived(BranchArchivedEvent),
    BranchReparented(BranchReparentedEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchCreatedEvent {
    pub occurred_at_unix_secs: u64,
    pub node: BranchNode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchAdoptedEvent {
    pub occurred_at_unix_secs: u64,
    pub node: BranchNode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BranchArchiveReason {
    IntegratedIntoParent { parent_branch: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchArchivedEvent {
    pub occurred_at_unix_secs: u64,
    pub branch_id: Uuid,
    pub branch_name: String,
    pub reason: BranchArchiveReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchReparentedEvent {
    pub occurred_at_unix_secs: u64,
    pub branch_id: Uuid,
    pub branch_name: String,
    pub old_parent: ParentRef,
    pub new_parent: ParentRef,
    pub old_base_ref: String,
    pub new_base_ref: String,
}

pub fn now_unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        BranchAdoptedEvent, BranchArchiveReason, BranchArchivedEvent, BranchNode, DigConfig,
        DigEvent, DigState, ParentRef,
    };
    use uuid::Uuid;

    #[test]
    fn tracks_roots_with_union_parent_ref() {
        let node = BranchNode {
            id: Uuid::nil(),
            branch_name: "feature/api".into(),
            parent: ParentRef::Trunk,
            base_ref: "main".into(),
            fork_point_oid: "abc123".into(),
            head_oid_at_creation: "abc123".into(),
            created_at_unix_secs: 1,
            archived: false,
        };

        let mut state = DigState::default();
        state.insert_branch(node.clone()).unwrap();

        assert_eq!(state.find_branch_by_name("feature/api"), Some(&node));
    }

    #[test]
    fn builds_config_with_trunk_branch() {
        assert_eq!(DigConfig::new("main".into()).trunk_branch, "main");
    }

    #[test]
    fn serializes_branch_archive_event_with_union_reason() {
        let event = DigEvent::BranchArchived(BranchArchivedEvent {
            occurred_at_unix_secs: 1,
            branch_id: Uuid::nil(),
            branch_name: "feature/api".into(),
            reason: BranchArchiveReason::IntegratedIntoParent {
                parent_branch: "main".into(),
            },
        });

        let serialized = serde_json::to_string(&event).unwrap();

        assert!(serialized.contains("\"type\":\"branch_archived\""));
        assert!(serialized.contains("\"kind\":\"integrated_into_parent\""));
    }

    #[test]
    fn serializes_branch_adopted_event() {
        let event = DigEvent::BranchAdopted(BranchAdoptedEvent {
            occurred_at_unix_secs: 1,
            node: BranchNode {
                id: Uuid::nil(),
                branch_name: "feature/api".into(),
                parent: ParentRef::Trunk,
                base_ref: "main".into(),
                fork_point_oid: "abc123".into(),
                head_oid_at_creation: "def456".into(),
                created_at_unix_secs: 1,
                archived: false,
            },
        });

        let serialized = serde_json::to_string(&event).unwrap();

        assert!(serialized.contains("\"type\":\"branch_adopted\""));
        assert!(serialized.contains("\"branch_name\":\"feature/api\""));
    }
}
