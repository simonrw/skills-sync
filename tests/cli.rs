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

fn skill_patch(find: &str, replace: &str, min: usize, max: usize) -> String {
    format!("version = 1\nfile = 'plugins/tools/skills/alpha/SKILL.md'\n[[operations]]\nop = 'replace_text'\nsection = []\nfind = '{find}'\nreplace = '{replace}'\nwhole_words = true\nexpect = {{ min = {min}, max = {max} }}\n")
}
fn alpha_git_patch(before: &str, after: &str) -> String {
    format!("diff --git a/plugins/tools/skills/alpha/SKILL.md b/plugins/tools/skills/alpha/SKILL.md\n--- a/plugins/tools/skills/alpha/SKILL.md\n+++ b/plugins/tools/skills/alpha/SKILL.md\n@@ -1 +1 @@\n-{before}\n+{after}\n")
}
#[test]
fn mixed_skill_patch_order_raw_hashes_reports_and_offline_replay() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    f.manifest(
        "['plugins/tools/skills/alpha']",
        "patches = ['one.patch', 'two.skillpatch.toml', 'three.patch', 'zero.skillpatch.toml']",
    );
    let one = alpha_git_patch("alpha v1", "alpha interim");
    let two = skill_patch("interim", "markdown", 1, 1);
    let three = alpha_git_patch("alpha markdown", "alpha final");
    let zero = skill_patch("absent", "ignored", 0, 0);
    for (name, text) in [
        ("one.patch", &one),
        ("two.skillpatch.toml", &two),
        ("three.patch", &three),
        ("zero.skillpatch.toml", &zero),
    ] {
        write(&f.project.join(name), text);
    }
    let preview = f.ok(&["sync", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&preview.stdout);
    assert!(stdout.contains("actual 1"));
    assert!(stdout.contains("actual 0"));
    assert!(stdout.contains("-alpha interim"));
    assert!(stdout.contains("+alpha markdown"));
    assert!(!f.project.join("skills.lock").exists());
    f.ok(&["sync"]);
    assert_eq!(f.content("alpha"), "alpha final\n");
    let lock = f.lock();
    let parsed: toml::Value = toml::from_str(std::str::from_utf8(&lock).unwrap()).unwrap();
    for (i, raw) in [&one, &two, &three, &zero].iter().enumerate() {
        assert_eq!(
            parsed["sources"]["tools"]["patches"][i]["sha256"]
                .as_str()
                .unwrap(),
            format!("{:x}", Sha256::digest(raw.as_bytes()))
        );
    }
    fs::rename(&f.repo, f.temp.path().join("unavailable-upstream")).unwrap();
    f.ok(&["sync", "--locked", "--offline"]);
    assert_eq!(f.lock(), lock);
    for bad in [
        two.replace("replace_text", "remove_sentence"),
        two.replace("version = 1", "version = 8"),
        skill_patch("missing", "oops", 1, 1),
    ] {
        write(&f.project.join("two.skillpatch.toml"), &bad);
        f.fail(&["sync"], "does not apply");
        assert_eq!(f.lock(), lock);
        assert_eq!(f.content("alpha"), "alpha final\n");
    }
}

#[test]
fn markdown_failure_ambiguity_and_locked_effective_tree_mismatch_are_safe() {
    let f = Fixture::new();
    f.commit_alpha("# Title\n\n## Tools\nold tool\n");
    f.manifest(
        "['plugins/tools/skills/alpha']",
        "patches = ['edit.skillpatch.toml']",
    );
    let patch =
        skill_patch("old tool", "new tool", 1, 1).replace("section = []", "section = ['Tools']");
    write(&f.project.join("edit.skillpatch.toml"), &patch);
    f.ok(&["sync"]);
    let lock = f.lock();
    let link = fs::read_link(f.project.join("target-a/alpha")).unwrap();
    f.commit_alpha("# Title\n\n## Tools\nold tool\n\n## Tools\nold tool\n");
    f.fail(&["update"], "ambiguous heading path");
    assert_eq!(f.lock(), lock);
    assert_eq!(
        fs::read_link(f.project.join("target-a/alpha")).unwrap(),
        link
    );
    let mut value: toml::Value = toml::from_str(std::str::from_utf8(&lock).unwrap()).unwrap();
    value["sources"]["tools"]["tree_sha256"] = toml::Value::String("0".repeat(64));
    write(
        &f.project.join("skills.lock"),
        &toml::to_string(&value).unwrap(),
    );
    f.fail(
        &["sync", "--locked", "--offline"],
        "differs from the lockfile",
    );
    assert_eq!(f.content("alpha"), "# Title\n\n## Tools\nnew tool\n");
}

#[test]
fn generate_from_patched_locked_offline_baseline_and_append_exactly() {
    let f = Fixture::new();
    let upstream = "# Title\n\n## Tools\nUse old tool here.\n\n## Verification\nCheck results.\n";
    f.commit_alpha(upstream);
    f.manifest(
        "['plugins/tools/skills/alpha']",
        "patches = ['existing.skillpatch.toml']",
    );
    write(
        &f.project.join("existing.skillpatch.toml"),
        &skill_patch("old tool", "existing tool", 1, 1)
            .replace("section = []", "section = ['Tools']"),
    );
    f.ok(&["sync"]);
    let baseline = f.content("alpha");
    let edited = baseline.replace("existing tool", "preferred tool").replace(
        "Check results.\n",
        "Check results.\n\n## Local preferences\n\nUse local tools.\n",
    );
    write(&f.project.join("edited.md"), &edited);
    let lock = f.lock();
    let manifest = fs::read(f.project.join("skills.toml")).unwrap();
    let state = fs::read(f.project.join("store/state.json")).unwrap();
    let guard = fs::read(f.project.join("store/sync.lock")).unwrap();
    let link = fs::read_link(f.project.join("target-a/alpha")).unwrap();
    fs::rename(&f.repo, f.temp.path().join("offline-upstream")).unwrap();
    let output = f.ok(&[
        "patch",
        "generate",
        "tools",
        "plugins/tools/skills/alpha/SKILL.md",
        "edited.md",
        "--output",
        "generated.skillpatch.toml",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Baseline tools@"));
    assert!(stdout.contains("Existing patch existing.skillpatch.toml sha256"));
    assert!(stdout.contains("Baseline document"));
    assert!(stdout.contains("scope [\"Tools\"]"));
    assert!(stdout.contains("scope [\"Verification\"]"));
    let generated = fs::read(f.project.join("generated.skillpatch.toml")).unwrap();
    assert_eq!(f.lock(), lock);
    assert_eq!(fs::read(f.project.join("skills.toml")).unwrap(), manifest);
    assert_eq!(fs::read(f.project.join("store/state.json")).unwrap(), state);
    assert_eq!(fs::read(f.project.join("store/sync.lock")).unwrap(), guard);
    assert_eq!(
        fs::read_link(f.project.join("target-a/alpha")).unwrap(),
        link
    );
    assert_eq!(f.content("alpha"), baseline);
    f.fail(
        &[
            "patch",
            "generate",
            "tools",
            "plugins/tools/skills/alpha/SKILL.md",
            "edited.md",
            "--output",
            "generated.skillpatch.toml",
        ],
        "creating",
    );
    assert_eq!(
        fs::read(f.project.join("generated.skillpatch.toml")).unwrap(),
        generated
    );
    f.manifest(
        "['plugins/tools/skills/alpha']",
        "patches = ['existing.skillpatch.toml', 'generated.skillpatch.toml']",
    );
    f.ok(&["sync", "--offline"]);
    assert_eq!(f.content("alpha"), edited);
    f.ok(&["sync", "--locked", "--offline"]);
    assert_eq!(f.content("alpha"), edited);
}

#[test]
fn generation_unsupported_changes_stale_inputs_and_output_aliases_write_nothing() {
    let f = Fixture::new();
    let base = "---\nname: alpha\n---\n# Title\n\n## Tools\nUse old tool.\n\n```\nold code\n```\n";
    f.commit_alpha(base);
    f.manifest(
        "['plugins/tools/skills/alpha']",
        "patches = ['existing.skillpatch.toml']",
    );
    let patch = skill_patch("old tool", "existing tool", 1, 1);
    write(&f.project.join("existing.skillpatch.toml"), &patch);
    f.ok(&["sync"]);
    let baseline = f.content("alpha");
    let lock = f.lock();
    let link = fs::read_link(f.project.join("target-a/alpha")).unwrap();
    for edited in [
        baseline.replace("name: alpha", "name: custom"),
        baseline.replace("old code", "new code"),
        baseline.replace("Use existing", "Use *existing*"),
        baseline.replace("## Tools", "## Renamed"),
        baseline.trim_end().to_owned(),
    ] {
        write(&f.project.join("edited.md"), &edited);
        f.fail(
            &[
                "patch",
                "generate",
                "tools",
                "plugins/tools/skills/alpha/SKILL.md",
                "edited.md",
                "--output",
                "failed.skillpatch.toml",
            ],
            "use a Git patch",
        );
        assert!(!f.project.join("failed.skillpatch.toml").exists());
    }
    write(
        &f.project.join("edited.md"),
        &baseline.replace("existing tool", "preferred tool"),
    );
    for output in [
        "wrong.toml",
        "store/unsafe.skillpatch.toml",
        "target-a/unsafe.skillpatch.toml",
        "target-a/alpha/unsafe.skillpatch.toml",
    ] {
        f.fail(
            &[
                "patch",
                "generate",
                "tools",
                "plugins/tools/skills/alpha/SKILL.md",
                "edited.md",
                "--output",
                output,
            ],
            if output == "wrong.toml" {
                "must end"
            } else {
                "unsafe output"
            },
        );
        assert!(!f.project.join(output).exists());
    }
    std::os::unix::fs::symlink(f.project.join("store"), f.project.join("cache-alias")).unwrap();
    f.fail(
        &[
            "patch",
            "generate",
            "tools",
            "plugins/tools/skills/alpha/SKILL.md",
            "edited.md",
            "--output",
            "cache-alias/unsafe.skillpatch.toml",
        ],
        "unsafe output",
    );
    std::os::unix::fs::symlink(
        f.project.join("skills.toml"),
        f.project.join("manifest-alias.skillpatch.toml"),
    )
    .unwrap();
    f.fail(
        &[
            "patch",
            "generate",
            "tools",
            "plugins/tools/skills/alpha/SKILL.md",
            "edited.md",
            "--output",
            "manifest-alias.skillpatch.toml",
        ],
        "collides",
    );
    for file in [
        "../SKILL.md",
        "/SKILL.md",
        "plugins/./tools/skills/alpha/SKILL.md",
        ".git/config",
        "plugins/tools/skills/alpha/asset.txt",
    ] {
        f.fail(
            &[
                "patch",
                "generate",
                "tools",
                file,
                "edited.md",
                "--output",
                "failed.skillpatch.toml",
            ],
            "source-relative Markdown path",
        );
    }
    write(
        &f.project.join("existing.skillpatch.toml"),
        &format!("{patch}\n# changed raw bytes\n"),
    );
    f.fail(
        &[
            "patch",
            "generate",
            "tools",
            "plugins/tools/skills/alpha/SKILL.md",
            "edited.md",
            "--output",
            "failed.skillpatch.toml",
        ],
        "manifest/patches changed",
    );
    write(&f.project.join("existing.skillpatch.toml"), &patch);
    f.manifest("['*']", "patches = ['existing.skillpatch.toml']");
    f.fail(
        &[
            "patch",
            "generate",
            "tools",
            "plugins/tools/skills/alpha/SKILL.md",
            "edited.md",
            "--output",
            "failed.skillpatch.toml",
        ],
        "manifest/patches changed",
    );
    assert!(!f.project.join("failed.skillpatch.toml").exists());
    assert_eq!(f.lock(), lock);
    assert_eq!(f.content("alpha"), baseline);
    assert_eq!(
        fs::read_link(f.project.join("target-a/alpha")).unwrap(),
        link
    );
}

#[test]
fn generation_local_revision_cache_mismatch_and_pending_journal_are_rejected() {
    let f = Fixture::new();
    write(
        &f.project.join("skills.toml"),
        &format!(
            "version = 1\nprefix = 'store'\ntargets = ['target-a']\n[sources.tools]\npath = '{}'\n",
            f.repo.display()
        ),
    );
    f.ok(&["sync", "--offline"]);
    write(&f.project.join("edited.md"), "alpha custom\n");
    let args = [
        "patch",
        "generate",
        "tools",
        "plugins/tools/skills/alpha/SKILL.md",
        "edited.md",
        "--output",
        "out.skillpatch.toml",
    ];
    let lock = f.lock();
    write(
        &f.repo.join("plugins/tools/skills/alpha/SKILL.md"),
        "alpha changed\n",
    );
    f.fail(&args, "differs from the lockfile");
    write(
        &f.repo.join("plugins/tools/skills/alpha/SKILL.md"),
        "alpha v1\n",
    );
    let link = fs::read_link(f.project.join("target-a/alpha")).unwrap();
    write(&link.join("SKILL.md"), "cache tampered\n");
    f.fail(&args, "cached snapshot");
    write(&link.join("SKILL.md"), "alpha v1\n");
    let mut parsed: toml::Value = toml::from_str(std::str::from_utf8(&lock).unwrap()).unwrap();
    parsed["sources"]["tools"]["tree_sha256"] = toml::Value::String("f".repeat(64));
    write(
        &f.project.join("skills.lock"),
        &toml::to_string(&parsed).unwrap(),
    );
    f.fail(&args, "differs from the lockfile");
    fs::write(f.project.join("skills.lock"), &lock).unwrap();
    write(
        &f.project.join("store/transaction.json"),
        "must not recover or parse this",
    );
    f.fail(&args, "generation does not recover");
    assert_eq!(
        fs::read_to_string(f.project.join("store/transaction.json")).unwrap(),
        "must not recover or parse this"
    );
    assert_eq!(f.lock(), lock);
    assert!(!f.project.join("out.skillpatch.toml").exists());
}

#[test]
fn generation_cwd_paths_lock_membership_and_no_installation_state_required() {
    let f = Fixture::new();
    f.manifest("['plugins/tools/skills/alpha']", "");
    f.ok(&["sync"]);
    fs::remove_file(f.project.join("store/state.json")).unwrap();
    fs::remove_file(f.project.join("store/sync.lock")).unwrap();
    fs::remove_dir_all(f.project.join("target-a")).unwrap();
    fs::remove_dir_all(f.project.join("target-b")).unwrap();
    let elsewhere = f.temp.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    write(&elsewhere.join("edited.md"), "alpha custom\n");
    let lock = f.lock();
    let output = Command::new(env!("CARGO_BIN_EXE_skills-sync"))
        .current_dir(&elsewhere)
        .args([
            "--manifest",
            f.project.join("skills.toml").to_str().unwrap(),
            "patch",
            "generate",
            "tools",
            "plugins/tools/skills/alpha/SKILL.md",
            "edited.md",
            "--output",
            "out.skillpatch.toml",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(elsewhere.join("out.skillpatch.toml").is_file());
    assert!(!f.project.join("out.skillpatch.toml").exists());
    assert!(!f.project.join("store/state.json").exists());
    assert!(!f.project.join("store/sync.lock").exists());
    assert!(!f.project.join("target-a").exists());
    assert_eq!(f.lock(), lock);
    let mut value: toml::Value = toml::from_str(std::str::from_utf8(&lock).unwrap()).unwrap();
    let extra = value["sources"]["tools"].clone();
    value["sources"]
        .as_table_mut()
        .unwrap()
        .insert("extra".into(), extra);
    write(
        &f.project.join("skills.lock"),
        &toml::to_string(&value).unwrap(),
    );
    write(&f.project.join("edited.md"), "alpha custom\n");
    f.fail(
        &[
            "patch",
            "generate",
            "tools",
            "plugins/tools/skills/alpha/SKILL.md",
            "edited.md",
            "--output",
            "out.skillpatch.toml",
        ],
        "source membership differs",
    );
    assert!(!f.project.join("out.skillpatch.toml").exists());
}

#[test]
fn generation_diff_limit_fails_without_output_or_installation_changes() {
    let f = Fixture::new();
    let base = format!("# Title\n\n## A\n{}\n", "a".repeat(1024));
    f.commit_alpha(&base);
    f.manifest("['plugins/tools/skills/alpha']", "");
    f.ok(&["sync"]);
    let lock = f.lock();
    write(
        &f.project.join("edited.md"),
        &base.replace(&"a".repeat(1024), &"b".repeat(1024)),
    );
    let output = f.cli(&[
        "patch",
        "generate",
        "tools",
        "plugins/tools/skills/alpha/SKILL.md",
        "edited.md",
        "--output",
        "out.skillpatch.toml",
    ]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("cell limit"));
    assert!(error.contains("use a Git patch"));
    assert!(error.contains("4:1"));
    assert!(!f.project.join("out.skillpatch.toml").exists());
    assert_eq!(f.lock(), lock);
    assert_eq!(f.content("alpha"), base);
}

#[test]
fn generation_semantic_manifest_guard_symlink_documents_and_prior_targets() {
    use fs2::FileExt;
    let f = Fixture::new();
    std::os::unix::fs::symlink("plugins/tools/skills/alpha", f.repo.join("alias")).unwrap();
    std::os::unix::fs::symlink(
        "plugins/tools/skills/alpha/SKILL.md",
        f.repo.join("alias.md"),
    )
    .unwrap();
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "aliases"]);
    f.manifest("['plugins/tools/skills/alpha']", "");
    f.ok(&["sync"]);
    let manifest = fs::read_to_string(f.project.join("skills.toml")).unwrap();
    write(
        &f.project.join("skills.toml"),
        &format!("{manifest}\n# semantic fingerprint intentionally unchanged\n"),
    );
    write(&f.project.join("edited.md"), "alpha custom\n");
    let args = [
        "patch",
        "generate",
        "tools",
        "plugins/tools/skills/alpha/SKILL.md",
        "edited.md",
        "--output",
        "out.skillpatch.toml",
    ];
    let guard = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.project.join("store/sync.lock"))
        .unwrap();
    guard.try_lock_exclusive().unwrap();
    f.fail(&args, "another sync");
    drop(guard);
    for file in ["alias.md", "alias/SKILL.md"] {
        f.fail(
            &[
                "patch",
                "generate",
                "tools",
                file,
                "edited.md",
                "--output",
                "out.skillpatch.toml",
            ],
            "symlink document or ancestor",
        );
    }
    std::os::unix::fs::symlink(
        f.project.join("skills.lock"),
        f.project.join("lock-alias.skillpatch.toml"),
    )
    .unwrap();
    f.fail(
        &[
            "patch",
            "generate",
            "tools",
            "plugins/tools/skills/alpha/SKILL.md",
            "edited.md",
            "--output",
            "lock-alias.skillpatch.toml",
        ],
        "collides",
    );
    let state_path = f.project.join("store/state.json");
    let state_bytes = fs::read(&state_path).unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&state_bytes).unwrap();
    let past_target = f.project.join("past-target");
    fs::create_dir(&past_target).unwrap();
    state["targets"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(past_target));
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    f.fail(
        &[
            "patch",
            "generate",
            "tools",
            "plugins/tools/skills/alpha/SKILL.md",
            "edited.md",
            "--output",
            "past-target/unsafe.skillpatch.toml",
        ],
        "unsafe output",
    );
    assert!(!past_target.join("unsafe.skillpatch.toml").exists());
    f.ok(&args);
    assert!(f.project.join("out.skillpatch.toml").is_file());
    assert_eq!(f.content("alpha"), "alpha v1\n");
}
