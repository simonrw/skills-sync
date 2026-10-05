# skills-sync

A declarative Rust CLI for managing a chosen set of agent skills. Declare sources
and paths once; sync adds, updates, and removes managed symlinks to match that
declaration. Git sources stay at their locked commits until you explicitly update
them. Local patches and local directories are hashed too.

## Install

Requires Rust 1.88+ and Git on PATH. Linux and macOS are supported; Windows is
currently untested and not supported.

```sh
cargo install --path . --locked
skills-sync init
# Edit skills.toml, then:
skills-sync sync
```

Commit `skills.toml`, `skills.lock`, and your patches. Ignore the prefix and agent
skill directories; they can be reconstructed from the manifest and lockfile.

## Declare your skills

```toml
version = 1
prefix = "~/.local/share/skills-sync"
targets = ["~/.agents/skills", "~/.claude/skills"]

[sources.documents]
repo = "https://github.com/anthropics/skills.git"
ref = "main"
skills = ["skills/pdf", "skills/docx"]
patches = ["patches/pdf.patch"]

[sources.team]
repo = "git@github.com:example/monorepo.git"
ref = "v2.0.0"
skills = ["plugins/*/skills/**", "docs/skills/review"]
rename = { "docs/skills/review" = "team-review" }

[sources.personal]
path = "./my-skills"
skills = ["*"]
```

Each source must have exactly one of `repo` or `path`. Source IDs are your own
stable names. Repositories may use HTTPS, SSH, `file://`, or a local filesystem
path. Git authentication uses your normal Git/SSH configuration; no credentials
are written to the manifest or lockfile unless you put them into the URL yourself.
Interactive Git credential prompts are disabled.

Paths in the manifest, including patches, resolve relative to the manifest's
directory, regardless of your working directory. Absolute paths and `~/` are
accepted. Git refs may be branches, tags, or full commit hashes. If `ref` is
omitted, updates resolve the upstream default branch.

### Skill selection

A skill is a directory containing a regular `SKILL.md` file. Discovery searches
the whole source recursively, including hidden directories, excluding `.git`.
It continues into nested directories even when their parent is itself a skill.

| Selector | Meaning |
| --- | --- |
| `"*"` | Every skill at every depth; also the default when `skills` is omitted |
| `"skills/pdf"` | One exact source-relative skill directory |
| `"plugins/*/skills/**"` | Glob against source-relative skill directory paths |
| `"."` | A source with `SKILL.md` directly at its root |

Patterns use `/` separators. Inside a pattern, `*` does not cross `/`; `**` does.
The standalone `"*"` is special and means all skills. Overlapping selectors are
deduplicated. Each selector must match at least one skill; misspellings fail before
installation changes. For a root skill, provide `rename = { "." = "my-skill" }`.

Skills install under their directory basename, rather than their frontmatter name.
Use `rename` with exact source-relative paths to override names. Name collisions
between any selected skills fail until you resolve them with a rename. IDs and
install names accept ASCII letters, digits, `.`, `_`, and `-` (except `.` and `..`).
To uninstall an entire source, remove its table. To uninstall everything, retain
your settings and an empty `[sources]` table.

## Commands

```sh
skills-sync sync                  # Reconcile, reusing locked Git commits
skills-sync sync --dry-run        # Resolve/validate and print proposed link changes
skills-sync sync --locked         # Fail if manifest, patches, local trees or selections changed
skills-sync sync --locked --offline
skills-sync update               # Advance every Git source and sync
skills-sync update team           # Advance only team; keep other locked Git revisions
skills-sync update team --dry-run
skills-sync list                  # Last installed names, source paths, upstreams, revisions
skills-sync list --json
skills-sync --manifest config/my-skills.toml sync
```

The lockfile sits beside the manifest with its extension changed to `.lock`.
For each source it records the upstream, requested ref, exact commit or local
content hash, ordered patch hashes, selected paths/install names, and patched tree
hash. Its ordering is stable and it has no timestamps. `--locked` also checks a
hash of the parsed manifest and patch contents, and leaves lockfile bytes intact.

Changing only selectors, renames, or patches reuses the existing Git commit.
Changing a repository or its requested ref resolves a new commit during normal
sync. Newly declared Git sources are resolved and locked during normal sync.
Local sources always snapshot their current content; `--locked` rejects changes.

`--offline` requires the needed Git repository and commit to exist in the prefix's
cache. It never fetches. For a fresh machine, use `sync --locked` to fetch the
locked commits first. Local sources work offline without a previous lockfile.
`--dry-run` may fetch repositories and populate snapshots, but leaves installed
links, state, and lockfile unchanged. Any pending interrupted transaction is
recovered first, including during a dry run.

## Prefix layout and ownership

```text
<prefix>/
  repos/<sha256-of-repo-url>.git/       # Bare mirrors with an origin remote
  sources/<source-id>/<hash>/
    source.json                       # Upstream, revision, patch and tree hashes
    tree/                             # Complete patched source snapshot
  state.json                          # Owned target links and installed origins
  sync.lock                           # OS file lock for concurrent sync protection
  transaction.json                    # Present only during a transaction

<target>/pdf -> <prefix>/sources/documents/<hash>/tree/skills/pdf
```

Snapshots include the full repository tree, keeping sibling references and helper
scripts available. They are treated as immutable; a modified cached snapshot is
reported instead of silently reused. Edit a local source or a patch rather than
files reached through installed symlinks. Old mirrors and snapshots remain cached
so you can inspect origins and reproduce earlier installations; automatic garbage
collection is not implemented.

A prefix belongs to one manifest. Use distinct prefixes for distinct independent
manifests. Target directories may contain unrelated skills: sync removes only
the exact symlinks it previously recorded. It refuses to overwrite regular files,
directories, foreign symlinks, or links you changed yourself. Missing managed
links are recreated. Changing the targets in the same manifest removes owned links
from the old targets and installs them in the new targets.

Plans are fully resolved and checked before links change. Link replacements and
individual state/lockfile writes are atomic. A transaction journal allows the
next sync to restore the previous installation after an interrupted transaction.
The whole set of target directories is not switched atomically; other processes
can observe changes as individual links are replaced. If a link is changed
externally during recovery, recovery stops and retains the journal for inspection.

## Maintain patches

Git patches are standard `git diff` files relative to the source root, applied using `git apply --check` followed by `git apply`. Files ending in `.skillpatch.toml` instead use version-1 scoped Markdown operations. Both formats apply in the listed order to a fresh snapshot and contribute raw-byte hashes to the lockfile. They never modify the upstream mirror or replay on an already patched tree.

For example, in a separate clone checked out at the locked commit:

```sh
# Edit skills/pdf/SKILL.md, then save the diff into your config project:
git diff --binary -- skills/pdf/SKILL.md > /path/to/config/patches/pdf.patch
```

Declare the patch in `patches`, then run `sync`. Review and commit the resulting
lockfile. If an update no longer accepts a patch, update fails before installed
links or the lockfile change. Refresh the patch against the new upstream revision
and retry `update`. Include newly added files in the diff using `git add -N` first.

### Scoped Markdown patches

```sh
cp ~/.agents/skills/pdf/SKILL.md ./edited-SKILL.md
# Edit the ordinary copy, not the installed symlink.
skills-sync patch generate documents skills/pdf/SKILL.md ./edited-SKILL.md \
  --output patches/pdf.skillpatch.toml
# Append this path to documents.patches, then:
skills-sync sync --dry-run
skills-sync sync
```

Generation requires a current lock and reconstructs its already-patched baseline offline. It prints the revision, existing patch hashes, document hash, and operation scopes. Edited and output paths resolve from your working directory. Document paths are source-relative. Generation may populate snapshots but never installs, updates the lock or manifest, fetches, or recovers an interrupted transaction. Output must be new and outside the prefix and installation targets. Create its parent directory first. Manifest checks use the same parsed-manifest fingerprint as locked sync, with raw input stability checked during generation.

Only `replace_text` and `insert_section` are supported. Sentence operations, regex operations, and unknown fields fail. Generation emits exact-count literal edits and verifies serialized replay byte-for-byte. Ambiguous edits, formatting changes, and changes to protected content fail with Git-patch guidance. Prose whitespace can normalize during application. Headings and protected syntax never become replacement targets. Phrases cannot cross formatting delimiters. Paragraphs and list items containing inline HTML are entirely protected. Markdown documents and skill-patch files are limited to 1 MiB, with at most 256 operations. Diff discovery is bounded to 1,048,576 scalar-LCS cells after trimming unchanged ends. Larger edits may require a Git patch.

Section insertion follows an anchor's complete subtree at the same heading level. Its canonical ATX heading has a blank line before its raw body. Separators use LF or CRLF deterministically. Mixed endings fail. Noncanonical section additions can require a Git patch. Normal sync prints rule counts, including explicitly allowed zero matches. Dry run also prints document diffs.

## Boundaries

Internal relative symlinks are preserved. Absolute, escaping, dangling, and cyclic
source symlinks are rejected. Discovery does not follow symlink directories.
Local sources copy ordinary files and symlinks, preserve file permissions, ignore
`.git`, and hash file contents, symlink targets, and executable bits. Local sources
cannot overlap the prefix or target directories.

Git snapshots come from `git archive` and honor committed `export-ignore` and
`export-subst` attributes. Submodules are rejected: declare each submodule as its
own source. Git LFS objects are not downloaded; archives contain their committed
pointer files. Skills' frontmatter and scripts are not executed or validated by
this installer. Paths must be valid UTF-8.

## Development

```sh
cargo nextest run --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
```

The integration tests create real temporary Git repositories and invoke the CLI.
They require no network and cover locked replay, updates, nested discovery,
patches, pruning, conflicting files, local snapshots, and crash recovery.
