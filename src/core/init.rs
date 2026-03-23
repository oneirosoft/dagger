use std::io;
use std::process::{Command, ExitStatus};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitOptions {}

#[derive(Debug)]
pub struct InitOutcome {
    pub status: ExitStatus,
}

pub fn run(options: &InitOptions) -> io::Result<InitOutcome> {
    let status = Command::new("git")
        .args(build_git_init_args(options))
        .status()?;

    Ok(InitOutcome { status })
}

fn build_git_init_args(_: &InitOptions) -> Vec<String> {
    vec!["init".to_string()]
}

#[cfg(test)]
mod tests {
    use super::{build_git_init_args, InitOptions};

    #[test]
    fn builds_git_init_command() {
        assert_eq!(build_git_init_args(&InitOptions::default()), vec!["init"]);
    }
}
