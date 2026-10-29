# Design

## Context

See `proposal.md` for motivation and `specs/sync-commands/spec.md` for behavior. `src/main.rs` already routes sync and update through `sync::run`. That function holds a prefix lock, resolves and validates sources, previews changes, then commits links, lockfile, and state using a recovery journal. Its rollback handler covers only that installation transaction.

`Manifest` in `src/config.rs` denies unknown fields. `lock::fingerprint` hashes its serialized representation with patch hashes. Adding a serialized default field without omitting empty values would invalidate existing locks. The README supports Linux and macOS, not Windows, and currently says skill scripts are not executed. This remains true for discovered source content; explicit manifest commands introduce a separate trust boundary.

`tests/cli.rs` exercises the real binary with temporary Git repositories and explicit prefixes. Its fixture's `extra` argument inserts fields inside a source table, so command tests must place the new array before source tables rather than use that argument unchanged.

## Goals / Non-Goals

Goals:
- Add one post-commit execution point shared by sync and update.
- Preserve existing lock fingerprints and transaction recovery for configurations without commands.
- Keep subprocess output usable for installers and diagnostics clear about partial success.

Non-goals:
- Command-backed sources, generated-output discovery, uninstall hooks, or command result caching.
- Before/after hook frameworks, per-source commands, shell selection, custom environment tables, or command-specific working directories.
- Sandboxing, timeouts, automatic retry, rollback of subprocess effects, or Windows support.
- Relaxing existing target requirements. Command-only configurations still supply targets and an empty `[sources]` table.

## Decisions

### One optional ordered array

Add `commands: Vec<String>` with a serde default and `skip_serializing_if = "Vec::is_empty"`. Validate blank and NUL-containing strings when loading the manifest, reporting the entry index. Preserve accepted strings exactly rather than trimming them for execution or hashing.

Example, placed before any TOML table:

```toml
version = 1
targets = ["~/.pi/agent/skills"]
commands = ["rex skill setup --agent pi --force"]

[sources]
```

This uses the existing version-1 format. A list of strings matches the requested shell commands without introducing named hook objects. Empty-value omission preserves legacy serialized hashes; nonempty arrays naturally participate in the existing fingerprint, including order. No lockfile schema change is needed.

### Execute after the journal is removed

Add a small command-running helper in `src/sync.rs`. Call it after the transaction result has passed the rollback handler, before reporting overall success. Do not put command execution inside the transactional closure. Retain the existing prefix lock through command execution to prevent another sync of the same prefix from interleaving.

The dry-run branch prints the ordered commands as planned actions before returning, without calling the runner. It retains existing recovery and cache behavior. The common execution path covers no-change runs, empty sources, locked/offline syncs, and filtered updates without separate dispatch changes.

Executing before commit was rejected because installers should see the newly installed skills and arbitrary external effects cannot be rolled back. Releasing the prefix lock first was rejected because it would let concurrent invocations interleave command runs. Recursive commands targeting the same prefix will fail its lock check rather than deadlock.

### Use a fixed POSIX shell

Use `std::process::Command` with `/bin/sh`, `-c`, and the original command string, setting `current_dir` to `Loaded.base`. Inherit environment and all standard streams; wait for each child before starting the next. Print an identifying message to stderr before spawning each command.

This supports quoting, redirects, pipelines, and environment expansion on supported platforms. Direct executable argument arrays would not meet shell-string semantics. `$SHELL` and interactive shells would introduce user-specific syntax and startup configuration. Each command gets a separate shell; directory or environment changes do not carry forward. No implicit `set -e` is added; the shell's final status determines command success.

### Treat command failure as post-install failure

Return an error on spawn/wait errors, nonzero exit, or signal termination, with the entry index, command, available status, and an explicit statement that the managed installation remains committed. Do not execute later commands or re-enter installation recovery. A subsequent invocation starts the full command list again, with no completion ledger.

Commands are global to the manifest. Update source filters only affect source revision selection. Offline continues to mean no Git fetching by skills-sync, not a network sandbox. Locked mode checks the manifest before running commands but does not pin the external executable or its outputs.

### Leave external outputs unmanaged

Do not add subprocess outputs to `State`, `LockFile.sources`, or `list`. Removing a command only removes future execution. Document that commands must not overwrite managed links, state, or cached snapshots; existing foreign-link and integrity protections remain in force if they do. An external installer can coexist in target directories by using names not managed by skills-sync.

## Risks / Trade-offs

- Arbitrary code execution from a manifest: document that users must trust the manifest and commands run with their permissions. Never discover executable hooks from downloaded skills.
- Partial success: diagnostics distinguish committed skills from failed commands; retry reruns every command, so recommend repeat-safe installers such as the supplied `--force` example.
- Commands can hang or modify managed files: expose their streams, retain normal process behavior, and document the ownership boundary. No isolation or execution timeout is promised.
- Commands may access the network during offline sync: document the flag's Git-only scope explicitly.
- Fingerprint compatibility can regress silently: test against the pre-change serialization shape, not only locks generated by the updated binary.

## Migration Plan

No automatic migration is required. Existing manifests omit the field and keep their locks valid. Users add `commands` and run ordinary sync to refresh the manifest fingerprint before using locked sync. To disable commands, remove the array and sync again. Older binaries reject the new field; remove it before downgrading. Disabling commands or downgrading does not remove external files already created.
