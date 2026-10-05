#![cfg(unix)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    repo: PathBuf,
    project: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("upstream");
        let project = temp.path().join("project");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&project).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        write(
            &repo.join("plugins/tools/skills/alpha/SKILL.md"),
            "alpha v1\n",
        );
        write(&repo.join("elsewhere/deep/beta/SKILL.md"), "beta v1\n");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "initial"]);
        Self {
            temp,
            repo,
            project,
        }
    }
    fn manifest(&self, selectors: &str, extra: &str) {
        write(&self.project.join("skills.toml"), &format!(
            "version = 1\nprefix = 'store'\ntargets = ['target-a', 'target-b']\n[sources.tools]\nrepo = '{}'\nref = 'main'\nskills = {selectors}\n{extra}\n", self.repo.display()));
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_skills-sync"))
            .current_dir(&self.project)
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Output {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn fail(&self, args: &[&str], message: &str) {
        let output = self.cli(args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn commit_alpha(&self, text: &str) {
        write(&self.repo.join("plugins/tools/skills/alpha/SKILL.md"), text);
        git(&self.repo, &["add", "."]);
        git(&self.repo, &["commit", "-m", "change alpha"]);
    }
    fn content(&self, name: &str) -> String {
        fs::read_to_string(self.project.join("target-a").join(name).join("SKILL.md")).unwrap()
    }
    fn lock(&self) -> Vec<u8> {
        fs::read(self.project.join("skills.lock")).unwrap()
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(path)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn wildcard_nested_skills_are_pinned_idempotent_and_update_explicitly() {
    let f = Fixture::new();
    f.manifest("['*']", "");
    f.ok(&["sync"]);
    assert_eq!(f.content("alpha"), "alpha v1\n");
    assert_eq!(f.content("beta"), "beta v1\n");
    assert!(fs::symlink_metadata(f.project.join("target-b/alpha"))
        .unwrap()
        .file_type()
        .is_symlink());
    let lock = f.lock();
    let link = fs::read_link(f.project.join("target-a/alpha")).unwrap();
    f.commit_alpha("alpha v2\n");
    let output = f.ok(&["sync", "--locked", "--offline"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("0 link changes"));
    assert_eq!(f.lock(), lock);
    assert_eq!(
        fs::read_link(f.project.join("target-a/alpha")).unwrap(),
        link
    );
    f.ok(&["sync"]);
    assert_eq!(f.content("alpha"), "alpha v1\n");
    f.ok(&["update", "tools"]);
    assert_eq!(f.content("alpha"), "alpha v2\n");
    assert_ne!(f.lock(), lock);
    let output = f.ok(&["list", "--json"]);
    let skills: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(skills.as_array().unwrap().len(), 2);
    assert_eq!(skills[1]["source"], "tools");
}

#[test]
fn selectors_prune_only_owned_links_and_rename() {
    let f = Fixture::new();
    f.manifest("['*']", "");
    f.ok(&["sync"]);
    write(&f.project.join("target-a/unmanaged/SKILL.md"), "keep me");
    f.manifest(
        "['plugins/*/skills/**']",
        "rename = { 'plugins/tools/skills/alpha' = 'custom-alpha' }",
    );
    f.fail(&["sync", "--locked"], "manifest/patches changed");
    f.ok(&["sync"]);
    assert!(!f.project.join("target-a/beta").exists());
    assert!(!f.project.join("target-b/alpha").exists());
    assert_eq!(f.content("custom-alpha"), "alpha v1\n");
    assert_eq!(f.content("unmanaged"), "keep me");
    write(
        &f.project.join("skills.toml"),
        "version = 1\nprefix = 'store'\ntargets = ['target-a', 'target-b']\n[sources]\n",
    );
    f.ok(&["sync"]);
    assert!(!f.project.join("target-a/custom-alpha").exists());
    assert_eq!(f.content("unmanaged"), "keep me");
}

#[test]
fn dry_run_and_failed_plan_leave_installation_and_lock_unchanged() {
    let f = Fixture::new();
    f.manifest("['*']", "");
    f.ok(&["sync"]);
    let lock = f.lock();
    let state = fs::read(f.project.join("store/state.json")).unwrap();
    f.commit_alpha("alpha v2\n");
    f.ok(&["update", "--dry-run"]);
    assert_eq!(f.lock(), lock);
    assert_eq!(f.content("alpha"), "alpha v1\n");
    assert_eq!(fs::read(f.project.join("store/state.json")).unwrap(), state);
    f.manifest("['missing/skill']", "");
    f.fail(&["sync"], "matches no skills");
    assert_eq!(f.lock(), lock);
    assert_eq!(f.content("alpha"), "alpha v1\n");
}

#[test]
fn ordered_patches_are_hashed_and_failure_is_safe() {
    let f = Fixture::new();
    f.manifest(
        "['plugins/tools/skills/alpha']",
        "patches = ['patches/one.patch', 'patches/two.patch']",
    );
    let patch = |before: &str, after: &str| {
        format!("diff --git a/plugins/tools/skills/alpha/SKILL.md b/plugins/tools/skills/alpha/SKILL.md\n--- a/plugins/tools/skills/alpha/SKILL.md\n+++ b/plugins/tools/skills/alpha/SKILL.md\n@@ -1 +1 @@\n-{before}\n+{after}\n")
    };
    write(
        &f.project.join("patches/one.patch"),
        &patch("alpha v1", "patched once"),
    );
    write(
        &f.project.join("patches/two.patch"),
        &patch("patched once", "patched twice"),
    );
    f.ok(&["sync"]);
    assert_eq!(f.content("alpha"), "patched twice\n");
    let lock = f.lock();
    f.ok(&["sync", "--locked", "--offline"]);
    write(
        &f.project.join("patches/two.patch"),
        &patch("wrong context", "oops"),
    );
    f.fail(&["sync", "--locked"], "manifest/patches changed");
    f.fail(&["sync"], "does not apply");
    assert_eq!(f.content("alpha"), "patched twice\n");
    assert_eq!(f.lock(), lock);
}

#[test]
fn unowned_files_and_changed_links_are_never_overwritten() {
    let f = Fixture::new();
    f.manifest("['*']", "");
    write(&f.project.join("target-b/beta/SKILL.md"), "mine");
    f.fail(&["sync"], "unmanaged file or directory");
    assert!(!f.project.join("target-a/alpha").exists());
    assert!(!f.project.join("skills.lock").exists());
    fs::remove_dir_all(f.project.join("target-b/beta")).unwrap();
    f.ok(&["sync"]);
    let lock = f.lock();
    fs::remove_file(f.project.join("target-a/alpha")).unwrap();
    std::os::unix::fs::symlink(&f.repo, f.project.join("target-a/alpha")).unwrap();
    f.fail(&["sync"], "externally changed symlink");
    assert_eq!(
        fs::read_link(f.project.join("target-a/alpha")).unwrap(),
        f.repo
    );
    assert_eq!(f.lock(), lock);
}

#[test]
fn local_sources_are_snapshotted_and_locked_by_content() {
    let f = Fixture::new();
    let local = f.temp.path().join("personal");
    write(&local.join("buried/mine/SKILL.md"), "local v1\n");
    write(&f.project.join("skills.toml"), &format!("version = 1\nprefix = 'store'\ntargets = ['target-a']\n[sources.personal]\npath = '{}'\n", local.display()));
    f.ok(&["sync", "--offline"]);
    write(&local.join("buried/mine/SKILL.md"), "local v2\n");
    assert_eq!(f.content("mine"), "local v1\n");
    let lock = f.lock();
    f.fail(&["sync", "--locked"], "local source content differs");
    assert_eq!(f.lock(), lock);
    f.ok(&["sync"]);
    assert_eq!(f.content("mine"), "local v2\n");
}

#[test]
fn install_name_collisions_require_explicit_renames() {
    let f = Fixture::new();
    write(&f.repo.join("another/alpha/SKILL.md"), "another alpha\n");
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "duplicate name"]);
    f.manifest("['*']", "");
    f.fail(&["sync"], "collides");
    assert!(!f.project.join("skills.lock").exists());
    f.manifest("['*']", "rename = { 'another/alpha' = 'other-alpha' }");
    f.ok(&["sync"]);
    assert_eq!(f.content("other-alpha"), "another alpha\n");
}

#[test]
fn clean_prefix_reproduces_the_locked_commit_after_upstream_moves() {
    let f = Fixture::new();
    f.manifest("['*']", "");
    f.ok(&["sync"]);
    let lock = f.lock();
    f.commit_alpha("alpha v2\n");
    fs::remove_dir_all(f.project.join("store")).unwrap();
    fs::remove_dir_all(f.project.join("target-a")).unwrap();
    fs::remove_dir_all(f.project.join("target-b")).unwrap();
    f.ok(&["sync", "--locked"]);
    assert_eq!(f.content("alpha"), "alpha v1\n");
    assert_eq!(f.lock(), lock);
}

#[test]
fn recovery_restores_an_interrupted_transaction() {
    let f = Fixture::new();
    f.manifest("['*']", "");
    f.ok(&["sync"]);
    let before: serde_json::Value =
        serde_json::from_slice(&fs::read(f.project.join("store/state.json")).unwrap()).unwrap();
    let lock = f.lock();
    let mut after = before.clone();
    let project = fs::canonicalize(&f.project).unwrap();
    let link = project.join("target-a/alpha");
    let changed = project.join("store/sources/fake/tree");
    assert!(before["links"].get(link.to_str().unwrap()).is_some());
    after["links"][link.to_str().unwrap()] = serde_json::json!(changed);
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&changed, &link).unwrap();
    let journal = serde_json::json!({"before": before, "after": after, "lock_before": String::from_utf8(lock.clone()).unwrap()});
    fs::write(
        f.project.join("store/transaction.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    write(&f.project.join("skills.lock"), "interrupted lock write");
    f.ok(&["sync", "--locked", "--offline"]);
    assert_eq!(f.content("alpha"), "alpha v1\n");
    assert_eq!(f.lock(), lock);
    assert!(!f.project.join("store/transaction.json").exists());
}

#[test]
fn relative_paths_resolve_from_manifest_and_init_does_not_overwrite() {
    let f = Fixture::new();
    f.ok(&["init"]);
    f.fail(&["init"], "creating");
    f.manifest("['plugins/tools/skills/alpha']", "");
    let output = Command::new(env!("CARGO_BIN_EXE_skills-sync"))
        .current_dir(f.temp.path())
        .args([
            "--manifest",
            f.project.join("skills.toml").to_str().unwrap(),
            "sync",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(f.content("alpha"), "alpha v1\n");
}

#[test]
fn source_symlinks_preserve_internal_references_and_reject_escapes() {
    let f = Fixture::new();
    write(&f.repo.join("shared.txt"), "shared data");
    std::os::unix::fs::symlink(
        "../../../../shared.txt",
        f.repo.join("plugins/tools/skills/alpha/ref.txt"),
    )
    .unwrap();
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "internal symlink"]);
    f.manifest("['*']", "");
    f.ok(&["sync"]);
    assert_eq!(
        fs::read_to_string(f.project.join("target-a/alpha/ref.txt")).unwrap(),
        "shared data"
    );
    let lock = f.lock();
    std::os::unix::fs::symlink(
        "/etc/passwd",
        f.repo.join("plugins/tools/skills/alpha/escape.txt"),
    )
    .unwrap();
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "unsafe symlink"]);
    f.fail(&["update"], "symlink escapes archive");
    assert_eq!(f.lock(), lock);
    assert_eq!(f.content("alpha"), "alpha v1\n");
}

#[test]
fn root_skill_requires_and_accepts_an_install_name() {
    let f = Fixture::new();
    write(&f.repo.join("SKILL.md"), "root skill\n");
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "root skill"]);
    f.manifest("['.']", "");
    f.fail(&["sync"], "root SKILL.md requires rename");
    f.manifest("['.']", "rename = { '.' = 'root-skill' }");
    f.ok(&["sync"]);
    assert_eq!(f.content("root-skill"), "root skill\n");
}

#[test]
fn target_changes_reconcile_old_and_new_destinations() {
    let f = Fixture::new();
    f.manifest("['*']", "");
    f.ok(&["sync"]);
    let manifest = fs::read_to_string(f.project.join("skills.toml")).unwrap();
    write(
        &f.project.join("skills.toml"),
        &manifest.replace("['target-a', 'target-b']", "['target-c']"),
    );
    f.ok(&["sync"]);
    assert!(!f.project.join("target-a/alpha").exists());
    assert!(!f.project.join("target-b/beta").exists());
    assert_eq!(
        fs::read_to_string(f.project.join("target-c/alpha/SKILL.md")).unwrap(),
        "alpha v1\n"
    );
}

#[test]
fn targeted_update_preserves_other_sources_commits() {
    let f = Fixture::new();
    f.manifest(
        "['plugins/tools/skills/alpha']",
        &format!(
            "\n[sources.other]\nrepo = '{}'\nref = 'main'\nskills = ['elsewhere/deep/beta']",
            f.repo.display()
        ),
    );
    f.ok(&["sync"]);
    write(&f.repo.join("elsewhere/deep/beta/SKILL.md"), "beta v2\n");
    f.commit_alpha("alpha v2\n");
    f.ok(&["update", "tools"]);
    assert_eq!(f.content("alpha"), "alpha v2\n");
    assert_eq!(f.content("beta"), "beta v1\n");
    f.fail(&["update", "unknown"], "unknown source ID");
    f.ok(&["update", "other"]);
    assert_eq!(f.content("beta"), "beta v2\n");
}

#[test]
fn offline_sync_uses_cached_objects_when_upstream_is_unavailable() {
    let f = Fixture::new();
    f.manifest("['*']", "");
    f.fail(&["sync", "--offline"], "not cached");
    f.ok(&["sync"]);
    let lock = f.lock();
    fs::rename(&f.repo, f.temp.path().join("unavailable")).unwrap();
    f.ok(&["sync", "--locked", "--offline"]);
    assert_eq!(f.content("alpha"), "alpha v1\n");
    f.fail(&["update"], "fetch");
    assert_eq!(f.lock(), lock);
}

#[test]
fn overlapping_paths_and_concurrent_sync_are_rejected() {
    use fs2::FileExt;
    let f = Fixture::new();
    f.manifest("['*']", "");
    f.ok(&["sync"]);
    let guard = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.project.join("store/sync.lock"))
        .unwrap();
    guard.lock_exclusive().unwrap();
    f.fail(&["sync"], "another sync is already running");
    FileExt::unlock(&guard).unwrap();
    std::os::unix::fs::symlink(f.project.join("store"), f.project.join("alias")).unwrap();
    let manifest = fs::read_to_string(f.project.join("skills.toml")).unwrap();
    write(
        &f.project.join("skills.toml"),
        &manifest.replace("['target-a', 'target-b']", "['alias/skills']"),
    );
    f.fail(&["sync"], "prefix and targets must not overlap");
}

#[test]
fn manifest_cannot_overwrite_itself_as_a_lockfile() {
    let f = Fixture::new();
    f.manifest("['*']", "");
    let text = fs::read_to_string(f.project.join("skills.toml")).unwrap();
    write(&f.project.join("config.lock"), &text);
    f.fail(
        &["--manifest", "config.lock", "sync"],
        "manifest cannot use the .lock extension",
    );
    assert_eq!(
        fs::read_to_string(f.project.join("config.lock")).unwrap(),
        text
    );
}
