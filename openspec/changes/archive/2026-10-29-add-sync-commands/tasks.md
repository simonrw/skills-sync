# Tasks

## 1. Manifest configuration and compatibility

- [x] 1.1 Add the optional ordered `commands` field in `src/config.rs`, omit empty arrays from serialization, and reject blank or NUL-containing entries. Verify CLI integration cases accept omitted, empty, and nonempty arrays and reject invalid entries before installation changes.
- [x] 1.2 Add lock compatibility coverage through the CLI using a fingerprint derived from the pre-change manifest serialization shape. Verify omitted and empty commands accept a legacy lock, and adding, editing, reordering, or removing commands causes locked sync to fail before commands execute.
- [x] 1.3 Add a commented top-level Rex command example in `examples/skills.toml` and document declaration syntax in `README.md`. Verify the example is outside source tables, the generated default manifest does not execute Rex, and the documented command-only form retains targets and `[sources]`.

## 2. Post-commit execution and preview

- [x] 2.1 Add sequential `/bin/sh -c` execution in `src/sync.rs` after the transaction rollback boundary, retaining the prefix guard and using the manifest directory with inherited environment and streams. Verify real CLI tests exercise ordered commands that observe installed skills, shell expansion, output, and a manifest supplied from another working directory.
- [x] 2.2 Add dry-run command previews without execution. Verify sync and update dry runs display commands in order while leaving command-created sentinel files absent.
- [x] 2.3 Verify repeat execution on no-change sync, locked offline sync, full and source-filtered updates, and empty-source manifests using local shell commands and temporary files. Verify list and patch generation do not execute configured commands; keep tests independent of Rex and network access.
- [x] 2.4 Document execution timing, independent POSIX shells, manifest-relative working directory, inherited environment and streams, repeated execution, dry runs, and offline's Git-only scope in `README.md`. Verify each documented behavior matches the spec and the CLI cases above.

## 3. Failures and external ownership

- [x] 3.1 Return contextual errors for launch/wait failure and unsuccessful exit, stop later commands, and state that managed installation changes remain committed. Verify a three-command CLI case with a middle `exit 7` leaves the new links, state, and lockfile intact, no pending journal, and no third-command sentinel; verify retry starts from the first command.
- [x] 3.2 Verify reconciliation errors prevent execution using a target conflict and a locked-input mismatch. Verify a command attempting a nested sync against the same prefix encounters the existing lock check rather than interleaving installations.
- [x] 3.3 Add a CLI ownership case where a command creates an unmanaged skill, then its declaration is removed. Verify subsequent sync preserves the external skill and `list --json` excludes it.
- [x] 3.4 Document trusted-code execution, repeat-safe commands, failure/retry semantics, no automatic cleanup, and the prohibition on overwriting managed files. Verify the README distinguishes explicit manifest commands from scripts discovered in source skills and explains that external executables and outputs are not pinned by the lockfile.

## 4. Integration checks

- [x] 4.1 Run `cargo nextest run --locked` and verify the full suite passes, including existing transaction recovery, source updates, and lock compatibility coverage.
- [x] 4.2 Run `cargo fmt --check` and `cargo clippy --locked --all-targets -- -D warnings`; verify both complete successfully.
