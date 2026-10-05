#![cfg(unix)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("source/alpha")).unwrap();
        fs::write(temp.path().join("source/alpha/SKILL.md"), "alpha\n").unwrap();
        fs::create_dir(temp.path().join("home")).unwrap();
        Self { temp }
    }

    fn manifest(&self, name: &str, prefix: Option<&str>) -> PathBuf {
        let project = self.temp.path().join(name);
        fs::create_dir(&project).unwrap();
        let path = project.join("skills.toml");
        let prefix = prefix
            .map(|p| format!("prefix = '{p}'\n"))
            .unwrap_or_default();
        fs::write(
            &path,
            format!(
                "version = 1\n{prefix}targets = ['target']\n[sources.local]\npath = '{}'\n",
                self.temp.path().join("source").display()
            ),
        )
        .unwrap();
        path
    }

    fn command(&self, manifest: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_skills-sync"));
        command
            .current_dir(self.temp.path())
            .env("HOME", self.temp.path().join("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("XDG_STATE_HOME")
            .arg("--manifest")
            .arg(manifest);
        command
    }
}

fn success(command: &mut Command) -> Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn prefixes(state_home: &Path) -> Vec<PathBuf> {
    fs::read_dir(state_home.join("skills-sync"))
        .unwrap()
        .map(|entry| entry.unwrap().path().canonicalize().unwrap())
        .collect()
}

#[test]
fn xdg_prefixes_are_global_isolated_and_independent_of_working_directory() {
    let f = Fixture::new();
    let manifest = f.manifest("first", None);
    let state_home = f.temp.path().join("state");
    success(
        f.command(&manifest)
            .env("XDG_STATE_HOME", &state_home)
            .env_remove("HOME")
            .arg("sync"),
    );
    let prefix = prefixes(&state_home).pop().unwrap();
    assert!(prefix.join("state.json").is_file());
    let project = manifest.parent().unwrap();
    assert!(fs::read_link(project.join("target/alpha"))
        .unwrap()
        .starts_with(&prefix));
    assert_eq!(
        fs::read_to_string(project.join("target/alpha/SKILL.md")).unwrap(),
        "alpha\n"
    );
    assert!(!project.join(".skills-sync").exists());
    assert!(!f.temp.path().join(".skills-sync").exists());
    let lock = fs::read(manifest.with_extension("lock")).unwrap();

    let second = f.manifest("second", None);
    success(
        f.command(&second)
            .env("XDG_STATE_HOME", &state_home)
            .arg("sync"),
    );
    assert_eq!(prefixes(&state_home).len(), 2);
    assert!(prefix.join("state.json").is_file());

    let output = success(
        f.command(&manifest)
            .current_dir(second.parent().unwrap())
            .env("XDG_STATE_HOME", &state_home)
            .args(["sync", "--locked", "--offline"]),
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("0 link changes"));
    assert_eq!(fs::read(manifest.with_extension("lock")).unwrap(), lock);
    let output = success(
        f.command(&manifest)
            .env("XDG_STATE_HOME", &state_home)
            .args(["list", "--json"]),
    );
    let skills: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(skills[0]["name"], "alpha");

    let third = f.manifest("third", None);
    fs::copy(
        manifest.with_extension("lock"),
        third.with_extension("lock"),
    )
    .unwrap();
    let alternate_state_home = f.temp.path().join("alternate-state");
    success(
        f.command(&third)
            .env("XDG_STATE_HOME", &alternate_state_home)
            .args(["sync", "--locked", "--offline"])
            .current_dir(project),
    );
    assert_eq!(fs::read(third.with_extension("lock")).unwrap(), lock);
    assert!(prefixes(&alternate_state_home)[0]
        .join("state.json")
        .is_file());
}

#[test]
fn unset_empty_and_relative_xdg_state_home_fall_back_to_home() {
    for xdg in [None, Some(""), Some("relative/state")] {
        let f = Fixture::new();
        let manifest = f.manifest("project", None);
        let mut command = f.command(&manifest);
        if let Some(value) = xdg {
            command.env("XDG_STATE_HOME", value);
        }
        success(command.arg("sync"));
        let prefix = prefixes(&f.temp.path().join("home/.local/state"))
            .pop()
            .unwrap();
        assert!(prefix.join("state.json").is_file());
        assert!(
            fs::read_link(manifest.parent().unwrap().join("target/alpha"))
                .unwrap()
                .starts_with(prefix)
        );
        assert!(!f.temp.path().join("relative").exists());
    }
}

#[test]
fn explicit_prefix_does_not_require_home_or_valid_xdg_state_home() {
    let f = Fixture::new();
    let manifest = f.manifest("project", Some("store"));
    success(
        f.command(&manifest)
            .env_remove("HOME")
            .env("XDG_STATE_HOME", "relative")
            .arg("sync"),
    );
    let prefix = manifest
        .parent()
        .unwrap()
        .join("store")
        .canonicalize()
        .unwrap();
    assert!(prefix.join("state.json").is_file());
    assert!(
        fs::read_link(manifest.parent().unwrap().join("target/alpha"))
            .unwrap()
            .starts_with(prefix)
    );
}

#[test]
fn default_prefix_rejects_missing_empty_or_relative_home() {
    for home in [None, Some(""), Some("relative/home")] {
        let f = Fixture::new();
        let manifest = f.manifest("project", None);
        let mut command = f.command(&manifest);
        command.env_remove("HOME");
        if let Some(value) = home {
            command.env("HOME", value);
        }
        let output = command.arg("sync").output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("HOME"));
        assert!(!manifest.parent().unwrap().join(".skills-sync").exists());
    }
}

#[test]
fn init_leaves_prefix_unset_to_use_the_xdg_default() {
    let f = Fixture::new();
    let manifest = f.temp.path().join("skills.toml");
    success(f.command(&manifest).arg("init"));
    let parsed: toml::Value = toml::from_str(&fs::read_to_string(manifest).unwrap()).unwrap();
    assert!(parsed.get("prefix").is_none());
}
