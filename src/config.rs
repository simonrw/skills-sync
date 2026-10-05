use crate::util;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<PathBuf>,
    pub targets: Vec<PathBuf>,
    pub sources: BTreeMap<String, Source>,
}
fn default_prefix(manifest_path: &Path) -> Result<PathBuf> {
    let root = match std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        Some(root) => root,
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .context("HOME must be a nonempty absolute path when XDG_STATE_HOME is unset, empty, or relative")?;
            home.join(".local/state")
        }
    };
    let prefix = root
        .join("skills-sync")
        .join(util::hash(util::path_str(manifest_path)?.as_bytes()));
    util::resolved_path(
        manifest_path.parent().context("manifest has no parent")?,
        &prefix,
    )
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub repo: Option<String>,
    pub path: Option<PathBuf>,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    #[serde(default = "all_skills")]
    pub skills: Vec<String>,
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
    #[serde(default)]
    pub patches: Vec<PathBuf>,
}
fn all_skills() -> Vec<String> {
    vec!["*".into()]
}

pub struct Loaded {
    pub manifest_path: PathBuf,
    pub manifest_bytes: Vec<u8>,
    pub manifest: Manifest,
    pub base: PathBuf,
    pub prefix: PathBuf,
    pub targets: Vec<PathBuf>,
    pub lock: PathBuf,
}

impl Loaded {
    pub fn read(path: &Path) -> Result<Self> {
        let path = fs::canonicalize(path)
            .with_context(|| format!("reading {}; run `skills-sync init` first", path.display()))?;
        let base = path
            .parent()
            .context("manifest has no parent")?
            .to_path_buf();
        let manifest_bytes = fs::read(&path)?;
        let manifest: Manifest =
            toml::from_str(std::str::from_utf8(&manifest_bytes)?).context("invalid manifest")?;
        if manifest.version != 1 {
            bail!("unsupported manifest version {}", manifest.version);
        }
        if manifest.targets.is_empty() {
            bail!("at least one target is required");
        }
        let prefix = match &manifest.prefix {
            Some(prefix) => util::resolved_path(&base, prefix)?,
            None => default_prefix(&path)?,
        };
        let targets = manifest
            .targets
            .iter()
            .map(|p| util::resolved_path(&base, p))
            .collect::<Result<Vec<_>>>()?;
        let mut seen = BTreeSet::new();
        for target in &targets {
            if !seen.insert(target) {
                bail!("duplicate target {}", target.display());
            }
            if target.starts_with(&prefix) || prefix.starts_with(target) {
                bail!("prefix and targets must not overlap");
            }
            for other in &targets {
                if other != target && (target.starts_with(other) || other.starts_with(target)) {
                    bail!("targets must not overlap");
                }
            }
        }
        for (id, source) in &manifest.sources {
            util::valid_name(id)?;
            if source.repo.is_some() == source.path.is_some() {
                bail!("source {id}: specify exactly one of repo or path");
            }
            if source.path.is_some() && source.reference.is_some() {
                bail!("source {id}: ref requires repo");
            }
            if source.skills.is_empty() {
                bail!("source {id}: skills cannot be empty; remove the source to uninstall it");
            }
            if source
                .repo
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.starts_with('-'))
                || source
                    .reference
                    .as_ref()
                    .is_some_and(|s| s.is_empty() || s.starts_with('-'))
            {
                bail!("source {id}: invalid repo or ref");
            }
            for name in source.rename.values() {
                util::valid_name(name)?;
            }
        }
        let lock = path.with_extension("lock");
        if lock == path {
            bail!("manifest cannot use the .lock extension; it would collide with its lockfile");
        }
        for reserved in ["state.json", "sync.lock", "transaction.json"] {
            if path == prefix.join(reserved) || lock == prefix.join(reserved) {
                bail!("manifest or lockfile collides with reserved prefix file {reserved}");
            }
        }
        Ok(Self {
            manifest_path: path,
            manifest_bytes,
            manifest,
            base,
            prefix,
            targets,
            lock,
        })
    }
    pub fn repo(&self, source: &Source) -> Result<String> {
        let repo = source.repo.as_ref().context("not a Git source")?;
        if repo.contains(':') {
            Ok(repo.clone())
        } else {
            Ok(util::path_str(&util::absolute(&self.base, Path::new(repo))?)?.to_owned())
        }
    }
}
