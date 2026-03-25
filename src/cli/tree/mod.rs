mod render;

use std::io;

use clap::Args;

use crate::core::tree::{self, TreeOptions};

use super::CommandOutcome;

pub(super) use render::{render_branch_lineage, render_stack_tree};

#[derive(Args, Debug, Clone, Default)]
pub struct TreeArgs {
    /// Show only the selected tracked branch stack
    #[arg(long = "branch", value_name = "BRANCH")]
    pub branch_name: Option<String>,
}

pub fn execute(args: TreeArgs) -> io::Result<CommandOutcome> {
    let outcome = tree::run(&args.clone().into())?;

    println!("{}", render::render_stack_tree(&outcome.view));

    Ok(CommandOutcome {
        status: outcome.status,
    })
}

impl From<TreeArgs> for TreeOptions {
    fn from(args: TreeArgs) -> Self {
        Self {
            branch_name: args.branch_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TreeArgs;
    use crate::core::tree::TreeOptions;

    #[test]
    fn converts_cli_args_into_core_tree_options() {
        let options = TreeOptions::from(TreeArgs {
            branch_name: Some("feat/auth".into()),
        });

        assert_eq!(options.branch_name.as_deref(), Some("feat/auth"));
    }
}
