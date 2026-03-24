use std::io;

use clap::{ArgAction, Args};

use crate::core::commit::{self, CommitEntry, CommitOptions, CommitOutcome};
use crate::ui::markers;

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

    if outcome.status.success() {
        let output = format_commit_success_output(&outcome);

        if !output.is_empty() {
            println!("{output}");
        }
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

fn format_commit_success_output(outcome: &CommitOutcome) -> String {
    let mut sections = Vec::new();

    if let Some(summary_line) = &outcome.summary_line {
        sections.push(summary_line.clone());
    }

    if !outcome.recent_commits.is_empty() {
        sections.push(format_recent_commits(&outcome.recent_commits));
    }

    sections.join("\n\n")
}

fn format_recent_commits(commits: &[CommitEntry]) -> String {
    commits
        .iter()
        .map(|commit| {
            let prefix = if commit.is_head {
                format!("{GREEN}{}{RESET}", markers::HEAD)
            } else {
                markers::LIST_ITEM.to_string()
            };
            let refs = format_commit_refs(commit);

            format!(
                "{prefix} {YELLOW}{}{RESET}{refs}: {}",
                commit.hash, commit.title
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_commit_refs(commit: &CommitEntry) -> String {
    if commit.refs.is_empty() {
        String::new()
    } else {
        format!(" ({})", commit.refs.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::{format_commit_success_output, format_recent_commits, CommitArgs};
    use crate::core::commit::{CommitEntry, CommitOptions, CommitOutcome};
    use std::os::unix::process::ExitStatusExt;

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
                refs: vec!["main".into(), "tag: v0.1.0".into()],
                is_head: true,
                title: "latest commit".into(),
            },
            CommitEntry {
                hash: "def5678".into(),
                refs: vec!["tag: v0.0.9".into()],
                is_head: false,
                title: "older commit".into(),
            },
        ]);

        assert_eq!(
            formatted,
            "\u{1b}[32m→\u{1b}[0m \u{1b}[33mabc1234\u{1b}[0m (main, tag: v0.1.0): latest commit\n* \u{1b}[33mdef5678\u{1b}[0m (tag: v0.0.9): older commit"
        );
    }

    #[test]
    fn formats_commit_output_with_summary_and_blank_line_before_log() {
        let outcome = CommitOutcome {
            status: std::process::ExitStatus::from_raw(0),
            summary_line: Some("10 files changed, 2245 insertions(+)".into()),
            recent_commits: vec![CommitEntry {
                hash: "abc1234".into(),
                refs: vec!["main".into()],
                is_head: true,
                title: "latest commit".into(),
            }],
        };

        assert_eq!(
            format_commit_success_output(&outcome),
            "10 files changed, 2245 insertions(+)\n\n\u{1b}[32m→\u{1b}[0m \u{1b}[33mabc1234\u{1b}[0m (main): latest commit"
        );
    }
}
