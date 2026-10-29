# sync-commands Specification

## Purpose

Allow users to declare shell commands that refresh externally managed skill installations whenever skills-sync successfully syncs or updates its managed skills.

## Requirements

### Requirement: Ordered command declaration
The manifest SHALL accept an optional top-level `commands` array of strings. An omitted or empty array SHALL mean no commands. Empty, whitespace-only, or NUL-containing commands and non-string entries MUST be rejected before reconciliation begins.

#### Scenario: Declare an external installer
- **WHEN** a manifest declares `commands = ["rex skill setup --agent pi --force"]`
- **THEN** the declaration is accepted without requiring a corresponding source entry

#### Scenario: No commands configured
- **WHEN** `commands` is absent or empty
- **THEN** sync and update retain their existing behavior without starting shell commands

#### Scenario: Invalid declaration
- **WHEN** a command is blank, contains NUL, or is not a string
- **THEN** loading the manifest fails before any installation changes or command execution

### Requirement: Post-sync execution
Every successful non-dry-run sync or update SHALL execute all declared commands sequentially in declaration order, after managed links, state, and lockfile have committed. This SHALL apply to no-change syncs, locked syncs, offline syncs, source-filtered updates, and manifests with empty sources. Other CLI operations MUST NOT execute these commands.

#### Scenario: Commands see the completed installation
- **WHEN** sync installs skills and has two commands
- **THEN** both commands see the committed installation and the second starts only after the first succeeds

#### Scenario: Repeat a no-change sync
- **WHEN** a successful sync is repeated without changing its inputs
- **THEN** all commands run again despite zero link changes

#### Scenario: Filtered update
- **WHEN** update selects one valid source ID
- **THEN** all commands run once after reconciliation, independently of the source filter

#### Scenario: Locked offline sync
- **WHEN** sync succeeds with `--locked --offline`
- **THEN** all commands run and the lockfile remains unchanged
- **AND** offline mode limits skills-sync's Git fetching, not network access by commands

#### Scenario: Command-only configuration
- **WHEN** a valid manifest has targets, an empty sources table, and commands
- **THEN** sync reconciles its managed installation and runs the commands

#### Scenario: Reconciliation fails
- **WHEN** validation, source preparation, recovery, or installation fails
- **THEN** no configured commands run

#### Scenario: Read or generate operations
- **WHEN** the user runs list, init, or patch generation
- **THEN** no configured commands run

### Requirement: Shell execution environment
Each command SHALL run in its own `/bin/sh -c` process from the canonical manifest directory, inheriting the caller's environment and standard input, output, and error. Commands SHALL use POSIX shell syntax rather than the user's interactive shell. The CLI SHALL identify each command before executing it.

#### Scenario: Invocation from another directory
- **WHEN** the user supplies a manifest path while running from another directory
- **THEN** relative paths used by the command resolve from the manifest directory

#### Scenario: Shell expressions and output
- **WHEN** a command uses environment expansion, quoting, or redirection
- **THEN** the shell interprets the expression and command output remains visible through the CLI's streams

#### Scenario: Independent shell state
- **WHEN** one command changes directory or exports a variable
- **THEN** that shell state does not change the starting directory or environment of later commands

### Requirement: Command preview
A successful dry run SHALL print the configured commands in declaration order as planned actions without executing them, for both sync and update. Existing dry-run recovery and cache behavior SHALL remain unchanged.

#### Scenario: Preview without side effects
- **WHEN** sync or update runs with `--dry-run` and a command would create a file
- **THEN** the output identifies the planned command and the file is not created

### Requirement: Failure after installation
A command launch failure or unsuccessful termination SHALL stop subsequent commands and cause a nonzero CLI exit. The diagnostic SHALL identify the command and available failure status, and state that managed installation changes remain committed. Completed command effects MUST NOT be rolled back or recorded as resumable work.

#### Scenario: Stop at the first failing command
- **WHEN** the second of three commands exits with status 7
- **THEN** the third is not started, the CLI fails, and the diagnostic identifies the second command and status 7
- **AND** the new managed links, state, and lockfile remain committed without a pending installation transaction

#### Scenario: Retry after a command failure
- **WHEN** a subsequent sync succeeds at reconciliation after an earlier command failure
- **THEN** command execution starts again with the first declared command

### Requirement: Lock compatibility
Nonempty command declarations, including their order and exact strings, SHALL contribute to the manifest fingerprint. Omitted and empty command arrays SHALL preserve the fingerprint of an otherwise identical legacy manifest. Locked sync MUST reject command changes before executing commands.

#### Scenario: Existing lock without commands
- **WHEN** an otherwise unchanged legacy manifest is used with its existing lockfile, with commands omitted or empty
- **THEN** locked sync accepts that lockfile

#### Scenario: Commands changed after locking
- **WHEN** a command is added, removed, reordered, or edited after the manifest was locked
- **THEN** locked sync fails its manifest check and executes no commands

### Requirement: External ownership and trust
Command-created files SHALL remain outside skills-sync's ownership, installed-skill listing, and automatic removal. Documentation SHALL explain that commands are trusted arbitrary code with the user's permissions, their effects are not transactional or pinned by the lockfile, and repeated execution requires repeat-safe commands.

#### Scenario: Remove a command declaration
- **WHEN** a command previously created an unmanaged skill and is later removed from the manifest
- **THEN** subsequent sync does not delete that skill merely because the command was removed
- **AND** the skill does not appear in skills-sync's installed-skill listing
