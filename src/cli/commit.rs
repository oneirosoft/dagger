use std::io;

use clap::{ArgAction, Args};

use crate::core::commit::{self, CommitEntry, CommitOptions};

use super::CommandOutcome;

#[derive(Args, Debug, Clone)]
pub struct CommitArgs {
    /// Pass through git commit -a
    #[arg(short = 'a')]
    pub all: bool,

    /// Pass through git commit -m
    #[arg(short = 'm', value_name = "MESSAGE", action = ArgAction::Append)]
    pub messages: Vec<String>,

    /// Pass through git commit --no-edit
    #[arg(long = "no-edit")]
    pub no_edit: bool,

    /// Pass through git commit --amend
    #[arg(long = "amend")]
    pub amend: bool,
}

pub fn execute(args: CommitArgs) -> io::Result<CommandOutcome> {
    let outcome = commit::run(&args.into())?;

    if outcome.status.success() && !outcome.recent_commits.is_empty() {
        println!("{}", format_recent_commits(&outcome.recent_commits));
    }

    Ok(CommandOutcome {
        status: outcome.status,
    })
}

impl From<CommitArgs> for CommitOptions {
    fn from(args: CommitArgs) -> Self {
        Self {
            all: args.all,
            messages: args.messages,
            no_edit: args.no_edit,
            amend: args.amend,
        }
    }
}

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";
const CHECKMARK: &str = "✓";

fn format_recent_commits(commits: &[CommitEntry]) -> String {
    commits
        .iter()
        .enumerate()
        .map(|(index, commit)| {
            let prefix = if index == 0 {
                format!("{GREEN}{CHECKMARK}{RESET}")
            } else {
                "*".to_string()
            };

            format!(
                "{prefix} {YELLOW}{}{RESET}: {}",
                commit.hash, commit.title
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{format_recent_commits, CommitArgs};
    use crate::core::commit::{CommitEntry, CommitOptions};

    #[test]
    fn converts_cli_args_into_core_commit_options() {
        let args = CommitArgs {
            all: true,
            messages: vec!["message".into()],
            no_edit: true,
            amend: true,
        };

        assert_eq!(
            CommitOptions::from(args),
            CommitOptions {
                all: true,
                messages: vec!["message".into()],
                no_edit: true,
                amend: true,
            }
        );
    }

    #[test]
    fn formats_recent_commits_for_cli_output() {
        let formatted = format_recent_commits(&[
            CommitEntry {
                hash: "abc1234".into(),
                title: "latest commit".into(),
            },
            CommitEntry {
                hash: "def5678".into(),
                title: "older commit".into(),
            },
        ]);

        assert_eq!(
            formatted,
            "\u{1b}[32m✓\u{1b}[0m \u{1b}[33mabc1234\u{1b}[0m: latest commit\n* \u{1b}[33mdef5678\u{1b}[0m: older commit"
        );
    }
}
