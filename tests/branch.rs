mod support;

use std::path::{Path, PathBuf};

use support::{
    active_rebase_head_name, commit_file, dgr, dgr_ok, dgr_ok_with_env, find_archived_node,
    find_node, git_ok, git_stdout, initialize_main_repo, install_fake_executable, load_events_json,
    load_operation_json, load_state_json, path_with_prepend, strip_ansi, with_temp_repo,
    write_file,
};

fn install_fake_gh(repo: &Path, script: &str) -> (PathBuf, String) {
    let bin_dir = repo.join("fake-bin");
    install_fake_executable(&bin_dir, "gh", script);

    let path = path_with_prepend(&bin_dir);

    (bin_dir, path)
}

#[test]
fn branch_command_renders_marked_lineage_and_tracks_parent() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);

        let output = dgr_ok(repo, &["branch", "feat/auth"]);
        let stdout = strip_ansi(&String::from_utf8(output.stdout).unwrap());

        assert!(stdout.contains("Created and switched to 'feat/auth'."));
        assert!(stdout.contains("✓ feat/auth\n│ \n* main"));

        let state = load_state_json(repo);
        let node = find_node(&state, "feat/auth").unwrap();
        assert_eq!(node["base_ref"], "main");
        assert_eq!(node["parent"]["kind"], "trunk");
    });
}

#[test]
fn init_reuses_marked_lineage_output_for_current_branch() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);
        dgr_ok(repo, &["branch", "feat/auth"]);

        let output = dgr_ok(repo, &["init"]);
        let stdout = strip_ansi(&String::from_utf8(output.stdout).unwrap());

        assert!(stdout.contains("Using existing Git repository."));
        assert!(stdout.contains("Dagger is already initialized."));
        assert!(stdout.contains("✓ feat/auth\n│ \n* main"));
    });
}

#[test]
fn init_lineage_shows_tracked_pull_request_numbers() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);
        dgr_ok(repo, &["branch", "feat/auth"]);

        let (_, path) = install_fake_gh(
            repo,
            r#"#!/bin/sh
set -eu
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  printf '[]\n'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
  printf 'https://github.com/oneirosoft/dagger/pull/123\n'
  exit 0
fi
echo "unexpected gh args: $*" >&2
exit 1
"#,
        );

        dgr_ok_with_env(repo, &["pr"], &[("PATH", path.as_str())]);

        let output = dgr_ok(repo, &["init"]);
        let stdout = strip_ansi(&String::from_utf8(output.stdout).unwrap());

        assert!(stdout.contains("✓ feat/auth (#123)\n│ \n* main"));
    });
}

#[test]
fn branch_delete_removes_leaf_branch_and_archives_it() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);
        dgr_ok(repo, &["branch", "feat/auth"]);
        commit_file(repo, "auth.txt", "auth\n", "feat: auth");
        git_ok(repo, &["checkout", "main"]);

        let output = dgr_ok(repo, &["branch", "-D", "feat/auth"]);
        let stdout = strip_ansi(&String::from_utf8(output.stdout).unwrap());

        assert!(stdout.contains("Deleted 'feat/auth'. It is no longer tracked by dagger."));
        assert!(!git_stdout(repo, &["branch", "--list", "feat/auth"]).contains("feat/auth"));
        assert_eq!(git_stdout(repo, &["branch", "--show-current"]), "main");

        let state = load_state_json(repo);
        assert!(find_node(&state, "feat/auth").is_none());
        let archived = find_archived_node(&state, "feat/auth").unwrap();
        assert_eq!(archived["archived"], true);
        let events = load_events_json(repo);
        assert!(events.iter().any(|event| {
            event["type"].as_str() == Some("branch_archived")
                && event["branch_name"].as_str() == Some("feat/auth")
                && event["reason"]["kind"].as_str() == Some("deleted_by_user")
        }));
        assert_eq!(load_operation_json(repo), None);
    });
}

#[test]
fn branch_delete_restacks_child_onto_deleted_branch_parent() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);
        dgr_ok(repo, &["branch", "feat/auth"]);
        commit_file(repo, "auth.txt", "auth\n", "feat: auth");
        dgr_ok(repo, &["branch", "feat/auth-api"]);
        commit_file(repo, "api.txt", "api\n", "feat: auth api");
        dgr_ok(repo, &["branch", "feat/auth-api-tests"]);
        commit_file(repo, "tests.txt", "tests\n", "feat: auth api tests");
        git_ok(repo, &["checkout", "feat/auth"]);

        let output = dgr_ok(repo, &["branch", "--delete", "feat/auth-api"]);
        let stdout = strip_ansi(&String::from_utf8(output.stdout).unwrap());

        assert!(stdout.contains("Deleted 'feat/auth-api'. It is no longer tracked by dagger."));
        assert!(stdout.contains("Returned to 'feat/auth' after deleting."));
        assert!(stdout.contains("- feat/auth-api-tests onto feat/auth"));
        assert!(
            !git_stdout(repo, &["branch", "--list", "feat/auth-api"]).contains("feat/auth-api")
        );
        assert_eq!(git_stdout(repo, &["branch", "--show-current"]), "feat/auth");
        assert_eq!(
            git_stdout(repo, &["merge-base", "feat/auth", "feat/auth-api-tests"]),
            git_stdout(repo, &["rev-parse", "feat/auth"])
        );

        let state = load_state_json(repo);
        assert!(find_node(&state, "feat/auth-api").is_none());
        assert!(find_archived_node(&state, "feat/auth-api").is_some());
        let child = find_node(&state, "feat/auth-api-tests").unwrap();
        assert_eq!(child["base_ref"], "feat/auth");
        assert_eq!(
            child["parent"]["node_id"],
            find_node(&state, "feat/auth").unwrap()["id"]
        );
        assert_eq!(load_operation_json(repo), None);
    });
}

#[test]
fn branch_delete_rejects_current_branch() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);
        dgr_ok(repo, &["branch", "feat/auth"]);

        let output = dgr(repo, &["branch", "--delete", "feat/auth"]);

        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(
            "cannot delete checked-out branch 'feat/auth'; switch to another branch first"
        ));
        assert!(git_stdout(repo, &["branch", "--list", "feat/auth"]).contains("feat/auth"));
    });
}

#[test]
fn branch_delete_rejects_trunk_branch() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);

        let output = dgr(repo, &["branch", "--delete", "main"]);

        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("cannot delete trunk branch 'main'"));
    });
}

#[test]
fn branch_delete_rejects_untracked_branch() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);
        git_ok(repo, &["checkout", "-b", "feat/manual"]);
        commit_file(repo, "manual.txt", "manual\n", "feat: manual");
        git_ok(repo, &["checkout", "main"]);

        let output = dgr(repo, &["branch", "--delete", "feat/manual"]);

        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("branch 'feat/manual' is not tracked by dagger"));
        assert!(git_stdout(repo, &["branch", "--list", "feat/manual"]).contains("feat/manual"));
    });
}

#[test]
fn branch_delete_rejects_branch_with_missing_tracked_descendant() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);
        dgr_ok(repo, &["branch", "feat/auth"]);
        commit_file(repo, "auth.txt", "auth\n", "feat: auth");
        dgr_ok(repo, &["branch", "feat/auth-api"]);
        commit_file(repo, "api.txt", "api\n", "feat: auth api");
        git_ok(repo, &["checkout", "main"]);
        git_ok(repo, &["branch", "-D", "feat/auth-api"]);

        let output = dgr(repo, &["branch", "--delete", "feat/auth"]);

        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr
                .contains("tracked descendants of 'feat/auth' are missing locally: feat/auth-api")
        );
        assert!(git_stdout(repo, &["branch", "--list", "feat/auth"]).contains("feat/auth"));
    });
}

#[test]
fn sync_continues_paused_branch_delete_restack() {
    with_temp_repo("dgr-branch-cli", |repo| {
        initialize_main_repo(repo);
        dgr_ok(repo, &["init"]);
        dgr_ok(repo, &["branch", "feat/auth"]);
        commit_file(repo, "shared.txt", "parent\n", "feat: auth");
        dgr_ok(repo, &["branch", "feat/auth-ui"]);
        commit_file(repo, "shared.txt", "child\n", "feat: auth ui");
        git_ok(repo, &["checkout", "main"]);
        commit_file(repo, "shared.txt", "main\n", "feat: trunk");

        let output = dgr(repo, &["branch", "--delete", "feat/auth"]);
        assert!(!output.status.success());
        assert!(git_stdout(repo, &["branch", "--list", "feat/auth"]).contains("feat/auth"));
        assert!(repo.join(".git/rebase-merge").exists() || repo.join(".git/rebase-apply").exists());
        assert!(active_rebase_head_name(repo).contains("feat/auth-ui"));
        let operation = load_operation_json(repo).unwrap();
        assert_eq!(operation["origin"]["type"].as_str(), Some("branch_delete"));

        write_file(repo, "shared.txt", "resolved\n");
        git_ok(repo, &["add", "shared.txt"]);

        let output = dgr_ok(repo, &["sync", "--continue"]);
        let stdout = strip_ansi(&String::from_utf8(output.stdout).unwrap());

        assert!(stdout.contains("Deleted 'feat/auth'. It is no longer tracked by dagger."));
        assert!(stdout.contains("Returned to 'main' after deleting."));
        assert!(stdout.contains("- feat/auth-ui onto main"));
        assert!(!git_stdout(repo, &["branch", "--list", "feat/auth"]).contains("feat/auth"));
        assert_eq!(git_stdout(repo, &["branch", "--show-current"]), "main");

        let state = load_state_json(repo);
        assert!(find_node(&state, "feat/auth").is_none());
        let archived = find_archived_node(&state, "feat/auth").unwrap();
        assert_eq!(archived["archived"], true);
        let events = load_events_json(repo);
        assert!(events.iter().any(|event| {
            event["type"].as_str() == Some("branch_archived")
                && event["branch_name"].as_str() == Some("feat/auth")
                && event["reason"]["kind"].as_str() == Some("deleted_by_user")
        }));
        let child = find_node(&state, "feat/auth-ui").unwrap();
        assert_eq!(child["base_ref"], "main");
        assert_eq!(child["parent"]["kind"], "trunk");
        assert_eq!(load_operation_json(repo), None);
    });
}
