use crate::{
    config::{Loaded, Source},
    patch, util,
};
use anyhow::{bail, Context, Result};
use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Patch {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LockedSource {
    pub kind: String,
    pub upstream: String,
    pub reference: Option<String>,
    /// A full Git commit, or the SHA-256 of a local source tree.
    pub revision: String,
    pub patches: Vec<Patch>,
    /// Repository-relative directory -> installed name.
    pub skills: BTreeMap<String, String>,
    pub tree_sha256: String,
}

pub struct Prepared {
    pub locked: LockedSource,
    pub tree: PathBuf,
    pub reports: Vec<patch::PatchReport>,
}

pub struct PatchInput {
    pub info: Patch,
    pub bytes: Vec<u8>,
}
pub type PatchInputs = BTreeMap<String, Vec<PatchInput>>;

pub fn load_patch_inputs(config: &Loaded) -> Result<PatchInputs> {
    config
        .manifest
        .sources
        .iter()
        .map(|(id, source)| {
            let stack = source
                .patches
                .iter()
                .map(|path| {
                    let full = util::absolute(&config.base, path)?;
                    let bytes = if path.to_string_lossy().ends_with(".skillpatch.toml") {
                        patch::read_bounded(&full)?
                    } else {
                        fs::read(&full)
                            .with_context(|| format!("reading patch {}", full.display()))?
                    };
                    Ok(PatchInput {
                        info: Patch {
                            path: path.clone(),
                            sha256: util::hash(&bytes),
                        },
                        bytes,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((id.clone(), stack))
        })
        .collect()
}

pub fn locked_baseline(
    config: &Loaded,
    id: &str,
    lock: &crate::lock::LockFile,
    inputs: &PatchInputs,
) -> Result<Prepared> {
    let spec = config
        .manifest
        .sources
        .get(id)
        .with_context(|| format!("unknown source ID {id:?}"))?;
    let old = lock.source(id).context("source missing from lockfile")?;
    let prepared = prepare(config, id, spec, Some(old), false, true, &inputs[id])?;
    if prepared.locked != *old {
        bail!("resolved skills, local source content or effective tree differs from the lockfile; run sync and review the lockfile");
    }
    Ok(prepared)
}

pub fn prepare(
    config: &Loaded,
    id: &str,
    source: &Source,
    old: Option<&LockedSource>,
    update: bool,
    offline: bool,
    patch_data: &[PatchInput],
) -> Result<Prepared> {
    let patch_info = patch_data
        .iter()
        .map(|p| p.info.clone())
        .collect::<Vec<_>>();
    let store = config.prefix.join("sources").join(id);
    fs::create_dir_all(&store)?;
    let temp = tempfile::tempdir_in(&store)?;
    let tree = temp.path().join("tree");
    fs::create_dir(&tree)?;
    let (kind, upstream, revision) = if let Some(local) = &source.path {
        let local = fs::canonicalize(util::absolute(&config.base, local)?)?;
        if !local.is_dir() {
            bail!("local source must be a directory");
        }
        // Avoid recursively snapshotting our own cache or installed links.
        if config.prefix.starts_with(&local)
            || local.starts_with(&config.prefix)
            || config
                .targets
                .iter()
                .any(|t| t.starts_with(&local) || local.starts_with(t))
        {
            bail!("local source must not overlap prefix or targets");
        }
        copy_tree(&local, &tree)?;
        (
            "local".to_owned(),
            util::path_str(&local)?.to_owned(),
            tree_hash(&tree)?,
        )
    } else {
        let upstream = config.repo(source)?;
        let mirror = mirror(config, &upstream, offline)?;
        let previous = old.filter(|o| {
            o.kind == "git" && o.upstream == upstream && o.reference == source.reference
        });
        let revision = if !update && previous.is_some() {
            let commit = &previous.context("previous revision")?.revision;
            validate_revision(commit)?;
            if util::git(
                Some(&mirror),
                &["cat-file", "-e", &format!("{commit}^{{commit}}")],
            )
            .is_err()
            {
                if offline {
                    bail!("commit {commit} is not cached; retry without --offline");
                }
                util::git(Some(&mirror), &["fetch", "origin", commit])?;
            }
            commit.clone()
        } else {
            if offline {
                bail!("source {id} needs resolution; run sync without --offline first");
            }
            util::git(
                Some(&mirror),
                &[
                    "fetch",
                    "--force",
                    "origin",
                    "+refs/heads/*:refs/heads/*",
                    "+refs/tags/*:refs/tags/*",
                    "+HEAD:refs/skills-sync/default",
                ],
            )?;
            let reference = source
                .reference
                .as_deref()
                .unwrap_or("refs/skills-sync/default");
            let output = util::git(
                Some(&mirror),
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{reference}^{{commit}}"),
                ],
            )?;
            let commit = String::from_utf8(output.stdout)?.trim().to_owned();
            validate_revision(&commit)?;
            commit
        };
        let listing = util::git(Some(&mirror), &["ls-tree", "-r", &revision])?;
        if listing
            .stdout
            .split(|b| *b == b'\n')
            .any(|l| l.starts_with(b"160000 "))
        {
            bail!("source {id} contains submodules; declare each submodule as a separate source");
        }
        let archive = util::git(Some(&mirror), &["archive", "--format=tar", &revision])?;
        unpack(&archive.stdout, &tree)?;
        ("git".to_owned(), upstream, revision)
    };
    validate_links(&tree)?;
    // Initialize an isolated index so git apply does not discover an enclosing repo.
    util::git(Some(&tree), &["init", "--quiet"])?;
    let mut reports = Vec::new();
    for input in patch_data {
        reports.push(
            patch::apply_file(&tree, &input.info.path, &input.bytes).with_context(|| {
                format!(
                    "patch {} does not apply to {id}@{revision}",
                    input.info.path.display()
                )
            })?,
        );
    }
    fs::remove_dir_all(tree.join(".git"))?;
    validate_links(&tree)?;
    let skills = select(&tree, source)?;
    let locked = LockedSource {
        kind,
        upstream,
        reference: source.reference.clone(),
        revision,
        patches: patch_info,
        skills,
        tree_sha256: tree_hash(&tree)?,
    };
    let generation = util::json_hash(&(
        &locked.kind,
        &locked.upstream,
        &locked.revision,
        &locked.patches,
        &locked.tree_sha256,
    ))?;
    let destination = store.join(generation);
    if destination.exists() {
        if tree_hash(&destination.join("tree"))? != locked.tree_sha256 {
            bail!(
                "cached snapshot {} was modified; remove that snapshot and retry",
                destination.display()
            );
        }
    } else {
        let origin = serde_json::json!({"kind": locked.kind, "upstream": locked.upstream, "revision": locked.revision, "patches": locked.patches, "tree_sha256": locked.tree_sha256});
        util::atomic_write(
            &temp.path().join("source.json"),
            &serde_json::to_vec_pretty(&origin)?,
        )?;
        fs::rename(temp.path(), &destination)?;
    }
    Ok(Prepared {
        locked,
        tree: destination.join("tree"),
        reports,
    })
}

fn validate_revision(commit: &str) -> Result<()> {
    if ![40, 64].contains(&commit.len()) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid commit hash {commit:?}");
    }
    Ok(())
}

fn mirror(config: &Loaded, upstream: &str, offline: bool) -> Result<PathBuf> {
    let cache = config.prefix.join("repos");
    if !offline {
        fs::create_dir_all(&cache)?;
    }
    let path = cache.join(format!("{}.git", util::hash(upstream.as_bytes())));
    if !path.exists() {
        if offline {
            bail!("repository {upstream} is not cached");
        }
        let staging = tempfile::tempdir_in(&cache)?;
        util::git(
            None,
            &[
                "clone",
                "--mirror",
                "--",
                upstream,
                util::path_str(&staging.path().join("repo"))?,
            ],
        )?;
        fs::rename(staging.path().join("repo"), &path)?;
    }
    Ok(path)
}

fn entries(root: &Path) -> impl Iterator<Item = walkdir::Result<walkdir::DirEntry>> {
    WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || e.file_name() != ".git")
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in entries(from) {
        let entry = entry?;
        if entry.depth() == 0 {
            continue;
        }
        let target = to.join(entry.path().strip_prefix(from)?);
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            fs::create_dir_all(&target)?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(&target, metadata.permissions())?;
        } else if metadata.file_type().is_symlink() {
            symlink(&fs::read_link(entry.path())?, &target)?;
        } else {
            bail!("unsupported file type at {}", entry.path().display());
        }
    }
    validate_links(to)
}

pub fn tree_hash(root: &Path) -> Result<String> {
    let mut records = BTreeMap::new();
    for entry in entries(root) {
        let entry = entry?;
        if entry.depth() == 0 || entry.file_type().is_dir() {
            continue;
        }
        let name = util::path_str(entry.path().strip_prefix(root)?)?.to_owned();
        let metadata = fs::symlink_metadata(entry.path())?;
        let record = if metadata.file_type().is_symlink() {
            (
                "link",
                util::path_str(&fs::read_link(entry.path())?)?.to_owned(),
                false,
            )
        } else if metadata.is_file() {
            (
                "file",
                util::hash(&fs::read(entry.path())?),
                executable(&metadata),
            )
        } else {
            bail!("unsupported file type at {}", entry.path().display());
        };
        records.insert(name, record);
    }
    util::json_hash(&records)
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> bool {
    false
}

pub fn symlink(from: &Path, to: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(from, to)?;
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(from, to)?;
    }
    Ok(())
}

fn validate_links(root: &Path) -> Result<()> {
    let root = fs::canonicalize(root)?;
    for entry in entries(&root) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            let target = fs::read_link(entry.path())?;
            if target.is_absolute() {
                bail!("absolute symlink in source: {}", entry.path().display());
            }
            let resolved = fs::canonicalize(entry.path()).with_context(|| {
                format!("dangling or cyclic symlink: {}", entry.path().display())
            })?;
            if !resolved.starts_with(&root) {
                bail!("symlink escapes source: {}", entry.path().display());
            }
        }
    }
    Ok(())
}

fn unpack(bytes: &[u8], root: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(bytes);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
            || path.components().any(|c| c.as_os_str() == ".git")
        {
            bail!("unsafe path in Git archive: {}", path.display());
        }
        let kind = entry.header().entry_type();
        // Git emits a global PAX record carrying the commit ID, not a file.
        if kind.is_pax_global_extensions() {
            continue;
        }
        if !(kind.is_dir() || kind.is_file() || kind.is_symlink()) {
            bail!("unsupported Git archive entry {}", path.display());
        }
        if kind.is_symlink() {
            let link = entry.link_name()?.context("missing symlink target")?;
            let parent = root
                .join(&path)
                .parent()
                .context("missing parent")?
                .to_path_buf();
            if link.is_absolute() || !util::absolute(&parent, &link)?.starts_with(root) {
                bail!("symlink escapes archive: {}", path.display());
            }
        }
        if !entry.unpack_in(root)? {
            bail!("archive entry escapes snapshot");
        }
    }
    Ok(())
}

fn select(root: &Path, source: &Source) -> Result<BTreeMap<String, String>> {
    let mut discovered = BTreeMap::new();
    for entry in entries(root) {
        let entry = entry?;
        if entry.file_name() == "SKILL.md" && entry.file_type().is_file() {
            let parent = entry.path().parent().context("SKILL.md has no directory")?;
            let path = parent.strip_prefix(root)?;
            let relative = if path.as_os_str().is_empty() {
                ".".to_owned()
            } else {
                util::path_str(path)?.replace('\\', "/")
            };
            let name = parent
                .file_name()
                .and_then(|s| s.to_str())
                .context("skill directory has no UTF-8 name")?
                .to_owned();
            // Root skills need an explicit rename because 'tree' is an implementation detail.
            let name = if relative == "." {
                ".".to_owned()
            } else {
                name
            };
            discovered.insert(relative, name);
        }
    }
    let mut selected = BTreeMap::new();
    for pattern in &source.skills {
        let matcher = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .with_context(|| format!("invalid selector {pattern:?}"))?
            .compile_matcher();
        let matches = discovered
            .iter()
            .filter(|(path, _)| pattern == "*" || matcher.is_match(path))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            bail!("selector {pattern:?} matches no skills (paths are relative to the source root)");
        }
        for (path, name) in matches {
            selected.insert(
                path.clone(),
                source.rename.get(path).unwrap_or(name).clone(),
            );
        }
    }
    for path in source.rename.keys() {
        if !selected.contains_key(path) {
            bail!("rename key {path:?} is not a selected skill");
        }
    }
    for (path, name) in &selected {
        if path == "." && name == "." {
            bail!("root SKILL.md requires rename = {{ \".\" = \"name\" }}");
        }
        util::valid_name(name)?;
    }
    Ok(selected)
}
