use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
};

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn json_hash(value: &impl Serialize) -> Result<String> {
    Ok(hash(&serde_json::to_vec(value)?))
}

pub fn absolute(base: &Path, path: &Path) -> Result<PathBuf> {
    let expanded = if path == Path::new("~") || path.starts_with("~/") {
        let home = std::env::var_os("HOME").context("HOME is required for ~ paths")?;
        PathBuf::from(home).join(path.strip_prefix("~")?)
    } else {
        path.to_path_buf()
    };
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    let mut normalized = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    Ok(normalized)
}

pub fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        bail!("invalid ID or install name {name:?}; use letters, digits, '.', '_' or '-'");
    }
    Ok(())
}

/// Resolve existing ancestors too, so symlink aliases cannot hide overlaps.
pub fn resolved_path(base: &Path, path: &Path) -> Result<PathBuf> {
    let full = absolute(base, path)?;
    let mut ancestor = full.as_path();
    let mut tail = Vec::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let mut resolved = fs::canonicalize(ancestor)?;
                for component in tail.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tail.push(
                    ancestor
                        .file_name()
                        .context("path has no existing ancestor")?
                        .to_os_string(),
                );
                ancestor = ancestor.parent().context("path has no parent")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

pub fn create_new(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().context("file has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(data)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn git(directory: Option<&Path>, args: &[&str]) -> Result<Output> {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    let output = command
        .args(args)
        .output()
        .context("running git; ensure Git is on PATH")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}
pub fn path_str(path: &Path) -> Result<&str> {
    path.to_str().context("paths must be valid UTF-8")
}
