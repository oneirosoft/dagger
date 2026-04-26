use std::io;

use clap::Args;

use crate::core::branch::{
    self, BranchOptions, CreateBranchOptions, DeleteBranchOptions, DeleteBranchOutcome,
};

use super::CommandOutcome;
use super::common;

#[derive(Args, Debug, Clone)]
pub struct BranchArgs {
    /// The name of the branch to create or delete
    pub name: String,

    /// Delete the tracked branch after restacking its descendants
    #[arg(short = 'D', long = "delete", conflicts_with = "parent_branch_name")]
    pub delete: bool,

    /// Override the tracked dagger parent branch
    #[arg(
        short = 'p',
        long = "parent",
        value_name = "BRANCH",
        conflicts_with = "delete"
    )]
    pub parent_branch_name: Option<String>,
}

pub fn execute(args: BranchArgs) -> io::Result<CommandOutcome> {
    let options = BranchOptions::try_from(args.clone())?;
    let outcome = branch::run(&options)?;

    print_branch_outcome(&outcome)?;

    Ok(CommandOutcome {
        status: outcome.status(),
    })
}

fn print_branch_outcome(outcome: &branch::BranchOutcome) -> io::Result<()> {
    match outcome {
        branch::BranchOutcome::Created(create_outcome) => print_create_outcome(create_outcome),
        branch::BranchOutcome::Deleted(delete_outcome) => print_delete_outcome(delete_outcome),
    }
}

fn print_create_outcome(outcome: &branch::CreateBranchOutcome) -> io::Result<()> {
    if !outcome.status.success() {
        return Ok(());
    }

    let Some(node) = &outcome.created_node else {
        return Ok(());
    };

    println!("Created and switched to '{}'.", node.branch_name);
    println!();
    println!("{}", super::tree::render_branch_lineage(&outcome.lineage));

    Ok(())
}

fn print_delete_outcome(outcome: &DeleteBranchOutcome) -> io::Result<()> {
    if outcome.status.success() {
        let rendered_tree =
            super::tree::render_focused_context_tree(&outcome.parent_branch_name, None)?;
        let output = format_delete_success_output(outcome, &rendered_tree);
        if !output.is_empty() {
            println!("{output}");
        }
        return Ok(());
    }

    if outcome.paused {
        common::print_restack_pause_guidance(outcome.failure_output.as_deref());
        return Ok(());
    }

    common::print_trimmed_stderr(outcome.failure_output.as_deref());
    Ok(())
}

impl TryFrom<BranchArgs> for BranchOptions {
    type Error = io::Error;

    fn try_from(args: BranchArgs) -> io::Result<Self> {
        if args.delete {
            if args.parent_branch_name.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--parent cannot be used with --delete",
                ));
            }

            return Ok(Self::Delete(DeleteBranchOptions {
                branch_name: args.name,
            }));
        }

        Ok(Self::Create(CreateBranchOptions {
            name: args.name,
            parent_branch_name: args.parent_branch_name,
        }))
    }
}

pub(crate) fn format_delete_success_output(
    outcome: &DeleteBranchOutcome,
    rendered_tree: &str,
) -> String {
    let mut sections = vec![format_delete_summary(outcome)];
    sections.extend(
        (!outcome.restacked_branches.is_empty())
            .then(|| common::format_restacked_branches(&outcome.restacked_branches)),
    );
    sections.extend((!rendered_tree.trim().is_empty()).then(|| rendered_tree.to_string()));

    common::join_sections(&sections)
}

fn format_delete_summary(outcome: &DeleteBranchOutcome) -> String {
    let mut lines = vec![format!(
        "Deleted '{}'. It is no longer tracked by dagger.",
        outcome.branch_name
    )];
    lines.extend(
        outcome
            .restored_original_branch
            .as_ref()
            .map(|branch| format!("Returned to '{}' after deleting.", branch)),
    );

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{BranchArgs, format_delete_success_output};
    use crate::core::branch::{
        BranchOptions, CreateBranchOptions, DeleteBranchOptions, DeleteBranchOutcome,
    };
    use crate::core::git;
    use crate::core::restack::RestackPreview;

    #[test]
    fn converts_create_cli_args_into_core_branch_options() {
        let args = BranchArgs {
            name: "feature/api".into(),
            delete: false,
            parent_branch_name: Some("main".into()),
        };

        let options = BranchOptions::try_from(args).unwrap();

        assert_eq!(
            options,
            BranchOptions::Create(CreateBranchOptions {
                name: "feature/api".into(),
                parent_branch_name: Some("main".into())
            })
        );
    }

    #[test]
    fn converts_delete_cli_args_into_core_branch_options() {
        let options = BranchOptions::try_from(BranchArgs {
            name: "feature/api".into(),
            delete: true,
            parent_branch_name: None,
        })
        .unwrap();

        assert_eq!(
            options,
            BranchOptions::Delete(DeleteBranchOptions {
                branch_name: "feature/api".into()
            })
        );
    }

    #[test]
    fn rejects_delete_with_parent_override() {
        let err = BranchOptions::try_from(BranchArgs {
            name: "feature/api".into(),
            delete: true,
            parent_branch_name: Some("main".into()),
        })
        .unwrap_err();

        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn formats_delete_success_output_with_restacked_branches() {
        let output = format_delete_success_output(
            &DeleteBranchOutcome {
                status: git::success_status().unwrap(),
                branch_name: "feat/auth".into(),
                parent_branch_name: "main".into(),
                restacked_branches: vec![RestackPreview {
                    branch_name: "feat/auth-ui".into(),
                    onto_branch: "main".into(),
                    parent_changed: true,
                }],
                restored_original_branch: Some("main".into()),
                failure_output: None,
                paused: false,
            },
            "main\n└── feat/auth-ui",
        );

        assert_eq!(
            output,
            concat!(
                "Deleted 'feat/auth'. It is no longer tracked by dagger.\n",
                "Returned to 'main' after deleting.\n\n",
                "Restacked:\n",
                "- feat/auth-ui onto main\n\n",
                "main\n",
                "└── feat/auth-ui"
            )
        );
    }
}
