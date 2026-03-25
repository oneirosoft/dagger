use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use uuid::Uuid;

#[test]
fn adopts_current_branch_onto_trunk_without_rebase() {
    with_temp_repo(|repo| {
        initialize_main_repo(repo);
        dig_ok(repo, &["init"]);
        git_ok(repo, &["checkout", "-b", "feat/adopted"]);
        commit_file(repo, "adopted.txt", "adopted\n", "feat: adopted");

        let output = dig_ok(repo, &["adopt", "-p", "main"]);
        let stdout = strip_ansi(&String::from_utf8(output.stdout).unwrap());

        assert!(stdout.contains("Adopted 'feat/adopted' under 'main'."));
        assert!(stdout.contains("main\n└── ✓ feat/adopted"));

        let state = load_state_json(repo);
        let adopted = find_node(&state, "feat/adopted").unwrap();
        assert_eq!(adopted["base_ref"], "main");
        assert_eq!(adopted["parent"]["kind"], "trunk");
    });
}

#[test]
fn adopts_named_branch_with_rebase_and_restores_original_branch() {
    with_temp_repo(|repo| {
        initialize_main_repo(repo);
        dig_ok(repo, &["init"]);
        dig_ok(repo, &["branch", "feat/auth"]);
        commit_file(repo, "auth.txt", "auth\n", "feat: auth");
        git_ok(repo, &["checkout", "main"]);
        git_ok(repo, &["checkout", "-b", "feat/auth-ui"]);
        commit_file(repo, "ui.txt", "ui\n", "feat: auth ui");
        git_ok(repo, &["checkout", "feat/auth"]);

        let output = dig_ok(repo, &["adopt", "feat/auth-ui", "-p", "feat/auth"]);
        let stdout = strip_ansi(&String::from_utf8(output.stdout).unwrap());

        assert!(stdout.contains("Adopted 'feat/auth-ui' under 'feat/auth'."));
        assert!(stdout.contains("Restacked 'feat/auth-ui' onto 'feat/auth'."));
        assert!(stdout.contains("Returned to 'feat/auth' after adopt."));
        assert!(stdout.contains("main\n└── feat/auth\n    └── ✓ feat/auth-ui"));

        let merge_base = git_stdout(repo, &["merge-base", "feat/auth", "feat/auth-ui"]);
        let parent_head = git_stdout(repo, &["rev-parse", "feat/auth"]);
        assert_eq!(merge_base, parent_head);
        assert_eq!(git_stdout(repo, &["branch", "--show-current"]), "feat/auth");

        let state = load_state_json(repo);
        let adopted = find_node(&state, "feat/auth-ui").unwrap();
        assert_eq!(adopted["base_ref"], "feat/auth");
        assert_eq!(adopted["parent"]["kind"], "branch");
    });
}

#[test]
fn leaves_state_untouched_when_adopt_rebase_conflicts() {
    with_temp_repo(|repo| {
        initialize_main_repo(repo);
        dig_ok(repo, &["init"]);
        dig_ok(repo, &["branch", "feat/auth"]);
        overwrite_file(repo, "shared.txt", "parent\n", "feat: parent change");
        git_ok(repo, &["checkout", "main"]);
        git_ok(repo, &["checkout", "-b", "feat/auth-ui"]);
        overwrite_file(repo, "shared.txt", "child\n", "feat: child change");
        git_ok(repo, &["checkout", "feat/auth"]);

        let output = dig(repo, &["adopt", "feat/auth-ui", "-p", "feat/auth"]);
        assert!(!output.status.success());

        let state = load_state_json(repo);
        assert!(find_node(&state, "feat/auth-ui").is_none());
        assert!(!events_contain_type(repo, "branch_adopted"));
        assert!(!repo.join(".git/rebase-merge").exists());
        assert!(!repo.join(".git/rebase-apply").exists());
        assert_eq!(git_stdout(repo, &["branch", "--show-current"]), "feat/auth");
    });
}

fn with_temp_repo(test: impl FnOnce(&Path)) {
    let repo_dir = std::env::temp_dir().join(format!("dig-adopt-cli-{}", Uuid::new_v4()));
    fs::create_dir_all(&repo_dir).unwrap();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        test(&repo_dir);
    }));

    fs::remove_dir_all(&repo_dir).unwrap();

    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

fn initialize_main_repo(repo: &Path) {
    git_ok(repo, &["init", "--quiet"]);
    git_ok(repo, &["checkout", "-b", "main"]);
    git_ok(repo, &["config", "user.name", "Dig Test"]);
    git_ok(repo, &["config", "user.email", "dig@example.com"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);
    commit_file(repo, "README.md", "root\n", "chore: init");
}

fn commit_file(repo: &Path, file_name: &str, contents: &str, message: &str) {
    fs::write(repo.join(file_name), contents).unwrap();
    git_ok(repo, &["add", file_name]);
    git_ok(
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

fn overwrite_file(repo: &Path, file_name: &str, contents: &str, message: &str) {
    fs::write(repo.join(file_name), contents).unwrap();
    git_ok(repo, &["add", file_name]);
    git_ok(
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            message,
        ],
    );
}

fn dig(repo: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dig"))
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap()
}

fn dig_ok(repo: &Path, args: &[&str]) -> Output {
    let output = dig(repo, args);
    assert!(
        output.status.success(),
        "dig {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git_ok(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .unwrap();

    assert!(status.success(), "git {:?} failed", args);
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();

    assert!(output.status.success(), "git {:?} failed", args);

    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn load_state_json(repo: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(repo.join(".git/dig/state.json")).unwrap()).unwrap()
}

fn find_node<'a>(state: &'a Value, branch_name: &str) -> Option<&'a Value> {
    state["nodes"].as_array().and_then(|nodes| {
        nodes.iter().find(|node| {
            node["branch_name"].as_str() == Some(branch_name)
                && node["archived"].as_bool() == Some(false)
        })
    })
}

fn events_contain_type(repo: &Path, event_type: &str) -> bool {
    fs::read_to_string(repo.join(".git/dig/events.ndjson"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .any(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|event| event["type"].as_str().map(str::to_string))
                .as_deref()
                == Some(event_type)
        })
}

fn strip_ansi(text: &str) -> String {
    let mut stripped = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
            continue;
        }

        stripped.push(ch);
    }

    stripped
}
