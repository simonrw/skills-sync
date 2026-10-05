# Skill patch generation

Design proposal with the first delivery slice implemented. Later-operation semantics below remain proposals.

## Implementation status

Delivered: strict version-1 `replace_text` and `insert_section`, ordered mixed Git and Markdown stacks, raw patch hashes, operation counts, dry-run document diffs, and `patch generate SOURCE FILE EDITED --output PATH`. Generation reconstructs and compares the complete locked effective source offline, serializes and parses the candidate again, and requires exact edited bytes before exclusive output creation. It does not install, fetch, recover transactions, or modify the manifest, lockfile, or installation state. Snapshot cache population is allowed.

The engine uses transient pulldown-cmark offset events, a private heading outline, and normalized literal source maps rather than an AST or persistent document graph. Structure and protected-byte comparisons guard edits. Generation discovers local changed prose windows with minimal unique context and canonical added sections. All semantics stay behind the file-level patch engine.

Conservative limits: 1 MiB Markdown documents and skill-patch files, 256 operations, 16 MiB matching and structural work, and 32 contextual expansion attempts. Diff discovery uses deterministic Unicode-scalar LCS after trimming equal ends, with a checked 1,048,576-cell allocation cap. Intersecting word-expanded windows coalesce. Independent edits remain separate operations, with fresh replay and parsing between them. Oversized or ambiguous edits fail with Git-patch advice.

Matches cannot cross formatting, escaping, entities, hard breaks, or prose-block boundaries. YAML frontmatter, code, HTML, heading text, and link destinations are protected. A paragraph or list item with inline HTML is protected wholesale, including its ordinary surrounding text. Generation may refuse ambiguous repeated prose, removed or added blocks, moved or renamed existing sections, whitespace-only edits, and noncanonical insertion separators. Application still follows scoped prose after section movement and rewrapping. Insertion uses deterministic LF or CRLF rendering, refuses mixed endings, and checks deeper body headings and retained structure. Generation never widens a count or invents a policy to account for duplicate text.

Legacy lock compatibility uses the existing fingerprint of the parsed manifest and ordered raw patches, not an unavailable historical raw-manifest hash. Captured manifest, patch, and lock bytes are rechecked during generation. Resolved output ancestors exclude manifest and lock aliases, the prefix cache, and current and prior installed targets. These checks are ordinary local-work safeguards, not protection from hostile filesystem races.

Deferred: `remove_sentence`, `replace_sentence`, all regex and matcher policies, sentence segmentation, and refreshing existing patches. These operation names are rejected clearly. They are never partially interpreted. Their examples and verification goals below describe future work, not current capabilities.

## Problem

Users edit a copy of an upstream skill and want to keep those edits across upstream updates. Standard Git patches can fail when neighboring lines change or prose is rewrapped. More permissive matching can silently change the wrong instruction.

The design must distinguish matching the same edit despite formatting changes from applying a policy to new upstream content. An edited document provides evidence for the first, but cannot establish the second.

## Original behavior

`src/main.rs` dispatches sync and update commands through `sync::run`. `src/config.rs` stores an ordered list of patch paths per source.

Originally, `source::patches` in `src/source.rs` read patch bytes and hashed them. `source::prepare` resolved a Git revision or copied a local source, created a temporary tree, and applied each patch with `git apply --check` followed by `git apply`. It validated the resulting tree, discovered skills, and stored an immutable snapshot. The delivered slice captures stacks through `source::load_patch_inputs` and dispatches both formats through `patch::apply_file` within that same snapshot pipeline.

`sync::run` prepares every source and checks the installation plan before changing links or the lockfile. `ordered_patches_are_hashed_and_failure_is_safe` in `tests/cli.rs` covers ordered application, patch hashing, and unchanged installed content and lockfile after patch failure.

New patch behavior belongs inside snapshot preparation, not installation reconciliation.

## Proposed usage

```sh
# Copy an installed skill to an ordinary file before editing it.
# Never edit through the installed symlink.
cp ~/.agents/skills/pdf/SKILL.md ./edited-SKILL.md

# Compare against a named source file at its locked revision.
skills-sync patch generate documents skills/pdf/SKILL.md ./edited-SKILL.md \
  --output patches/pdf.skillpatch.toml

# Add the generated path to the source's patches list, then preview.
skills-sync sync --dry-run
skills-sync sync
```

The baseline is the locked source with its existing patches applied. The new patch is intended to be appended to that stack. Generation prints the baseline revision and existing patch hashes. It rejects stale configuration and a local source whose content differs from the locked revision, rather than comparing against a moving baseline.

Generation does not update upstream revisions, installed links, the manifest, or the lockfile. It refuses to overwrite an existing output file. CLI input and output paths resolve from the working directory; file paths inside patches resolve from the source root. Manifest patch paths retain their current manifest-relative behavior.

Refreshing an existing patch is a separate future workflow. Appending another patch must not quietly replace or duplicate an earlier one.

## Two designs considered

### Saved base and edited documents with three-way merge

Store the original document and the edited document, then merge their changes into later upstream versions. This has a small interface and captures arbitrary changes without inventing a language. It hides merge mechanics well, but cannot express persistent rules such as removing new sentences mentioning a keyword. Conflict behavior also remains largely line-oriented.

### Scoped Markdown operations

Store edits targeting source-relative files, heading paths, prose, and sections. The engine hides parsing, source-offset matching, cardinality checks, and byte-preserving edits. Callers see only the edit and its scope. This expresses both conservative generated edits and explicit recurring policies, at the cost of owning a small patch language.

Choose scoped operations. Retain Git diffs for scripts, assets, arbitrary formatting edits, and cases that the Markdown operations cannot represent safely. Do not add a second fuzzy merge engine underneath failed operations.

## Format

Use TOML because the project already depends on it. Files ending in `.skillpatch.toml` use the new versioned format. Existing Git patch files keep their existing behavior. Unknown versions, operation names, and fields fail validation.

A hand-authored patch might look like this:

```toml
version = 1
file = "skills/example/SKILL.md"

[[operations]]
op = "replace_text"
section = ["Tooling"]
find = "Claude Code"
replace = "pi"
whole_words = true
expect = { min = 2, max = 2 }

[[operations]]
op = "remove_sentence"
section = ["Privacy"]
match = { word = "telemetry" }
expect = { min = 1, max = 3 }

[[operations]]
op = "insert_section"
after = ["Verification"]
heading = "Local preferences"
content = "Use mise to run project tools.\n"
```

These are explicitly authored policies, not examples of what the generator infers automatically.

An empty heading path selects document prose. A nonempty path identifies headings by their ancestor titles, ignoring a single document-title H1. A section includes its descendants. Duplicate matching paths are ambiguous and fail; selecting the first match is forbidden.

The file field is a validated source-relative path to one regular Markdown file. Reject absolute paths, parent traversal, and symlink targets. Version one operates on skill documents, not arbitrary repository files.

## Operations

- `replace_text`: replace a literal phrase within prose, optionally requiring Unicode word boundaries around the phrase. Matching may normalize whitespace in prose. It preserves bytes outside the actual replacement spans.
- `remove_sentence`: remove sentences matching exact text, a whole word, or an explicitly authored regex. Exactly one matcher is allowed. Word and text matching are case-sensitive by default.
- `replace_sentence`: apply an explicitly authored regex substitution within individual prose sentences. Matches cannot cross sentence or prose-block boundaries. Use Rust regex syntax and replacement conventions, not shell expressions or executable code.
- `insert_section`: insert a complete section after a uniquely identified section subtree, at the same heading level. Derive the heading marker from that level. An existing sibling with the same heading is a conflict, not permission to overwrite it.

Every matching operation declares a minimum and maximum match count. For removal, the count means sentences removed. For replacement, it means replacement spans. Generated operations use exact counts. Wider ranges require an explicit user edit. An insertion requires exactly one anchor and no conflicting sibling heading.

Do not match YAML frontmatter, fenced or indented code, inline code, link destinations, or raw HTML as prose. Parse Markdown into source ranges and edit those ranges; do not serialize a new document from an AST. Sentence segmentation happens within eligible prose blocks and must account for abbreviations, URLs, list structure, and punctuation. Uncertain boundaries use a stricter literal edit or make generation fail.

Sentence matching across formatting nodes is allowed only when the engine can map the edit back to a contiguous source range without corrupting Markdown. Otherwise report an unsupported edit. Regex matching is bounded by file and pattern size limits.

## Generation

1. Obtain and identify the locked effective baseline without installing anything.
2. Parse baseline and edited documents with source offsets.
3. Align uniquely identifiable headings and prose blocks.
4. Emit scoped literal replacements, exact sentence removals, and complete section insertions when those operations account for the change.
5. Apply the candidate patch to the baseline using the same engine that sync uses.
6. Require byte-for-byte equality with the edited document before writing the output.

Generation never generalizes a deletion into a keyword rule. Replacing several occurrences does not prove that every future occurrence should change. Do not infer regexes or document-wide substitutions either.

When the engine cannot represent a change, generation fails with the affected location and explains that a Git patch is appropriate. Do not silently omit formatting changes, frontmatter edits, moves, or code changes to obtain a successful result.

Baseline revision and content hash are provenance, not hard preconditions during future updates. Requiring the original content hash during application would defeat the feature.

## Application and reporting

Operations apply in declaration order. Resolve each operation against the document produced by its predecessors. Match all spans for an operation before editing, reject overlapping spans, and apply replacements from the end of the document backward.

Failure discards the temporary source snapshot. Installation links and the lockfile remain unchanged, as they do today. Both formats can appear in one ordered patch stack and contribute their raw-byte hashes to the lockfile.

Failures identify the patch, operation index, file, heading scope, expected count, actual count, and candidate locations. A missing heading or zero matches is an error by default. An explicitly authored minimum of zero permits absence but must be visible in the preview report.

Dry-run output includes the document diff and operation counts, not only symlink changes. Normal updates also report applied rule counts, so a successful broad deletion is not invisible.

Line movement, blank-line changes, and prose rewrapping should survive. Heading renames, duplicated targets, and substantive rewording should produce conflicts. The engine does not equate similar wording with identical intent.

Fresh snapshots make repeated sync runs deterministic. The operations need not be idempotent on arbitrary already-patched documents. In particular, absence alone is not evidence that a removal already happened.

## Internal sketch

Keep patch parsing, generation, application, and matching semantics together in a new `src/patch.rs`. Keep Git source resolution in `src/source.rs` and installation transactions in `src/sync.rs`.

Conceptual signatures, not code to compile yet:

```rust
struct SkillPatch {
    file: SourceRelativeMarkdownPath,
    operations: Vec<Operation>,
}

enum Operation {
    ReplaceText(TextReplacement),
    RemoveSentence(SentenceRemoval),
    ReplaceSentence(SentenceReplacement),
    InsertSection(SectionInsertion),
}

enum SentenceMatcher {
    Text(String),
    Word(String),
    Regex(CompiledPattern),
}

struct MatchCount {
    min: usize,
    max: usize,
}

struct AppliedDocument {
    bytes: Vec<u8>,
    report: PatchReport,
}

fn generate(base: &str, edited: &str) -> Result<SkillPatch>;
fn apply(document: &str, patch: &SkillPatch) -> Result<AppliedDocument>;
fn apply_file(tree: &Path, path: &Path, bytes: &[u8]) -> Result<PatchReport>;
```

The CLI supplies file identity and baseline provenance around pure document generation. Parse file paths, patterns, matcher variants, and count ranges into validated types at the patch boundary. The application engine neither fetches Git data nor writes installed skills.

Move the existing Git application loop behind the same file-level dispatcher, rather than giving source preparation two competing patch pipelines. Reuse snapshot construction for generation without invoking `sync::run` or copying its transaction behavior.

## Verification contract

The implementation must prove these behaviors with temporary local repositories and real CLI invocations:

- Generated patches reproduce each edited input exactly.
- Rewrapping prose, moving sections, and adding unrelated content retain the intended changes.
- Duplicate headings, duplicated match text, missing anchors, and changed match counts fail before installation changes.
- Code blocks, inline code, frontmatter, and link destinations remain untouched by prose rules.
- Keyword and regex policies operate only within their declared scope and report their effects.
- A keyword appearing in a new sentence exceeds a generated exact count unless a user explicitly widens the policy.
- Unsupported edits fail generation without producing a misleading partial patch.
- Mixed Git and Markdown patches honor order and participate in locked offline replay.
- A failed update leaves installed content and lockfile bytes unchanged.
- New patch versions or changed application semantics cannot silently replay under an old lockfile; an output mismatch must fail locked sync.

## Delivery and unresolved choices

Start with scoped literal replacements and section insertion, round-trip generation, and snapshot integration. Add sentence removal after sentence-boundary fixtures establish its contract. Add manual regex operations last. Do not use an LLM at sync time.

Generation is deterministic and noninteractive. The user chose conservative patches they edit afterward. The generator emits scoped edits with exact match counts and verifies that the patch reproduces the edited document exactly. Users can then widen match counts, introduce regexes, or add keyword rules manually and preview the result with `sync --dry-run`. Do not infer recurring policies or prompt users to choose them during generation.

The remaining question is how much punctuation and heading variation to tolerate. Version one should normalize prose whitespace and heading whitespace only, then refuse ambiguity. Broader tolerance needs specific fixtures proving that it does not redirect edits.
