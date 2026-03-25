mod apply;
mod plan;
mod types;

pub(crate) use apply::{apply, apply_with_reporter, delete_merged_branch};
pub(crate) use plan::plan;
pub(crate) use types::{
    MergeEvent, MergeMode, MergeOptions, MergeOutcome, MergePlan, MergeTreeNode,
};

#[cfg(test)]
mod tests;
