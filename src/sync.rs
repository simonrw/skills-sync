use crate::{
    config::Loaded,
    lock::{self, LockFile},
    source, util,
};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub struct Options {
    pub locked: bool,
    pub offline: bool,
    pub dry_run: bool,
    pub update: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Installed {
    name: String,
    source: String,
    upstream: String,
    revision: String,
    path: String,
    destination: PathBuf,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct State {
    version: u32,
    owner: PathBuf,
    targets: Vec<PathBuf>,
    links: BTreeMap<PathBuf, PathBuf>,
    skills: Vec<Installed>,
}

#[derive(Deserialize, Serialize)]
struct Journal {
    before: State,
    after: State,
    lock_before: Option<String>,
}

pub fn run(config: &Loaded, options: Options) -> Result<()> {
    fs::create_dir_all(&config.prefix)?;
    let guard = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.prefix.join("sync.lock"))?;
    guard
        .try_lock_exclusive()
        .context("another sync is already running for this prefix")?;
    recover(config)?;
    let previous: State = read_json(&config.prefix.join("state.json"))?.unwrap_or_else(|| State {
        version: 1,
        owner: config.lock.clone(),
        ..State::default()
    });
    validate_state(config, &previous)?;
    let old = lock::read(config)?;
    if let Some(ids) = &options.update {
        for id in ids {
            if !config.manifest.sources.contains_key(id) {
                bail!("unknown source ID {id:?}");
            }
        }
    }
    let inputs = source::load_patch_inputs(config)?;
    let fingerprint = lock::fingerprint(config, &inputs)?;
    if options.locked
        && old
            .as_ref()
            .is_none_or(|l| !l.matches_manifest(&fingerprint))
    {
        bail!("lockfile is missing or manifest/patches changed; run sync without --locked and review the lockfile");
    }
    let mut lock = LockFile::new(fingerprint);
    let mut reports = Vec::new();
    let mut next = State {
        version: 1,
        owner: config.lock.clone(),
        targets: config.targets.clone(),
        ..State::default()
    };
    let mut names = BTreeMap::new();
    for (id, spec) in &config.manifest.sources {
        eprintln!("Resolving {id} ...");
        let update = options
            .update
            .as_ref()
            .is_some_and(|ids| ids.is_empty() || ids.contains(id));
        let prepared = source::prepare(
            config,
            id,
            spec,
            old.as_ref().and_then(|l| l.source(id)),
            update,
            options.offline,
            &inputs[id],
        )
        .with_context(|| format!("preparing source {id}"))?;
        for (path, name) in &prepared.locked.skills {
            if let Some(other) = names.insert(name.clone(), format!("{id}:{path}")) {
                bail!("install name {name:?} collides between {other} and {id}:{path}; use rename");
            }
            let destination = if path == "." {
                prepared.tree.clone()
            } else {
                prepared.tree.join(path)
            };
            for target in &config.targets {
                next.links.insert(target.join(name), destination.clone());
            }
            next.skills.push(Installed {
                name: name.clone(),
                source: id.clone(),
                upstream: prepared.locked.upstream.clone(),
                revision: prepared.locked.revision.clone(),
                path: path.clone(),
                destination,
            });
        }
        reports.extend(prepared.reports);
        lock.insert(id.clone(), prepared.locked);
    }
    if options.locked && old.as_ref() != Some(&lock) {
        bail!("resolved skills or local source content differs from the lockfile; run sync without --locked");
    }
    // Validate the whole plan before touching any installed link.
    preflight(&previous.links, &next.links)?;
    for report in &reports {
        report.print(options.dry_run);
    }
    let mut changes = 0;
    for path in union(&previous.links, &next.links) {
        let actual = current_link(&path)?;
        let desired = next.links.get(&path);
        if actual.as_ref() == desired {
            continue;
        }
        changes += 1;
        if let Some(destination) = desired {
            println!("link {} -> {}", path.display(), destination.display());
        } else {
            println!("remove {}", path.display());
        }
    }
    if options.dry_run {
        println!(
            "Dry run: {} skills, {changes} link changes; lockfile and installed links unchanged",
            next.skills.len()
        );
        return Ok(());
    }
    let lock_bytes = toml::to_string_pretty(&lock)?;
    // Journal first; failed or interrupted installs can restore their previous links.
    let journal = Journal {
        before: previous,
        after: next.clone(),
        lock_before: read_optional(&config.lock)?,
    };
    let journal_path = config.prefix.join("transaction.json");
    util::atomic_write(&journal_path, &serde_json::to_vec_pretty(&journal)?)?;
    let result = (|| -> Result<()> {
        reconcile(&journal.before.links, &journal.after.links)?;
        if !options.locked {
            util::atomic_write(&config.lock, lock_bytes.as_bytes())?;
        }
        util::atomic_write(
            &config.prefix.join("state.json"),
            &serde_json::to_vec_pretty(&next)?,
        )?;
        fs::remove_file(&journal_path)?;
        Ok(())
    })();
    if let Err(error) = result {
        if let Err(rollback) = recover(config) {
            bail!("sync failed: {error:#}; rollback failed: {rollback:#}; transaction journal retained");
        }
        return Err(error.context("sync failed; previous installation restored"));
    }
    println!(
        "Synced {} skills to {} targets ({changes} link changes)",
        next.skills.len(),
        config.targets.len()
    );
    Ok(())
}

fn validate_state(config: &Loaded, state: &State) -> Result<()> {
    if state.version != 1 {
        bail!("unsupported installation state version");
    }
    if state.owner != config.lock {
        bail!(
            "prefix belongs to manifest {}; use a different prefix",
            state.owner.display()
        );
    }
    for (path, destination) in &state.links {
        if !state
            .targets
            .iter()
            .any(|t| path.parent() == Some(t.as_path()))
            || !destination.starts_with(config.prefix.join("sources"))
        {
            bail!("invalid managed link in installation state");
        }
        util::valid_name(
            path.file_name()
                .and_then(|s| s.to_str())
                .context("invalid managed link name")?,
        )?;
    }
    Ok(())
}

fn union(
    before: &BTreeMap<PathBuf, PathBuf>,
    after: &BTreeMap<PathBuf, PathBuf>,
) -> BTreeSet<PathBuf> {
    before.keys().chain(after.keys()).cloned().collect()
}

fn current_link(path: &Path) -> Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(Some(fs::read_link(path)?)),
        Ok(_) => bail!(
            "refusing to overwrite unmanaged file or directory {}",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn preflight(
    before: &BTreeMap<PathBuf, PathBuf>,
    after: &BTreeMap<PathBuf, PathBuf>,
) -> Result<()> {
    for path in union(before, after) {
        if let Some(actual) = current_link(&path)? {
            if before.get(&path) != Some(&actual) {
                bail!(
                    "refusing to overwrite unmanaged or externally changed symlink {}",
                    path.display()
                );
            }
        }
        // Fail early for a regular file blocking any target's ancestors.
        let mut ancestor = path.parent();
        while let Some(directory) = ancestor {
            if directory.exists() && !directory.is_dir() {
                bail!(
                    "target ancestor is not a directory: {}",
                    directory.display()
                );
            }
            ancestor = directory.parent();
        }
    }
    Ok(())
}

fn reconcile(
    before: &BTreeMap<PathBuf, PathBuf>,
    after: &BTreeMap<PathBuf, PathBuf>,
) -> Result<()> {
    for path in union(before, after) {
        let actual = current_link(&path)?;
        if actual.as_ref() == after.get(&path) {
            continue;
        }
        if actual.is_some() && actual.as_ref() != before.get(&path) {
            bail!("managed link changed during sync: {}", path.display());
        }
        if let Some(destination) = after.get(&path) {
            let parent = path.parent().context("target has no parent")?;
            fs::create_dir_all(parent)?;
            let staging = tempfile::tempdir_in(parent)?;
            let temporary = staging.path().join("link");
            source::symlink(destination, &temporary)?;
            // Each changed symlink is replaced atomically.
            fs::rename(&temporary, &path)?;
        } else if actual.is_some() {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn recover(config: &Loaded) -> Result<()> {
    let path = config.prefix.join("transaction.json");
    let Some(journal): Option<Journal> = read_json(&path)? else {
        return Ok(());
    };
    validate_state(config, &journal.before)?;
    validate_state(config, &journal.after)?;
    eprintln!("Recovering interrupted sync ...");
    for path in union(&journal.before.links, &journal.after.links) {
        if let Some(actual) = current_link(&path)? {
            if journal.before.links.get(&path) != Some(&actual)
                && journal.after.links.get(&path) != Some(&actual)
            {
                bail!("cannot recover externally changed link {}", path.display());
            }
        }
    }
    reconcile(&journal.after.links, &journal.before.links)?;
    if let Some(bytes) = &journal.lock_before {
        util::atomic_write(&config.lock, bytes.as_bytes())?;
    } else if config.lock.exists() {
        fs::remove_file(&config.lock)?;
    }
    util::atomic_write(
        &config.prefix.join("state.json"),
        &serde_json::to_vec_pretty(&journal.before)?,
    )?;
    fs::remove_file(&path)?;
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    read_optional(path)?
        .map(|s| {
            serde_json::from_str(&s).with_context(|| format!("invalid state {}", path.display()))
        })
        .transpose()
}

pub fn list(config: &Loaded, json: bool) -> Result<()> {
    if config.prefix.join("transaction.json").exists() {
        bail!("interrupted transaction; run sync to recover before listing");
    }
    let state: State = read_json(&config.prefix.join("state.json"))?
        .context("no installed state; run sync first")?;
    validate_state(config, &state)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&state.skills)?);
    } else {
        for skill in &state.skills {
            println!(
                "{}\t{}:{}\t{}@{}",
                skill.name, skill.source, skill.path, skill.upstream, skill.revision
            );
        }
    }
    Ok(())
}

pub fn installed_targets(config: &Loaded) -> Result<Vec<PathBuf>> {
    let Some(state): Option<State> = read_json(&config.prefix.join("state.json"))? else {
        return Ok(Vec::new());
    };
    validate_state(config, &state)?;
    Ok(state.targets)
}
