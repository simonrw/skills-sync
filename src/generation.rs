use crate::{config::Loaded, lock::LockFile, patch, source, sync, util};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::{fs, io::ErrorKind, path::Path};

pub fn run(config: &Loaded, id: &str, file: &Path, edited: &Path, output: &Path) -> Result<()> {
    let guard = match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(config.prefix.join("sync.lock"))
    {
        Ok(guard) => {
            guard
                .try_lock_exclusive()
                .context("another sync is already running for this prefix")?;
            Some(guard)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if config.prefix.join("transaction.json").exists() {
        bail!("interrupted transaction; generation does not recover it; run sync first");
    }
    let cwd = std::env::current_dir()?;
    let output_path = validate_output(config, &cwd, output)?;
    let lock_bytes = fs::read(&config.lock).context("lockfile is missing; run sync first")?;
    let inputs = source::load_patch_inputs(config)?;
    let locked = LockFile::require_current(config, &inputs)?;
    check_inputs(config, &lock_bytes, &inputs)?;
    let baseline = source::locked_baseline(config, id, &locked, &inputs)?;
    let edited_bytes = patch::read_bounded(&util::absolute(&cwd, edited)?)?;
    let document = patch::read_document(&baseline.tree, file)?;
    let mut generated = patch::generate_file(&baseline.tree, file, &edited_bytes)?;
    check_inputs(config, &lock_bytes, &inputs)?;
    if validate_output(config, &cwd, output)? != output_path {
        bail!("output ancestors changed during generation");
    }
    util::create_new(&output_path, &generated.bytes)?;
    println!("Baseline {id}@{}", baseline.locked.revision);
    for input in &inputs[id] {
        println!(
            "Existing patch {} sha256 {}",
            input.info.path.display(),
            input.info.sha256
        );
    }
    println!(
        "Baseline document {} sha256 {}",
        file.display(),
        util::hash(&document)
    );
    generated.report.patch = output.to_path_buf();
    generated.report.print(false);
    println!("Created {} (append to source patches)", output.display());
    drop(guard);
    Ok(())
}
fn check_inputs(config: &Loaded, lock_bytes: &[u8], inputs: &source::PatchInputs) -> Result<()> {
    if fs::read(&config.manifest_path)? != config.manifest_bytes
        || fs::read(&config.lock)? != lock_bytes
    {
        bail!("manifest or lockfile changed during generation; no output written");
    }
    for stack in inputs.values() {
        for input in stack {
            if fs::read(util::absolute(&config.base, &input.info.path)?)? != input.bytes {
                bail!(
                    "patch {} changed during generation; no output written",
                    input.info.path.display()
                );
            }
        }
    }
    Ok(())
}
fn validate_output(config: &Loaded, cwd: &Path, output: &Path) -> Result<std::path::PathBuf> {
    if !output.to_string_lossy().ends_with(".skillpatch.toml") {
        bail!("output must end in .skillpatch.toml");
    }
    let resolved = util::resolved_path(cwd, output)?;
    if resolved == util::resolved_path(cwd, &config.manifest_path)?
        || resolved == util::resolved_path(cwd, &config.lock)?
    {
        bail!("unsafe output: collides with manifest or lockfile");
    }
    let roots = std::iter::once(config.prefix.clone())
        .chain(config.targets.clone())
        .chain(sync::installed_targets(config)?);
    for root in roots {
        if resolved.starts_with(util::resolved_path(cwd, &root)?) {
            bail!("unsafe output: inside cache, prefix or installed target");
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_inputs_are_rechecked_even_when_semantics_are_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = temp.path().join("skills.toml");
        fs::write(
            &manifest,
            "version = 1\nprefix = 'store'\ntargets = ['target']\n[sources]\n",
        )
        .unwrap();
        let config = Loaded::read(&manifest).unwrap();
        fs::write(&config.lock, "captured lock bytes").unwrap();
        let lock = fs::read(&config.lock).unwrap();
        let inputs = source::PatchInputs::new();
        assert!(check_inputs(&config, &lock, &inputs).is_ok());
        let mut changed = config.manifest_bytes.clone();
        changed.extend_from_slice(b"\n# comment\n");
        fs::write(&manifest, changed).unwrap();
        assert!(check_inputs(&config, &lock, &inputs).is_err());
        fs::write(&manifest, &config.manifest_bytes).unwrap();
        fs::write(&config.lock, "different lock bytes").unwrap();
        assert!(check_inputs(&config, &lock, &inputs).is_err());
        fs::write(&config.lock, &lock).unwrap();
        fs::write(config.base.join("edit.skillpatch.toml"), "captured patch").unwrap();
        let mut inputs = source::PatchInputs::new();
        inputs.insert(
            "source".into(),
            vec![source::PatchInput {
                info: source::Patch {
                    path: "edit.skillpatch.toml".into(),
                    sha256: util::hash(b"captured patch"),
                },
                bytes: b"captured patch".to_vec(),
            }],
        );
        assert!(check_inputs(&config, &lock, &inputs).is_ok());
        fs::write(config.base.join("edit.skillpatch.toml"), "changed patch").unwrap();
        assert!(check_inputs(&config, &lock, &inputs).is_err());
    }
}
