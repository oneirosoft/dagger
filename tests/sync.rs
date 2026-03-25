mod support;

use support::{
    commit_file, dig, dig_ok, dig_with_input, find_node, git_ok, git_stdout, initialize_main_repo,
    load_operation_json, load_state_json, overwrite_file, strip_ansi, with_temp_repo, write_file,
};

#[test]
fn sync_without_continue_reports_not_implemented() {
    with_temp_repo("dig-sync-cli", |repo| {
        initialize_main_repo(repo);
        dig_ok(repo, &["init"]);

        let output = dig(repo, &["sync"]);
        let stderr = String::from_utf8(output.stderr).unwrap();

        assert!(!output.status.success());
        assert!(stderr.contains("dig sync is not implemented yet"));
    });
}

#[test]
fn sync_continues_paused_commit_restack() {
    with_temp_repo("dig-sync-cli", |repo| {
        initialize_main_repo(repo);
        dig_ok(repo, &["init"]);
        dig_ok(repo, &["branch", "feat/auth"]);
        commit_file(repo, "shared.txt", "base\n", "feat: auth");
        dig_ok(repo, &["branch", "feat/auth-ui"]);
        overwrite_file(repo, "shared.txt", "child\n", "feat: child");
        git_ok(repo, &["checkout", "feat/auth"]);
        write_file(repo, "shared.txt", "parent\n");
        git_ok(repo, &["add", "shared.txt"]);

        let paused = dig(repo, &["commit", "-m", "feat: parent follow-up"]);
        assert!(!paused.status.success());
        assert!(load_operation_json(repo).is_some());

        std::fs::write(repo.join("shared.txt"), "resolved\n").unwrap();
        git_ok(repo, &["add", "shared.txt"]);

        let resumed = dig_ok(repo, &["sync", "--continue"]);
        let stdout = strip_ansi(&String::from_utf8(resumed.stdout).unwrap());

        assert!(stdout.contains("feat: parent follow-up"));
        assert!(stdout.contains("Restacked:"));
        assert!(stdout.contains("- feat/auth-ui onto feat/auth"));
        assert_eq!(git_stdout(repo, &["branch", "--show-current"]), "feat/auth");
        assert_eq!(
            git_stdout(repo, &["merge-base", "feat/auth", "feat/auth-ui"]),
            git_stdout(repo, &["rev-parse", "feat/auth"])
        );
        assert!(load_operation_json(repo).is_none());
    });
}

#[test]
fn sync_continues_paused_adopt_rebase() {
    with_temp_repo("dig-sync-cli", |repo| {
        initialize_main_repo(repo);
        dig_ok(repo, &["init"]);
        dig_ok(repo, &["branch", "feat/auth"]);
        overwrite_file(repo, "shared.txt", "parent\n", "feat: parent");
        git_ok(repo, &["checkout", "main"]);
        git_ok(repo, &["checkout", "-b", "feat/auth-ui"]);
        overwrite_file(repo, "shared.txt", "child\n", "feat: child");
        git_ok(repo, &["checkout", "feat/auth"]);

        let paused = dig(repo, &["adopt", "feat/auth-ui", "-p", "feat/auth"]);
        assert!(!paused.status.success());
        assert!(load_operation_json(repo).is_some());

        std::fs::write(repo.join("shared.txt"), "resolved\n").unwrap();
        git_ok(repo, &["add", "shared.txt"]);

        let resumed = dig_ok(repo, &["sync", "--continue"]);
        let stdout = strip_ansi(&String::from_utf8(resumed.stdout).unwrap());

        assert!(stdout.contains("Adopted 'feat/auth-ui' under 'feat/auth'."));
        assert!(stdout.contains("Restacked 'feat/auth-ui' onto 'feat/auth'."));
        assert!(stdout.contains("Returned to 'feat/auth' after adopt."));
        assert_eq!(git_stdout(repo, &["branch", "--show-current"]), "feat/auth");
        assert_eq!(
            git_stdout(repo, &["merge-base", "feat/auth", "feat/auth-ui"]),
            git_stdout(repo, &["rev-parse", "feat/auth"])
        );

        let state = load_state_json(repo);
        assert!(find_node(&state, "feat/auth-ui").is_some());
        assert!(load_operation_json(repo).is_none());
    });
}

#[test]
fn sync_continues_paused_merge_and_preserves_delete_prompt() {
    with_temp_repo("dig-sync-cli", |repo| {
        initialize_main_repo(repo);
        dig_ok(repo, &["init"]);
        dig_ok(repo, &["branch", "feat/auth"]);
        dig_ok(repo, &["branch", "feat/auth-ui"]);
        overwrite_file(repo, "shared.txt", "child\n", "feat: child");
        git_ok(repo, &["checkout", "feat/auth"]);
        overwrite_file(repo, "shared.txt", "parent\n", "feat: parent");

        let paused = dig(repo, &["merge", "feat/auth"]);
        assert!(!paused.status.success());
        assert!(load_operation_json(repo).is_some());

        std::fs::write(repo.join("shared.txt"), "resolved\n").unwrap();
        git_ok(repo, &["add", "shared.txt"]);

        let resumed = dig_with_input(repo, &["sync", "--continue"], "n\n");
        let stdout = strip_ansi(&String::from_utf8(resumed.stdout).unwrap());

        assert!(resumed.status.success());
        assert!(stdout.contains("Merged 'feat/auth' into 'main'."));
        assert!(stdout.contains("Kept merged branch 'feat/auth'."));
        assert_eq!(git_stdout(repo, &["branch", "--show-current"]), "main");
        assert!(git_stdout(repo, &["branch", "--list", "feat/auth"]).contains("feat/auth"));
        assert!(load_operation_json(repo).is_none());
    });
}

#[test]
fn sync_continues_paused_clean_operation() {
    with_temp_repo("dig-sync-cli", |repo| {
        initialize_main_repo(repo);
        dig_ok(repo, &["init"]);
        dig_ok(repo, &["branch", "feat/auth"]);
        dig_ok(repo, &["branch", "feat/auth-api"]);
        overwrite_file(repo, "shared.txt", "child\n", "feat: child");
        git_ok(repo, &["checkout", "feat/auth"]);
        overwrite_file(repo, "shared.txt", "parent\n", "feat: parent");
        git_ok(repo, &["checkout", "main"]);
        git_ok(repo, &["merge", "--squash", "feat/auth"]);
        git_ok(repo, &["commit", "--quiet", "-m", "feat: merge auth"]);
        git_ok(repo, &["checkout", "feat/auth"]);

        let paused = dig_with_input(repo, &["clean", "--branch", "feat/auth"], "y\n");
        assert!(!paused.status.success());
        assert!(load_operation_json(repo).is_some());

        std::fs::write(repo.join("shared.txt"), "resolved\n").unwrap();
        git_ok(repo, &["add", "shared.txt"]);

        let resumed = dig_ok(repo, &["sync", "--continue"]);
        let stdout = strip_ansi(&String::from_utf8(resumed.stdout).unwrap());

        assert!(stdout.contains("Deleted:"));
        assert!(stdout.contains("- feat/auth"));
        assert!(stdout.contains("Restacked:"));
        assert_eq!(git_stdout(repo, &["branch", "--show-current"]), "main");

        let state = load_state_json(repo);
        let child = find_node(&state, "feat/auth-api").unwrap();
        assert_eq!(child["base_ref"], "main");
        assert!(find_node(&state, "feat/auth").is_none());
        assert!(load_operation_json(repo).is_none());
    });
}

#[test]
fn sync_clears_stale_operation_after_rebase_abort() {
    with_temp_repo("dig-sync-cli", |repo| {
        initialize_main_repo(repo);
        dig_ok(repo, &["init"]);
        dig_ok(repo, &["branch", "feat/auth"]);
        commit_file(repo, "shared.txt", "base\n", "feat: auth");
        dig_ok(repo, &["branch", "feat/auth-ui"]);
        overwrite_file(repo, "shared.txt", "child\n", "feat: child");
        git_ok(repo, &["checkout", "feat/auth"]);
        write_file(repo, "shared.txt", "parent\n");
        git_ok(repo, &["add", "shared.txt"]);

        let paused = dig(repo, &["commit", "-m", "feat: parent follow-up"]);
        assert!(!paused.status.success());
        assert!(load_operation_json(repo).is_some());

        git_ok(repo, &["rebase", "--abort"]);

        let resumed = dig(repo, &["sync", "--continue"]);
        let stderr = String::from_utf8(resumed.stderr).unwrap();

        assert!(!resumed.status.success());
        assert!(stderr.contains("paused dig commit operation is stale"));
        assert!(load_operation_json(repo).is_none());
    });
}
