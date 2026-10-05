use crate::{
    config::Loaded,
    source::{LockedSource, PatchInputs},
    util,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::ErrorKind};

#[derive(Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LockFile {
    version: u32,
    manifest_sha256: String,
    sources: BTreeMap<String, LockedSource>,
}
impl LockFile {
    pub fn new(fingerprint: String) -> Self {
        Self {
            version: 1,
            manifest_sha256: fingerprint,
            sources: BTreeMap::new(),
        }
    }
    pub fn source(&self, id: &str) -> Option<&LockedSource> {
        self.sources.get(id)
    }
    pub fn insert(&mut self, id: String, source: LockedSource) {
        self.sources.insert(id, source);
    }
    pub fn matches_manifest(&self, fingerprint: &str) -> bool {
        self.manifest_sha256 == fingerprint
    }
    pub fn require_current(config: &Loaded, inputs: &PatchInputs) -> Result<Self> {
        let lock = read(config)?.context("lockfile is missing; run sync first")?;
        if !lock.matches_manifest(&fingerprint(config, inputs)?)
            || lock.sources.keys().ne(config.manifest.sources.keys())
        {
            bail!("manifest/patches changed or source membership differs from lockfile; run sync and review the lockfile");
        }
        Ok(lock)
    }
}
pub fn read(config: &Loaded) -> Result<Option<LockFile>> {
    let bytes = match fs::read_to_string(&config.lock) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let lock: LockFile = toml::from_str(&bytes).context("invalid lockfile")?;
    if lock.version != 1 {
        bail!("unsupported lockfile version");
    }
    Ok(Some(lock))
}
pub fn fingerprint(config: &Loaded, inputs: &PatchInputs) -> Result<String> {
    let hashes = inputs
        .iter()
        .map(|(id, stack)| (id, stack.iter().map(|p| &p.info).collect::<Vec<_>>()))
        .collect::<BTreeMap<_, _>>();
    util::json_hash(&(&config.manifest, hashes))
}
