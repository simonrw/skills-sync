use crate::util;
use anyhow::{bail, Context, Result};
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    ops::Range,
    path::{Component, Path, PathBuf},
};
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_BYTES: usize = 1024 * 1024;
const MAX_OPERATIONS: usize = 256;
const MAX_MATCH_WORK: usize = 16 * MAX_BYTES;
const MAX_CONTEXT_ATTEMPTS: usize = 32;
const MAX_DIFF_CELLS: usize = 1_048_576;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SkillPatch {
    version: u32,
    file: PathBuf,
    operations: Vec<Operation>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    ReplaceText {
        section: Vec<String>,
        find: String,
        replace: String,
        #[serde(default)]
        whole_words: bool,
        expect: Count,
    },
    InsertSection {
        after: Vec<String>,
        heading: String,
        content: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Count {
    min: usize,
    max: usize,
}

#[derive(Default)]
pub struct PatchReport {
    pub patch: PathBuf,
    pub file: PathBuf,
    rules: Vec<String>,
    diff: String,
}
impl PatchReport {
    pub fn print(&self, dry_run: bool) {
        for rule in &self.rules {
            println!(
                "patch {} file {} {rule}",
                self.patch.display(),
                self.file.display()
            );
        }
        if dry_run && !self.diff.is_empty() {
            print!("{}", self.diff);
        }
    }
}
pub struct GeneratedPatch {
    pub bytes: Vec<u8>,
    pub report: PatchReport,
}

fn parse(bytes: &[u8]) -> Result<SkillPatch> {
    if bytes.len() > MAX_BYTES {
        bail!("patch exceeds {MAX_BYTES} byte limit");
    }
    let patch: SkillPatch = toml::from_str(std::str::from_utf8(bytes)?)
        .context("invalid skill patch; only replace_text and insert_section are supported (sentence/regex operations are deferred)")?;
    if patch.version != 1 {
        bail!("unsupported skill patch version {}", patch.version);
    }
    validate_path(&patch.file)?;
    if patch.operations.is_empty() || patch.operations.len() > MAX_OPERATIONS {
        bail!("skill patch requires 1..={MAX_OPERATIONS} operations");
    }
    for op in &patch.operations {
        match op {
            Operation::ReplaceText {
                find,
                expect,
                section,
                ..
            } => {
                if normalize(find).is_empty() {
                    bail!("find must contain non-whitespace prose");
                }
                if expect.min > expect.max {
                    bail!("expected min must be <= max");
                }
                validate_scope(section)?;
            }
            Operation::InsertSection { after, heading, .. } => {
                validate_scope(after)?;
                if after.is_empty() {
                    bail!("insertion requires a nonempty anchor path");
                }
                if normalize(heading).is_empty() || heading.contains(['\r', '\n']) {
                    bail!("insertion heading must be a nonempty single line");
                }
            }
        }
    }
    Ok(patch)
}
fn validate_scope(scope: &[String]) -> Result<()> {
    if scope.iter().any(|s| normalize(s).is_empty()) {
        bail!("empty heading path component");
    }
    Ok(())
}
fn validate_path(path: &Path) -> Result<()> {
    let raw = util::path_str(path)?;
    if raw.is_empty()
        || raw
            .split(['/', '\\'])
            .any(|s| matches!(s, "" | "." | ".." | ".git"))
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || !path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("md") || s.eq_ignore_ascii_case("markdown"))
    {
        bail!("file must be a source-relative Markdown path with normal components, without .git");
    }
    Ok(())
}
pub fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        bail!("{} exceeds {MAX_BYTES} byte limit", path.display());
    }
    Ok(bytes)
}
fn document_path(tree: &Path, file: &Path) -> Result<PathBuf> {
    validate_path(file)?;
    let mut path = tree.to_path_buf();
    let root = fs::symlink_metadata(&path)?;
    if root.file_type().is_symlink() || !root.is_dir() {
        bail!("source root must be a regular directory");
    }
    let parts = file.components().collect::<Vec<_>>();
    for (i, part) in parts.iter().enumerate() {
        path.push(part.as_os_str());
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            bail!("symlink document or ancestor: {}", path.display());
        }
        if if i + 1 == parts.len() {
            !meta.is_file()
        } else {
            !meta.is_dir()
        } {
            bail!(
                "document requires regular file and directory ancestors: {}",
                path.display()
            );
        }
    }
    Ok(path)
}
pub fn read_document(tree: &Path, file: &Path) -> Result<Vec<u8>> {
    read_bounded(&document_path(tree, file)?)
}
fn decode(bytes: &[u8]) -> Result<&str> {
    if bytes.len() > MAX_BYTES {
        bail!("document exceeds {MAX_BYTES} byte limit");
    }
    std::str::from_utf8(bytes).context("Markdown document must be UTF-8")
}

#[derive(Debug)]
struct Heading {
    level: u8,
    title: String,
    path: Vec<String>,
    parent: Option<usize>,
    range: Range<usize>,
    end: usize,
}
struct Scan {
    headings: Vec<Heading>,
    runs: Vec<Range<usize>>,
    structure: Vec<String>,
    structure_ranges: Vec<Range<usize>>,
}
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn frontmatter(source: &str) -> Range<usize> {
    let mut lines = source.split_inclusive('\n');
    if !lines
        .next()
        .is_some_and(|s| s.trim_end().trim_start_matches('\u{feff}') == "---")
    {
        return 0..0;
    }
    let mut end = source.find('\n').map_or(source.len(), |i| i + 1);
    for line in lines {
        end += line.len();
        if matches!(line.trim_end(), "---" | "...") {
            return 0..end;
        }
    }
    0..source.len()
}
fn scan(source: &str) -> Result<Scan> {
    let metadata = frontmatter(source);
    let mut result = Scan {
        headings: Vec::new(),
        runs: Vec::new(),
        structure: Vec::new(),
        structure_ranges: Vec::new(),
    };
    let mut tags = Vec::new();
    let mut tag_ranges = Vec::new();
    let mut html_blocks: Vec<Range<usize>> = Vec::new();
    let mut structure_bytes = 0;
    let mut run: Option<Range<usize>> = None;
    let mut active_heading: Option<(u8, Range<usize>, String)> = None;
    let flush = |run: &mut Option<Range<usize>>, runs: &mut Vec<Range<usize>>| {
        if let Some(r) = run.take() {
            if !normalize(&source[r.clone()]).is_empty() {
                runs.push(r);
            }
        }
    };
    for (event, range) in Parser::new_ext(
        source,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    )
    .into_offset_iter()
    {
        if range.start < metadata.end {
            continue;
        }
        match event {
            Event::Start(tag) => {
                flush(&mut run, &mut result.runs);
                let event = format!("start {tag:?}");
                structure_bytes += event.len();
                if structure_bytes > MAX_MATCH_WORK {
                    bail!("Markdown structure exceeds bounded work limit; use a Git patch");
                }
                result.structure.push(event);
                result.structure_ranges.push(range.clone());
                if let Tag::Heading(level, _, _) = &tag {
                    active_heading = Some((*level as u8, range.clone(), String::new()));
                }
                tags.push(tag);
                tag_ranges.push(range);
            }
            Event::End(tag) => {
                flush(&mut run, &mut result.runs);
                let event = format!("end {tag:?}");
                structure_bytes += event.len();
                if structure_bytes > MAX_MATCH_WORK {
                    bail!("Markdown structure exceeds bounded work limit; use a Git patch");
                }
                result.structure.push(event);
                result.structure_ranges.push(range.clone());
                if matches!(tag, Tag::Heading(..)) {
                    let (level, mut raw, title) =
                        active_heading.take().context("invalid heading event")?;
                    raw.end = range.end;
                    result.headings.push(Heading {
                        level,
                        title: normalize(&title),
                        path: Vec::new(),
                        parent: None,
                        range: raw,
                        end: source.len(),
                    });
                }
                tags.pop();
                tag_ranges.pop();
            }
            Event::Text(text) => {
                if let Some((_, _, title)) = &mut active_heading {
                    title.push_str(&text);
                }
                let eligible = tags.iter().any(|t| matches!(t, Tag::Paragraph | Tag::Item))
                    && !tags.iter().any(|t| {
                        matches!(
                            t,
                            Tag::Heading(..)
                                | Tag::CodeBlock(..)
                                | Tag::Image(..)
                                | Tag::Link(LinkType::Autolink | LinkType::Email, ..)
                        )
                    })
                    && source.get(range.clone()) == Some(text.as_ref());
                if eligible {
                    if let Some(r) = &mut run {
                        if r.end == range.start
                            || source[r.end..range.start].chars().all(char::is_whitespace)
                        {
                            r.end = range.end;
                        } else {
                            flush(&mut run, &mut result.runs);
                            run = Some(range);
                        }
                    } else {
                        run = Some(range);
                    }
                } else {
                    flush(&mut run, &mut result.runs);
                }
            }
            Event::SoftBreak => {
                if let Some(r) = &mut run {
                    if r.end == range.start
                        && source[range.clone()].chars().all(char::is_whitespace)
                    {
                        r.end = range.end;
                    } else {
                        flush(&mut run, &mut result.runs);
                    }
                }
                if let Some((_, _, title)) = &mut active_heading {
                    title.push(' ');
                }
            }
            Event::Html(_) => {
                for (tag, range) in tags.iter().zip(&tag_ranges) {
                    if matches!(tag, Tag::Paragraph | Tag::Item) {
                        html_blocks.push(range.clone());
                    }
                }
                flush(&mut run, &mut result.runs);
            }
            Event::Code(text) => {
                if let Some((_, _, title)) = &mut active_heading {
                    title.push_str(&text);
                }
                flush(&mut run, &mut result.runs);
            }
            _ => {
                flush(&mut run, &mut result.runs);
            }
        }
    }
    flush(&mut run, &mut result.runs);
    html_blocks.sort_by_key(|r| r.start);
    let mut barriers: Vec<Range<usize>> = Vec::new();
    for range in html_blocks {
        if let Some(last) = barriers.last_mut().filter(|r| r.end >= range.start) {
            last.end = last.end.max(range.end);
        } else {
            barriers.push(range);
        }
    }
    result.runs.retain(|run| {
        let i = barriers.partition_point(|r| r.end <= run.start);
        barriers.get(i).is_none_or(|block| block.start >= run.end)
    });
    let title = result.headings.first().is_some_and(|h| h.level == 1)
        && result.headings.iter().filter(|h| h.level == 1).count() == 1;
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..result.headings.len() {
        while stack
            .last()
            .is_some_and(|j| result.headings[*j].level >= result.headings[i].level)
        {
            let j = stack.pop().unwrap();
            result.headings[j].end = result.headings[i].range.start;
        }
        let parent = stack.last().copied();
        let mut path = parent.map_or_else(Vec::new, |p| result.headings[p].path.clone());
        if !(title && i == 0) {
            path.push(result.headings[i].title.clone());
        }
        result.headings[i].path = path;
        result.headings[i].parent = parent;
        stack.push(i);
    }
    Ok(result)
}
fn resolve(scan: &Scan, path: &[String], source: &str) -> Result<Option<usize>> {
    let path = path.iter().map(|s| normalize(s)).collect::<Vec<_>>();
    let matches = scan
        .headings
        .iter()
        .enumerate()
        .filter(|(_, h)| h.path == path)
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        bail!(
            "ambiguous heading path {path:?}: expected 1 heading, actual {}, candidates [{}]",
            matches.len(),
            matches
                .iter()
                .map(|i| location(source, scan.headings[*i].range.start))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(matches.first().copied())
}
fn scope_range(scan: &Scan, path: &[String], source: &str) -> Result<Option<Range<usize>>> {
    if path.is_empty() {
        return Ok(Some(0..source.len()));
    }
    Ok(resolve(scan, path, source)?.map(|i| scan.headings[i].range.end..scan.headings[i].end))
}
struct Window {
    text: String,
    map: Vec<(Range<usize>, Range<usize>)>,
}
fn window(source: &str, range: Range<usize>) -> Window {
    let mut text = String::new();
    let mut map: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    for (i, c) in source[range.clone()].char_indices() {
        let start = range.start + i;
        if c.is_whitespace() {
            if text.ends_with(' ') {
                map.last_mut().unwrap().1.end = start + c.len_utf8();
            } else {
                let n = text.len();
                text.push(' ');
                map.push((n..text.len(), start..start + c.len_utf8()));
            }
        } else {
            let n = text.len();
            text.push(c);
            map.push((n..text.len(), start..start + c.len_utf8()));
        }
    }
    Window { text, map }
}
fn matches(
    source: &str,
    scan: &Scan,
    section: &[String],
    find: &str,
    whole_words: bool,
) -> Result<Vec<Range<usize>>> {
    let Some(scope) = scope_range(scan, section, source)? else {
        return Ok(Vec::new());
    };
    let find = normalize(find);
    let mut found = Vec::new();
    let mut work = 0;
    for run in &scan.runs {
        if run.start < scope.start || run.end > scope.end {
            continue;
        }
        let w = window(source, run.clone());
        let boundaries = if whole_words {
            w.text
                .split_word_bound_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(w.text.len()))
                .collect::<std::collections::BTreeSet<_>>()
        } else {
            Default::default()
        };
        let mut cursor = 0;
        while let Some(relative) = w.text[cursor..].find(&find) {
            let start = cursor + relative;
            let end = start + find.len();
            cursor = start + w.text[start..].chars().next().unwrap().len_utf8();
            work += find.len();
            if work > MAX_MATCH_WORK {
                bail!("matching exceeds bounded work limit; use a Git patch");
            }
            if whole_words && !(boundaries.contains(&start) && boundaries.contains(&end)) {
                continue;
            }
            let a = w
                .map
                .binary_search_by_key(&start, |(n, _)| n.start)
                .map_err(|_| anyhow::anyhow!("invalid match start"))?;
            let b = w
                .map
                .binary_search_by_key(&end, |(n, _)| n.end)
                .map_err(|_| anyhow::anyhow!("invalid match end"))?;
            let span = w.map[a].1.start..w.map[b].1.end;
            if normalize(&source[span.clone()]) != find {
                bail!("unsafe normalized source mapping");
            }
            found.push(span);
        }
    }
    Ok(found)
}
fn skeleton(source: &str, scan: &Scan) -> (Vec<String>, Vec<String>) {
    let mut result = Vec::new();
    let mut previous = 0;
    for run in &scan.runs {
        result.push(source[previous..run.start].to_owned());
        previous = run.end;
    }
    result.push(source[previous..].to_owned());
    (scan.structure.clone(), result)
}
fn location(source: &str, offset: usize) -> String {
    let line = source[..offset].bytes().filter(|b| *b == b'\n').count() + 1;
    let column = source[..offset]
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .chars()
        .count()
        + 1;
    format!("{line}:{column}")
}
fn rule_context(
    index: usize,
    scope: &[String],
    expected: &str,
    found: &[Range<usize>],
    source: &str,
) -> String {
    format!(
        "operation {} scope {:?} expected {expected}, actual {}, candidates [{}]",
        index + 1,
        scope,
        found.len(),
        found
            .iter()
            .map(|r| location(source, r.start))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
fn newline(source: &str) -> Result<&'static str> {
    let crlf = source
        .as_bytes()
        .windows(2)
        .filter(|w| *w == b"\r\n")
        .count();
    let lf = source.bytes().filter(|b| *b == b'\n').count();
    if source.matches('\r').count() != crlf || (crlf > 0 && crlf != lf) {
        bail!("mixed newline styles are unsupported; use a Git patch");
    }
    Ok(if crlf > 0 { "\r\n" } else { "\n" })
}
fn render_insertion(
    source: &str,
    at: usize,
    level: u8,
    heading: &str,
    content: &str,
) -> Result<String> {
    let nl = newline(source)?;
    if !content.is_empty() && newline(content)? != nl && content.contains('\n') {
        bail!("insertion body newline style differs from document");
    }
    let mut inserted = String::new();
    let before = &source[..at];
    if !before.is_empty() && !before.ends_with(nl) {
        inserted.push_str(nl);
    }
    if !before.is_empty() && !before.ends_with(&format!("{nl}{nl}")) {
        inserted.push_str(nl);
    }
    inserted.push_str(&format!(
        "{} {}{nl}{nl}",
        "#".repeat(level as usize),
        heading
    ));
    inserted.push_str(content);
    if !content.is_empty() && !content.ends_with(nl) {
        inserted.push_str(nl);
    }
    if at < source.len() && !inserted.ends_with(&format!("{nl}{nl}")) {
        inserted.push_str(nl);
    }
    Ok(inserted)
}
fn insert(
    source: &str,
    outline: &Scan,
    after: &[String],
    heading: &str,
    content: &str,
) -> Result<(String, usize)> {
    let i = resolve(outline, after, source)?
        .context("missing insertion anchor (expected 1, actual 0, candidates [])")?;
    let anchor = &outline.headings[i];
    let title = normalize(heading);
    let conflicts = outline
        .headings
        .iter()
        .filter(|h| h.parent == anchor.parent && h.title == title)
        .map(|h| location(source, h.range.start))
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        bail!(
            "conflicting sibling heading {heading:?}: expected 0 siblings, actual {}, candidates [{}]",
            conflicts.len(),
            conflicts.join(", ")
        );
    }
    let at = anchor.end;
    let inserted = render_insertion(source, at, anchor.level, heading, content)?;
    let fragment = scan(&inserted)?;
    if fragment
        .headings
        .first()
        .is_none_or(|h| h.level != anchor.level || h.title != normalize(heading))
        || fragment
            .headings
            .iter()
            .skip(1)
            .any(|h| h.level <= anchor.level)
    {
        bail!("insertion content must contain only deeper headings and a literal root heading");
    }
    let mut result = source.to_owned();
    result.insert_str(at, &inserted);
    let parsed = scan(&result)?;
    let expected = outline
        .headings
        .iter()
        .map(|h| (h.level, h.title.clone(), source[h.range.clone()].to_owned()))
        .collect::<Vec<_>>();
    let mut actual = Vec::new();
    let mut added = Vec::new();
    for h in &parsed.headings {
        if h.range.start >= at && h.range.start < at + inserted.len() {
            added.push((h.level, h.title.clone()));
        } else {
            actual.push((h.level, h.title.clone(), result[h.range.clone()].to_owned()));
        }
    }
    if expected != actual
        || added
            != fragment
                .headings
                .iter()
                .map(|h| (h.level, h.title.clone()))
                .collect::<Vec<_>>()
    {
        bail!("insertion changes existing heading structure; use a Git patch");
    }
    let mut retained = result.clone();
    retained.replace_range(at..at + inserted.len(), "");
    if retained != source {
        bail!("insertion changed retained bytes");
    }
    let mut retained_scan = Scan {
        headings: Vec::new(),
        runs: Vec::new(),
        structure: Vec::new(),
        structure_ranges: Vec::new(),
    };
    for (event, range) in parsed.structure.iter().zip(&parsed.structure_ranges) {
        if range.start < at || range.start >= at + inserted.len() {
            retained_scan.structure.push(event.clone());
        }
    }
    for run in &parsed.runs {
        if run.end <= at {
            retained_scan.runs.push(run.clone());
        } else if run.start >= at + inserted.len() {
            retained_scan
                .runs
                .push(run.start - inserted.len()..run.end - inserted.len());
        } else if run.start < at || run.end > at + inserted.len() {
            bail!("insertion absorbs retained prose");
        }
    }
    if skeleton(source, outline) != skeleton(source, &retained_scan) {
        bail!("insertion changes retained structure or protected bytes; use a Git patch");
    }
    let sentinel = format!(
        "{inserted}\n{} sentinel-check\n\nretained sentinel prose\n",
        "#".repeat(anchor.level as usize)
    );
    let sentinel_scan = scan(&sentinel)?;
    if !sentinel_scan
        .headings
        .iter()
        .any(|h| h.title == "sentinel-check")
    {
        bail!("insertion absorbs following content; use a Git patch");
    }
    Ok((result, at))
}
fn apply(source: &str, patch: &SkillPatch) -> Result<(String, PatchReport)> {
    let mut current = source.to_owned();
    let mut report = PatchReport {
        file: patch.file.clone(),
        ..Default::default()
    };
    for (index, op) in patch.operations.iter().enumerate() {
        let parsed = scan(&current)?;
        match op {
            Operation::ReplaceText {
                section,
                find,
                replace,
                whole_words,
                expect,
            } => {
                let found =
                    matches(&current, &parsed, section, find, *whole_words).with_context(|| {
                        format!(
                            "operation {} scope {section:?} expected {}..={}",
                            index + 1,
                            expect.min,
                            expect.max
                        )
                    })?;
                let context = rule_context(
                    index,
                    section,
                    &format!("{}..={}", expect.min, expect.max),
                    &found,
                    &current,
                );
                if found.len() < expect.min || found.len() > expect.max {
                    bail!("{context}");
                }
                if found.windows(2).any(|w| w[0].end > w[1].start) {
                    bail!("{context}: overlapping matches");
                }
                let removed = found.iter().map(|r| r.end - r.start).sum::<usize>();
                let added = replace
                    .len()
                    .checked_mul(found.len())
                    .context("replacement size overflow")?;
                if added > MAX_BYTES - (current.len() - removed) {
                    bail!("{context}: resulting document exceeds {MAX_BYTES} byte limit");
                }
                let mut next = current.clone();
                for span in found.iter().rev() {
                    next.replace_range(span.clone(), replace);
                }
                decode(next.as_bytes())?;
                let next_scan = scan(&next)?;
                if skeleton(&current, &parsed) != skeleton(&next, &next_scan) {
                    bail!("{context}: replacement changes Markdown structure or protected bytes; use a Git patch");
                }
                current = next;
                report.rules.push(context);
            }
            Operation::InsertSection {
                after,
                heading,
                content,
            } => {
                let (next, at) =
                    insert(&current, &parsed, after, heading, content).with_context(|| {
                        format!(
                            "operation {} scope {after:?} expected 1 insertion",
                            index + 1
                        )
                    })?;
                report.rules.push(rule_context(
                    index,
                    after,
                    "1 insertion",
                    std::slice::from_ref(&(at..at)),
                    &current,
                ));
                decode(next.as_bytes())?;
                current = next;
            }
        }
    }
    if current != source {
        report.diff = document_diff(&patch.file, source, &current)?;
    }
    Ok((current, report))
}
fn document_diff(file: &Path, before: &str, after: &str) -> Result<String> {
    let temp = tempfile::tempdir()?;
    let a = temp.path().join("before");
    let b = temp.path().join("after");
    fs::write(&a, before)?;
    fs::write(&b, after)?;
    let output = std::process::Command::new("git")
        .args([
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-color",
            "--",
            util::path_str(&a)?,
            util::path_str(&b)?,
        ])
        .output()?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        bail!("could not render document diff");
    }
    let diff = String::from_utf8(output.stdout)?;
    let body = diff
        .lines()
        .skip_while(|l| !l.starts_with("@@"))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "--- a/{}\n+++ b/{}\n{body}\n",
        file.display(),
        file.display()
    ))
}
pub fn apply_file(tree: &Path, declared: &Path, bytes: &[u8]) -> Result<PatchReport> {
    if declared.to_string_lossy().ends_with(".skillpatch.toml") {
        let patch = parse(bytes).with_context(|| format!("patch {}", declared.display()))?;
        let path = document_path(tree, &patch.file)?;
        let bytes = read_bounded(&path)?;
        let (output, mut report) = apply(decode(&bytes)?, &patch).with_context(|| {
            format!("patch {} file {}", declared.display(), patch.file.display())
        })?;
        fs::write(path, output)?;
        report.patch = declared.to_path_buf();
        Ok(report)
    } else {
        let temp = tempfile::NamedTempFile::new()?;
        fs::write(temp.path(), bytes)?;
        let name = util::path_str(temp.path())?;
        util::git(Some(tree), &["apply", "--check", "--", name])?;
        util::git(Some(tree), &["apply", "--", name])?;
        Ok(PatchReport::default())
    }
}

fn unsupported(source: &str, at: usize, message: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "unsupported edit at {}: {message}; use a Git patch",
        location(source, at)
    )
}
fn first_difference(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or_else(|| a.len().min(b.len()), |((i, _), _)| i)
}
fn unique_headings(scan: &Scan) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for h in &scan.headings {
        if !seen.insert(h.path.clone()) {
            bail!("ambiguous heading path {:?}; use a Git patch", h.path);
        }
    }
    Ok(())
}
struct DiffWindow {
    before: Range<usize>,
    after: Range<usize>,
}
fn previous_word(source: &str, mut at: usize) -> usize {
    while at > 0 && source[..at].chars().next_back().unwrap().is_whitespace() {
        at -= source[..at].chars().next_back().unwrap().len_utf8();
    }
    while at > 0 && !source[..at].chars().next_back().unwrap().is_whitespace() {
        at -= source[..at].chars().next_back().unwrap().len_utf8();
    }
    at
}
fn next_word(source: &str, mut at: usize) -> usize {
    while at < source.len() && source[at..].chars().next().unwrap().is_whitespace() {
        at += source[at..].chars().next().unwrap().len_utf8();
    }
    while at < source.len() && !source[at..].chars().next().unwrap().is_whitespace() {
        at += source[at..].chars().next().unwrap().len_utf8();
    }
    at
}
fn lexical_range(source: &str, mut r: Range<usize>) -> Range<usize> {
    while r.start > 0
        && !source[..r.start]
            .chars()
            .next_back()
            .unwrap()
            .is_whitespace()
    {
        r.start -= source[..r.start].chars().next_back().unwrap().len_utf8();
    }
    while r.end < source.len() && !source[r.end..].chars().next().unwrap().is_whitespace() {
        r.end += source[r.end..].chars().next().unwrap().len_utf8();
    }
    if r.start == r.end || source[r.clone()].starts_with(char::is_whitespace) {
        r.start = previous_word(source, r.start);
    }
    if r.start == r.end || source[r.clone()].ends_with(char::is_whitespace) {
        r.end = next_word(source, r.end);
    }
    r
}
fn diff_windows(before: &str, after: &str) -> Result<Vec<DiffWindow>> {
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    let suffix = before[prefix..]
        .chars()
        .rev()
        .zip(after[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    let a = before[prefix..before.len() - suffix]
        .chars()
        .collect::<Vec<_>>();
    let b = after[prefix..after.len() - suffix]
        .chars()
        .collect::<Vec<_>>();
    let width = b.len().checked_add(1).context("diff size overflow")?;
    let cells = a
        .len()
        .checked_add(1)
        .and_then(|n| n.checked_mul(width))
        .context("diff size overflow")?;
    if cells > MAX_DIFF_CELLS {
        bail!("literal diff exceeds {MAX_DIFF_CELLS} cell limit; use a Git patch");
    }
    let mut lcs = vec![0u32; cells];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * width + j] = if a[i] == b[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let offsets = |chars: &[char]| {
        let mut at = prefix;
        let mut offsets = vec![at];
        for c in chars {
            at += c.len_utf8();
            offsets.push(at);
        }
        offsets
    };
    let ax = offsets(&a);
    let bx = offsets(&b);
    let mut i = 0;
    let mut j = 0;
    let mut windows: Vec<DiffWindow> = Vec::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
            continue;
        }
        let ai = i;
        let bj = j;
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                break;
            }
            if j == b.len() || (i < a.len() && lcs[(i + 1) * width + j] >= lcs[i * width + j + 1]) {
                i += 1;
            } else {
                j += 1;
            }
        }
        let window = DiffWindow {
            before: lexical_range(before, ax[ai]..ax[i]),
            after: lexical_range(after, bx[bj]..bx[j]),
        };
        if let Some(last) = windows.last_mut().filter(|last| {
            last.before.end > window.before.start || last.after.end > window.after.start
        }) {
            last.before.start = last.before.start.min(window.before.start);
            last.after.start = last.after.start.min(window.after.start);
            last.before.end = last.before.end.max(window.before.end);
            last.after.end = last.after.end.max(window.after.end);
        } else {
            windows.push(window);
        }
    }
    Ok(windows)
}
fn generate(file: &Path, base: &str, edited: &str) -> Result<SkillPatch> {
    validate_path(file)?;
    if base == edited {
        bail!("no changes; no patch written");
    }
    let edited_scan = scan(edited)?;
    unique_headings(&edited_scan)?;
    let mut current = base.to_owned();
    let mut operations = Vec::new();
    let mut initial = scan(base)?;
    unique_headings(&initial)?;
    for h in &edited_scan.headings {
        if initial.headings.iter().any(|old| old.path == h.path) {
            continue;
        }
        if h.parent.is_some_and(|p| {
            !initial
                .headings
                .iter()
                .any(|old| old.path == edited_scan.headings[p].path)
        }) {
            continue;
        }
        let anchor = edited_scan
            .headings
            .iter()
            .rev()
            .find(|p| p.range.start < h.range.start && p.parent == h.parent && p.level == h.level)
            .filter(|p| initial.headings.iter().any(|old| old.path == p.path))
            .ok_or_else(|| {
                unsupported(
                    edited,
                    h.range.start,
                    "new section has no existing preceding sibling anchor",
                )
            })?;
        let nl = newline(edited)?;
        let raw = &edited[h.range.end..h.end];
        let body = raw.strip_prefix(nl).unwrap_or(raw);
        let op = Operation::InsertSection {
            after: anchor.path.clone(),
            heading: h.title.clone(),
            content: body.to_owned(),
        };
        let candidate = SkillPatch {
            version: 1,
            file: file.to_path_buf(),
            operations: vec![op],
        };
        current = apply(&current, &candidate)?.0;
        operations.extend(candidate.operations);
        if operations.len() > MAX_OPERATIONS {
            return Err(unsupported(edited, h.range.start, "too many operations"));
        }
        initial = scan(&current)?;
    }
    let current_scan = scan(&current)?;
    unique_headings(&current_scan)?;
    let outline = |s: &Scan| {
        s.headings
            .iter()
            .map(|h| (h.level, h.path.clone()))
            .collect::<Vec<_>>()
    };
    if outline(&current_scan) != outline(&edited_scan)
        || skeleton(&current, &current_scan) != skeleton(edited, &edited_scan)
    {
        return Err(unsupported(
            edited,
            first_difference(&current, edited),
            "formatting, protected bytes, heading movement or prose block structure changed",
        ));
    }
    for ordinal in 0..edited_scan.runs.len() {
        loop {
            let parsed = scan(&current)?;
            let r = parsed.runs[ordinal].clone();
            let e = edited_scan.runs[ordinal].clone();
            let before = &current[r.clone()];
            let after = &edited[e.clone()];
            if before == after {
                break;
            }
            if normalize(before) == normalize(after) {
                return Err(unsupported(edited, e.start, "whitespace-only changes"));
            }
            let windows = diff_windows(before, after)?;
            let first = windows.first().context("no literal difference window")?;
            let mut start = first.before.start;
            let mut end = first.before.end;
            let mut edited_start = first.after.start;
            let mut edited_end = first.after.end;
            let right_limit = windows.get(1).map_or(before.len(), |w| w.before.start);
            let section = parsed
                .headings
                .iter()
                .rev()
                .find(|h| h.range.end <= r.start && r.start < h.end)
                .map_or_else(Vec::new, |h| h.path.clone());
            let mut attempts = 0;
            loop {
                attempts += 1;
                if attempts > MAX_CONTEXT_ATTEMPTS {
                    return Err(unsupported(
                        edited,
                        e.start,
                        "unique literal context exceeds bounded search limit",
                    ));
                }
                let find = &before[start..end];
                if !normalize(find).is_empty()
                    && !find.starts_with(char::is_whitespace)
                    && !find.ends_with(char::is_whitespace)
                    && matches(&current, &parsed, &section, find, false)?.len() == 1
                {
                    break;
                }
                let left = previous_word(before, start);
                let right = next_word(before, end).min(right_limit);
                if left == start && right == end {
                    return Err(unsupported(
                        edited,
                        e.start,
                        "literal prose is not uniquely addressable without crossing another edit",
                    ));
                }
                let new_edited_start = edited_start
                    .checked_sub(start - left)
                    .context("unsafe left diff context")?;
                let new_edited_end = edited_end + (right - end);
                if Some(&before[left..start]) != after.get(new_edited_start..edited_start)
                    || Some(&before[end..right]) != after.get(edited_end..new_edited_end)
                {
                    return Err(unsupported(
                        edited,
                        e.start,
                        "context crosses an independent edit",
                    ));
                }
                start = left;
                end = right;
                edited_start = new_edited_start;
                edited_end = new_edited_end;
            }
            let op = Operation::ReplaceText {
                section,
                find: before[start..end].to_owned(),
                replace: after
                    .get(edited_start..edited_end)
                    .context("unsafe replacement diff window; use a Git patch")?
                    .to_owned(),
                whole_words: false,
                expect: Count { min: 1, max: 1 },
            };
            let candidate = SkillPatch {
                version: 1,
                file: file.to_path_buf(),
                operations: vec![op],
            };
            let next = apply(&current, &candidate)?.0;
            if next == current {
                return Err(unsupported(edited, e.start, "candidate makes no progress"));
            }
            current = next;
            operations.extend(candidate.operations);
            if operations.len() > MAX_OPERATIONS {
                return Err(unsupported(edited, e.start, "too many operations"));
            }
        }
    }
    if current != edited {
        return Err(unsupported(
            edited,
            first_difference(&current, edited),
            "remaining bytes are not representable",
        ));
    }
    Ok(SkillPatch {
        version: 1,
        file: file.to_path_buf(),
        operations,
    })
}
pub fn generate_file(tree: &Path, file: &Path, edited: &[u8]) -> Result<GeneratedPatch> {
    let base_bytes = read_document(tree, file)?;
    let base = decode(&base_bytes)?;
    let edited = decode(edited)?;
    let generated = generate(file, base, edited).with_context(|| {
        if base == edited {
            "no changes; no patch written".to_owned()
        } else {
            format!(
                "unsupported edit at {} in {}; use a Git patch",
                location(edited, first_difference(base, edited)),
                file.display()
            )
        }
    })?;
    let bytes = toml::to_string_pretty(&generated)?.into_bytes();
    let decoded = parse(&bytes)?;
    let (result, report) = apply(base, &decoded)?;
    if result != edited {
        return Err(unsupported(
            edited,
            first_difference(&result, edited),
            "serialized patch does not reproduce all edited bytes",
        ));
    }
    Ok(GeneratedPatch { bytes, report })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn replace(
        section: &[&str],
        find: &str,
        replacement: &str,
        min: usize,
        max: usize,
        words: bool,
    ) -> SkillPatch {
        SkillPatch {
            version: 1,
            file: "skills/x/SKILL.md".into(),
            operations: vec![Operation::ReplaceText {
                section: section.iter().map(|s| s.to_string()).collect(),
                find: find.into(),
                replace: replacement.into(),
                whole_words: words,
                expect: Count { min, max },
            }],
        }
    }
    #[test]
    fn strict_wire_and_limits() {
        let good = toml::to_string(&replace(&[], "a", "b", 1, 1, false)).unwrap();
        assert!(parse(good.as_bytes()).is_ok());
        for bad in [
            good.replace("version = 1", "version = 2"),
            good.replace("replace_text", "remove_sentence"),
            good.replace("find = \"a\"", "find = \" \""),
            good.replace("min = 1", "min = 2"),
            format!("mystery = 1\n{good}"),
            good.replace("max = 1", "max = 1\nmystery = 0"),
            good.replace("whole_words = false", "whole_words = false\nregex = 'a'"),
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(parse(&vec![b'x'; MAX_BYTES + 1]).is_err());
        assert!(decode(&vec![b'x'; MAX_BYTES + 1]).is_err());
        let mut patch = replace(&[], "a", "b", 1, 1, false);
        patch.operations = (0..=MAX_OPERATIONS)
            .map(|_| Operation::InsertSection {
                after: vec!["a".into()],
                heading: "b".into(),
                content: "".into(),
            })
            .collect();
        assert!(parse(toml::to_string(&patch).unwrap().as_bytes()).is_err());
    }
    #[test]
    fn relative_paths_and_symlinks() {
        for bad in [
            "",
            "/x.md",
            "../x.md",
            "a/./x.md",
            "a/../x.md",
            ".git/x.md",
            "x.txt",
            "a//x.md",
        ] {
            assert!(validate_path(Path::new(bad)).is_err(), "{bad}");
        }
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("SKILL.md"), "text").unwrap();
        assert!(read_document(temp.path(), Path::new("SKILL.md")).is_ok());
        fs::write(temp.path().join("large.md"), vec![b'x'; MAX_BYTES + 1]).unwrap();
        assert!(read_document(temp.path(), Path::new("large.md")).is_err());
        assert!(decode(&[0xff]).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(temp.path().join("SKILL.md"), temp.path().join("alias.md"))
                .unwrap();
            std::os::unix::fs::symlink(temp.path(), temp.path().join("dir")).unwrap();
            assert!(read_document(temp.path(), Path::new("alias.md")).is_err());
            assert!(read_document(temp.path(), Path::new("dir/SKILL.md")).is_err());
        }
    }
    #[test]
    fn whitespace_scope_movement_and_chaining() {
        let base = "# Title\n\n## Other\nkeep this\n\n## Tools\nUse old\n  tool here.\n\n### Nested\nold tool too.\n";
        let mut p = replace(&["Tools"], "old tool", "new tool", 2, 2, false);
        p.operations
            .extend(replace(&["Tools"], "new tool", "chosen tool", 2, 2, false).operations);
        let result = apply(base, &p).unwrap().0;
        assert!(result.contains("Use chosen tool here."));
        assert!(result.contains("keep this"));
        let moved =
            "# Title\n\n## Tools\nold tool\n\n### Nested\nold tool\n\n## Other\nkeep this\n";
        assert!(apply(moved, &p).unwrap().0.contains("chosen tool"));
    }
    #[test]
    fn protected_syntax_and_created_markup() {
        let base = "---\nname: target\n---\n# target\n\n## Prose\ntarget `target` [target](target) *target* &target; \\*target\\*\n\n```\ntarget\n```\n\n<div>target</div>\n";
        let p = replace(&["Prose"], "target", "chosen", 4, 4, false);
        let result = apply(base, &p).unwrap().0;
        assert!(result.contains("name: target"));
        assert!(result.contains("`target`"));
        assert!(result.contains("(target)"));
        assert!(result.contains("```\ntarget"));
        assert!(result.contains("<div>target</div>"));
        for find in ["target target", "target &target", "target *target"] {
            assert!(apply(base, &replace(&[], find, "oops", 1, 1, false)).is_err());
        }
        for bad in [
            "*marked*",
            "`code`",
            "[link](url)",
            "new\n\n## Heading",
            "&amp;",
            "\\*",
        ] {
            assert!(
                apply(
                    "plain target text\n",
                    &replace(&[], "target", bad, 1, 1, false)
                )
                .is_err(),
                "{bad}"
            );
        }
    }
    #[test]
    fn unicode_words_overlap_and_absent_scope() {
        let base = "cafe\u{301} cafe café 你好世界 Привет приветствие\n";
        assert!(apply(base, &replace(&[], "cafe", "x", 1, 1, true)).is_ok());
        assert!(apply(base, &replace(&[], "Привет", "x", 1, 1, true)).is_ok());
        assert!(apply(base, &replace(&[], "你好", "x", 1, 1, true)).is_ok());
        assert!(apply("aaaa\n", &replace(&[], "aa", "b", 3, 3, false)).is_err());
        let (_, report) = apply(base, &replace(&["Missing"], "x", "y", 0, 0, false)).unwrap();
        assert!(report.rules[0].contains("actual 0"));
        assert!(apply(
            "## Same\nx\n## Same\nx\n",
            &replace(&["Same"], "x", "y", 0, 2, false)
        )
        .is_err());
        assert!(apply(
            "# First\nx\n# Second\nx\n",
            &replace(&["First"], "x", "y", 1, 1, false)
        )
        .is_ok());
    }
    #[test]
    fn insertion_subtree_siblings_and_termination() {
        let base = "# Title\n\n## A\ntext\n\n### Child\nchild\n\n## B\nother\n";
        let p = SkillPatch {
            version: 1,
            file: "SKILL.md".into(),
            operations: vec![Operation::InsertSection {
                after: vec!["A".into()],
                heading: "New".into(),
                content: "body\n\n### Deep\n```rust\ncode\n```\n".into(),
            }],
        };
        let result = apply(base, &p).unwrap().0;
        assert!(result.find("### Child").unwrap() < result.find("## New").unwrap());
        assert!(result.find("## New").unwrap() < result.find("## B").unwrap());
        for content in ["## Wrong\n", "```\nunterminated\n", "<!-- unclosed\n"] {
            let bad = SkillPatch {
                version: 1,
                file: "SKILL.md".into(),
                operations: vec![Operation::InsertSection {
                    after: vec!["A".into()],
                    heading: "New".into(),
                    content: content.into(),
                }],
            };
            assert!(apply(base, &bad).is_err(), "{content}");
        }
        let conflict = SkillPatch {
            version: 1,
            file: "SKILL.md".into(),
            operations: vec![Operation::InsertSection {
                after: vec!["A".into()],
                heading: "B".into(),
                content: "".into(),
            }],
        };
        let error = format!("{:#}", apply(base, &conflict).err().unwrap());
        assert!(error.contains("expected 0 siblings, actual 1"), "{error}");
        assert!(error.contains("candidates [9:1]"), "{error}");
        let error = format!("{:#}", apply("## Other\ntext\n", &p).err().unwrap());
        assert!(error.contains("expected 1, actual 0"), "{error}");
        assert!(error.contains("candidates []"), "{error}");
        assert!(apply(
            &base.replace('\n', "\r\n"),
            &SkillPatch {
                version: 1,
                file: "SKILL.md".into(),
                operations: vec![Operation::InsertSection {
                    after: vec!["A".into()],
                    heading: "New".into(),
                    content: "body\r\n".into()
                }]
            }
        )
        .is_ok());
    }
    #[test]
    fn generated_minimal_edits_exact_serialized_replay_and_refusals() {
        let base = "# Title\n\n## Tools\nUse old tool here, keep these words.\n\nOther old tool instructions.\n\n## Verify\nCheck carefully.\n";
        let edited = base
            .replace("Use old tool", "Use new tool")
            .replace("Check carefully", "Check twice");
        let generated = generate(Path::new("SKILL.md"), base, &edited).unwrap();
        assert_eq!(generated.operations.len(), 2);
        assert!(
            matches!(&generated.operations[0], Operation::ReplaceText { find, .. } if find == "Use old" || find == "old tool here," || find.contains("old"))
        );
        let decoded = parse(toml::to_string(&generated).unwrap().as_bytes()).unwrap();
        assert_eq!(apply(base, &decoded).unwrap().0, edited);
        for edited in [
            base.replace("# Title", "# Renamed"),
            base.replace("Use old tool", "Use *old* tool"),
            base.replace("\n\n## Verify", "\n## Verify"),
            base.trim_end().to_owned(),
        ] {
            assert!(generate(Path::new("SKILL.md"), base, &edited).is_err());
        }
        assert!(generate(Path::new("SKILL.md"), "## A\nx\n\nx\n", "## A\ny\n\nx\n").is_err());
        let p = SkillPatch {
            version: 1,
            file: "SKILL.md".into(),
            operations: vec![Operation::InsertSection {
                after: vec!["Tools".into()],
                heading: "Extra".into(),
                content: "Added raw body.\n".into(),
            }],
        };
        let edited = apply(base, &p).unwrap().0;
        let generated = generate(Path::new("SKILL.md"), base, &edited).unwrap();
        assert_eq!(apply(base, &generated).unwrap().0, edited);
    }
    #[test]
    fn inline_html_blocks_and_frontmatter_are_barriers() {
        let base = "---\r\nname: target\r\n---\r\n# Title\r\n\r\nBefore <span>target</span> target after.\r\n\r\n- item <span>target</span> target\r\n\r\nSeparate target prose.\r\n";
        let result = apply(base, &replace(&[], "target", "chosen", 1, 1, false))
            .unwrap()
            .0;
        assert!(result.contains("name: target"));
        assert!(result.contains("Before <span>target</span> target after."));
        assert!(result.contains("- item <span>target</span> target"));
        assert!(result.contains("Separate chosen prose."));
        let autolinks = "<https://target.example> <target@example.com> target prose.\n";
        let result = apply(autolinks, &replace(&[], "target", "chosen", 1, 1, false))
            .unwrap()
            .0;
        assert!(result.contains("<https://target.example> <target@example.com> chosen prose."));
        let metadata = "\u{feff}---  \nname: target\n---\n\ntarget prose.\n";
        assert!(
            apply(metadata, &replace(&[], "target", "chosen", 1, 1, false))
                .unwrap()
                .0
                .contains("name: target")
        );
        assert!(apply(
            "---\nname: target\n",
            &replace(&[], "target", "chosen", 1, 1, false)
        )
        .is_err());
        assert!(apply(
            "target &amp; target\n",
            &replace(&[], "target target", "x", 1, 1, false)
        )
        .is_err());
        assert!(apply(
            "target  \ntarget\n",
            &replace(&[], "target target", "x", 1, 1, false)
        )
        .is_err());
    }
    #[test]
    fn generation_unicode_deletion_context_newlines_and_multiple_insertions() {
        for (base, edited) in [
            (
                "# T\n\n## A\nUse old tool here. Other old tool.\n",
                "# T\n\n## A\nUse new tool here. Other old tool.\n",
            ),
            (
                "# T\n\n## A\nUse a simple tool here.\n",
                "# T\n\n## A\nUse a tool here.\n",
            ),
            (
                "# T\r\n\r\n## A\r\nUse café tools.\r\n",
                "# T\r\n\r\n## A\r\nUse local café tools.\r\n",
            ),
            (
                "Unique plain old sentence.\n",
                "Unique plain new sentence.\n",
            ),
        ] {
            let p = generate(Path::new("SKILL.md"), base, edited).unwrap();
            assert_eq!(
                apply(
                    base,
                    &parse(toml::to_string(&p).unwrap().as_bytes()).unwrap()
                )
                .unwrap()
                .0,
                edited
            );
        }
        let base = "# Title\n\nAnchor\n------\n\nBody.\n\n## Next\nOther.\n";
        let insertion = SkillPatch {
            version: 1,
            file: "SKILL.md".into(),
            operations: vec![
                Operation::InsertSection {
                    after: vec!["Anchor".into()],
                    heading: "Extra".into(),
                    content: "raw body\n\n### Nested\n```\nraw code\n```\n".into(),
                },
                Operation::InsertSection {
                    after: vec!["Extra".into()],
                    heading: "Second".into(),
                    content: "second body\n".into(),
                },
            ],
        };
        let edited = apply(base, &insertion).unwrap().0;
        assert_eq!(
            apply(
                base,
                &generate(Path::new("SKILL.md"), base, &edited).unwrap()
            )
            .unwrap()
            .0,
            edited
        );
        assert!(apply("## A\ntext\r\n", &insertion).is_err());
        let p = SkillPatch {
            version: 1,
            file: "SKILL.md".into(),
            operations: vec![Operation::InsertSection {
                after: vec!["A".into()],
                heading: "New".into(),
                content: "[name]: https://example.com\n".into(),
            }],
        };
        assert!(apply("## A\n[label][name]\n", &p).is_err());
    }
    #[test]
    fn replacement_expansion_is_bounded_before_allocation() {
        let base = "target target target\n";
        let replacement = "x".repeat(MAX_BYTES / 2);
        let error = apply(base, &replace(&[], "target", &replacement, 3, 3, false))
            .err()
            .unwrap();
        assert!(error.to_string().contains("resulting document exceeds"));
    }
    #[test]
    fn separate_minimal_windows_and_diff_limit() {
        let base = "# Title\n\n## A\nUse old tools, keep all these intervening words unchanged, verify once today.\n";
        let edited = base.replace("old", "new").replace("once", "twice");
        let generated = generate(Path::new("SKILL.md"), base, &edited).unwrap();
        assert_eq!(generated.operations.len(), 2);
        assert!(
            matches!(&generated.operations[0], Operation::ReplaceText { find, replace, .. } if find == "old" && replace == "new")
        );
        assert!(
            matches!(&generated.operations[1], Operation::ReplaceText { find, replace, .. } if find == "once" && replace == "twice")
        );
        assert_eq!(
            apply(
                base,
                &parse(toml::to_string(&generated).unwrap().as_bytes()).unwrap()
            )
            .unwrap()
            .0,
            edited
        );
        let error = diff_windows(&"a".repeat(1024), &"b".repeat(1024))
            .err()
            .unwrap();
        assert!(error.to_string().contains("cell limit"));
    }
}
