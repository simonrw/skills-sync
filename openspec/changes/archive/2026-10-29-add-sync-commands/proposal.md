# Proposal

## Why

Some tools install their own agent skills through commands such as `rex skill setup --agent pi --force`. Declaring these commands alongside skill sources lets one sync or update refresh both kinds of installation.

## What Changes

- Add an optional ordered top-level `commands` array of shell strings to `skills.toml`.
- Run each command from the manifest directory after successful reconciliation on every sync and update, including runs with no link changes and source-filtered updates.
- Print commands without executing them during dry runs.
- Stop on the first command failure and return an error without rolling back the completed skill installation.
- Keep command-created files outside ownership, listing, and uninstall tracking.
- Include nonempty command declarations in the manifest fingerprint while preserving existing locks for manifests without commands.

## Capabilities

### New Capabilities

- `sync-commands`: Declaration, execution, preview, and failure behavior of user-configured post-sync shell commands.

### Modified Capabilities

None. The project currently has no main specs.

## Impact

- `src/config.rs`: Manifest parsing and validation.
- `src/sync.rs`: Shared sync/update execution path, dry-run output, and post-transaction execution.
- `src/lock.rs`: Existing parsed-manifest fingerprint behavior and compatibility tests.
- `tests/cli.rs`: CLI integration coverage using local shell commands, not Rex.
- `README.md` and `examples/skills.toml`: Configuration, trust boundary, and command lifecycle documentation.
- No new dependencies or manifest/lockfile version changes are expected. Commands execute trusted user configuration with the user's permissions; their effects are neither reproducible snapshots nor transactional.
