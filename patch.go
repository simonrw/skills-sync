package main

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strings"
	"unicode/utf8"

	"github.com/pelletier/go-toml/v2"
)

type Count struct {
	Min uint64 `toml:"min"`
	Max uint64 `toml:"max"`
}
type Operation struct {
	Op         string   `toml:"op"`
	Section    []string `toml:"section"`
	Find       string   `toml:"find"`
	Replace    string   `toml:"replace"`
	WholeWords bool     `toml:"whole_words"`
	Expect     Count    `toml:"expect"`
	After      []string `toml:"after"`
	Heading    string   `toml:"heading"`
	Content    string   `toml:"content"`
}
type SkillPatch struct {
	Version    uint32      `toml:"version"`
	File       string      `toml:"file"`
	Operations []Operation `toml:"operations"`
}
type PatchReport struct {
	Patch, File string
	Rules       []string
	Diff        string
}

func (r PatchReport) Print(dryRun bool) {
	for _, rule := range r.Rules {
		fmt.Printf("patch %s file %s %s\n", r.Patch, r.File, rule)
	}
	if dryRun && r.Diff != "" {
		fmt.Print(r.Diff)
	}
}
func validateDocumentPath(path string) error {
	valid := path != "" && !filepath.IsAbs(path)
	for _, part := range strings.FieldsFunc(path, func(r rune) bool { return r == '/' || r == '\\' }) {
		if part == "." || part == ".." || part == ".git" {
			valid = false
		}
	}
	for _, part := range strings.Split(strings.ReplaceAll(path, "\\", "/"), "/") {
		if part == "" {
			valid = false
		}
	}
	ext := strings.ToLower(filepath.Ext(path))
	if ext != ".md" && ext != ".markdown" {
		valid = false
	}
	if !valid {
		return fmt.Errorf("file must be a source-relative Markdown path with normal components, without .git")
	}
	return nil
}
func readBounded(path string) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, maxBytes+1))
	if err != nil {
		return nil, err
	}
	if len(data) > maxBytes {
		return nil, fmt.Errorf("%s exceeds %d byte limit", path, maxBytes)
	}
	return data, nil
}
func documentPath(tree, file string) (string, error) {
	if err := validateDocumentPath(file); err != nil {
		return "", err
	}
	root, err := os.Lstat(tree)
	if err != nil {
		return "", err
	}
	if !root.IsDir() || root.Mode()&os.ModeSymlink != 0 {
		return "", fmt.Errorf("source root must be a regular directory")
	}
	parts := strings.Split(file, string(filepath.Separator))
	path := tree
	for i, part := range parts {
		path = filepath.Join(path, part)
		meta, err := os.Lstat(path)
		if err != nil {
			return "", err
		}
		if meta.Mode()&os.ModeSymlink != 0 {
			return "", fmt.Errorf("symlink document or ancestor: %s", path)
		}
		if i == len(parts)-1 && !meta.Mode().IsRegular() || i < len(parts)-1 && !meta.IsDir() {
			return "", fmt.Errorf("document requires regular file and directory ancestors: %s", path)
		}
	}
	return path, nil
}
func readDocument(tree, file string) ([]byte, error) {
	path, err := documentPath(tree, file)
	if err != nil {
		return nil, err
	}
	return readBounded(path)
}
func decodeDocument(data []byte) (string, error) {
	if len(data) > maxBytes {
		return "", fmt.Errorf("document exceeds %d byte limit", maxBytes)
	}
	if !utf8.Valid(data) {
		return "", fmt.Errorf("Markdown document must be UTF-8")
	}
	return string(data), nil
}
func parsePatch(data []byte) (*SkillPatch, error) {
	if len(data) > maxBytes {
		return nil, fmt.Errorf("patch exceeds %d byte limit", maxBytes)
	}
	var p SkillPatch
	if err := strictTOML(data, &p); err != nil {
		return nil, fmt.Errorf("invalid skill patch; only replace_text and insert_section are supported (sentence/regex operations are deferred): %w", err)
	}
	var raw map[string]any
	if err := toml.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	if p.Version != 1 {
		return nil, fmt.Errorf("unsupported skill patch version %d", p.Version)
	}
	if err := validateDocumentPath(p.File); err != nil {
		return nil, err
	}
	if len(p.Operations) == 0 || len(p.Operations) > maxOperations {
		return nil, fmt.Errorf("skill patch requires 1..=%d operations", maxOperations)
	}
	rawOps, _ := raw["operations"].([]any)
	for i, op := range p.Operations {
		fields, ok := rawOps[i].(map[string]any)
		if !ok {
			return nil, fmt.Errorf("invalid operation")
		}
		required := []string{}
		optional := ""
		scope := op.Section
		switch op.Op {
		case "replace_text":
			required = []string{"op", "section", "find", "replace", "expect"}
			optional = "whole_words"
			if normalize(op.Find) == "" {
				return nil, fmt.Errorf("find must contain non-whitespace prose")
			}
			if op.Expect.Min > op.Expect.Max {
				return nil, fmt.Errorf("expected min must be <= max")
			}
			expect, ok := fields["expect"].(map[string]any)
			if !ok || expect["min"] == nil || expect["max"] == nil {
				return nil, fmt.Errorf("invalid expected count")
			}
		case "insert_section":
			required = []string{"op", "after", "heading", "content"}
			scope = op.After
			if len(op.After) == 0 {
				return nil, fmt.Errorf("insertion requires a nonempty anchor path")
			}
			if normalize(op.Heading) == "" || strings.ContainsAny(op.Heading, "\r\n") {
				return nil, fmt.Errorf("insertion heading must be a nonempty single line")
			}
		default:
			return nil, fmt.Errorf("invalid skill patch; only replace_text and insert_section are supported (sentence/regex operations are deferred)")
		}
		allowed := map[string]bool{optional: true}
		for _, name := range required {
			allowed[name] = true
			if _, ok := fields[name]; !ok {
				return nil, fmt.Errorf("missing field %s", name)
			}
		}
		for name := range fields {
			if !allowed[name] {
				return nil, fmt.Errorf("unknown field %s", name)
			}
		}
		for _, part := range scope {
			if normalize(part) == "" {
				return nil, fmt.Errorf("empty heading path component")
			}
		}
	}
	return &p, nil
}
func renderInsertion(source string, at, level int, heading, content string) (string, error) {
	nl, err := newline(source)
	if err != nil {
		return "", err
	}
	if content != "" {
		bodyNL, err := newline(content)
		if err != nil {
			return "", err
		}
		if bodyNL != nl && strings.Contains(content, "\n") {
			return "", fmt.Errorf("insertion body newline style differs from document")
		}
	}
	var b strings.Builder
	before := source[:at]
	if before != "" && !strings.HasSuffix(before, nl) {
		b.WriteString(nl)
	}
	if before != "" && !strings.HasSuffix(before, nl+nl) {
		b.WriteString(nl)
	}
	b.WriteString(strings.Repeat("#", level) + " " + heading + nl + nl)
	b.WriteString(content)
	if content != "" && !strings.HasSuffix(content, nl) {
		b.WriteString(nl)
	}
	if at < len(source) && !strings.HasSuffix(b.String(), nl+nl) {
		b.WriteString(nl)
	}
	return b.String(), nil
}
func insertSection(source string, outline *scanResult, after []string, title, content string) (string, int, error) {
	i, err := resolve(outline, after, source)
	if err != nil {
		return "", 0, err
	}
	if i < 0 {
		return "", 0, fmt.Errorf("missing insertion anchor (expected 1, actual 0, candidates [])")
	}
	anchor := outline.Headings[i]
	conflicts := []string{}
	for _, h := range outline.Headings {
		if h.Parent == anchor.Parent && h.Title == normalize(title) {
			conflicts = append(conflicts, location(source, h.Range.Start))
		}
	}
	if len(conflicts) > 0 {
		return "", 0, fmt.Errorf("conflicting sibling heading %q: expected 0 siblings, actual %d, candidates [%s]", title, len(conflicts), strings.Join(conflicts, ", "))
	}
	at := anchor.End
	inserted, err := renderInsertion(source, at, anchor.Level, title, content)
	if err != nil {
		return "", 0, err
	}
	fragment, err := scan(inserted)
	if err != nil {
		return "", 0, err
	}
	if len(fragment.Headings) == 0 || fragment.Headings[0].Level != anchor.Level || fragment.Headings[0].Title != normalize(title) {
		return "", 0, fmt.Errorf("insertion content must contain only deeper headings and a literal root heading")
	}
	for _, h := range fragment.Headings[1:] {
		if h.Level <= anchor.Level {
			return "", 0, fmt.Errorf("insertion content must contain only deeper headings and a literal root heading")
		}
	}
	result := source[:at] + inserted + source[at:]
	parsed, err := scan(result)
	if err != nil {
		return "", 0, err
	}
	type identity struct {
		Level      int
		Title, Raw string
	}
	expected, actual := []identity{}, []identity{}
	added, expectedAdded := []identity{}, []identity{}
	for _, h := range outline.Headings {
		expected = append(expected, identity{h.Level, h.Title, source[h.Range.Start:h.Range.End]})
	}
	for _, h := range parsed.Headings {
		if h.Range.Start >= at && h.Range.Start < at+len(inserted) {
			added = append(added, identity{h.Level, h.Title, ""})
		} else {
			actual = append(actual, identity{h.Level, h.Title, result[h.Range.Start:h.Range.End]})
		}
	}
	for _, h := range fragment.Headings {
		expectedAdded = append(expectedAdded, identity{h.Level, h.Title, ""})
	}
	if !reflect.DeepEqual(expected, actual) || !reflect.DeepEqual(added, expectedAdded) {
		return "", 0, fmt.Errorf("insertion changes existing heading structure; use a Git patch")
	}
	retained := &scanResult{Runs: []span{}, Structure: []string{}}
	for i, event := range parsed.Structure {
		r := parsed.StructureRanges[i]
		if r.Start < at || r.Start >= at+len(inserted) {
			retained.Structure = append(retained.Structure, event)
		}
	}
	for _, r := range parsed.Runs {
		if r.End <= at {
			retained.Runs = append(retained.Runs, r)
		} else if r.Start >= at+len(inserted) {
			retained.Runs = append(retained.Runs, span{r.Start - len(inserted), r.End - len(inserted)})
		} else if r.Start < at || r.End > at+len(inserted) {
			return "", 0, fmt.Errorf("insertion absorbs retained prose")
		}
	}
	if !reflect.DeepEqual(skeleton(source, outline), skeleton(source, retained)) {
		return "", 0, fmt.Errorf("insertion changes retained structure or protected bytes; use a Git patch")
	}
	sentinel := inserted + "\n" + strings.Repeat("#", anchor.Level) + " sentinel-check\n\nretained sentinel prose\n"
	sentinelScan, err := scan(sentinel)
	if err != nil {
		return "", 0, err
	}
	found := false
	for _, h := range sentinelScan.Headings {
		if h.Title == "sentinel-check" {
			found = true
		}
	}
	if !found {
		return "", 0, fmt.Errorf("insertion absorbs following content; use a Git patch")
	}
	return result, at, nil
}
func applyPatch(source string, p *SkillPatch) (string, PatchReport, error) {
	current := source
	report := PatchReport{File: p.File}
	for index, op := range p.Operations {
		parsed, err := scan(current)
		if err != nil {
			return "", report, err
		}
		if op.Op == "replace_text" {
			found, err := matches(current, parsed, op.Section, op.Find, op.WholeWords)
			if err != nil {
				return "", report, fmt.Errorf("operation %d scope %s expected %d..=%d: %w", index+1, scopeString(op.Section), op.Expect.Min, op.Expect.Max, err)
			}
			context := ruleContext(index, op.Section, fmt.Sprintf("%d..=%d", op.Expect.Min, op.Expect.Max), found, current)
			if uint64(len(found)) < op.Expect.Min || uint64(len(found)) > op.Expect.Max {
				return "", report, fmt.Errorf("%s", context)
			}
			removed := 0
			for i, r := range found {
				if i > 0 && found[i-1].End > r.Start {
					return "", report, fmt.Errorf("%s: overlapping matches", context)
				}
				removed += r.End - r.Start
			}
			if uint64(len(op.Replace))*uint64(len(found)) > uint64(maxBytes-(len(current)-removed)) {
				return "", report, fmt.Errorf("%s: resulting document exceeds %d byte limit", context, maxBytes)
			}
			var b strings.Builder
			cursor := 0
			for _, r := range found {
				b.WriteString(current[cursor:r.Start])
				b.WriteString(op.Replace)
				cursor = r.End
			}
			b.WriteString(current[cursor:])
			next := b.String()
			nextScan, err := scan(next)
			if err != nil {
				return "", report, err
			}
			if !reflect.DeepEqual(skeleton(current, parsed), skeleton(next, nextScan)) {
				return "", report, fmt.Errorf("%s: replacement changes Markdown structure or protected bytes; use a Git patch", context)
			}
			current = next
			report.Rules = append(report.Rules, context)
		} else {
			next, at, err := insertSection(current, parsed, op.After, op.Heading, op.Content)
			if err != nil {
				return "", report, fmt.Errorf("operation %d scope %s expected 1 insertion: %w", index+1, scopeString(op.After), err)
			}
			if _, err := decodeDocument([]byte(next)); err != nil {
				return "", report, err
			}
			report.Rules = append(report.Rules, ruleContext(index, op.After, "1 insertion", []span{{at, at}}, current))
			current = next
		}
	}
	if current != source {
		diff, err := documentDiff(p.File, source, current)
		if err != nil {
			return "", report, err
		}
		report.Diff = diff
	}
	return current, report, nil
}
func documentDiff(file, before, after string) (string, error) {
	temp, err := os.MkdirTemp("", "skills-sync-diff-")
	if err != nil {
		return "", err
	}
	defer os.RemoveAll(temp)
	a, b := filepath.Join(temp, "before"), filepath.Join(temp, "after")
	if err := os.WriteFile(a, []byte(before), 0600); err != nil {
		return "", err
	}
	if err := os.WriteFile(b, []byte(after), 0600); err != nil {
		return "", err
	}
	cmd := exec.Command("git", "diff", "--no-index", "--no-ext-diff", "--no-color", "--", a, b)
	output, err := cmd.Output()
	if err != nil {
		if exit, ok := err.(*exec.ExitError); !ok || exit.ExitCode() != 1 {
			return "", fmt.Errorf("could not render document diff: %w", err)
		}
	}
	lines := strings.Split(strings.TrimSuffix(string(output), "\n"), "\n")
	for i := range lines {
		lines[i] = strings.TrimSuffix(lines[i], "\r")
	}
	start := len(lines)
	for i, l := range lines {
		if strings.HasPrefix(l, "@@") {
			start = i
			break
		}
	}
	return fmt.Sprintf("--- a/%s\n+++ b/%s\n%s\n", file, file, strings.Join(lines[start:], "\n")), nil
}
func applyFile(tree, declared string, data []byte) (PatchReport, error) {
	if strings.HasSuffix(declared, ".skillpatch.toml") {
		p, err := parsePatch(data)
		if err != nil {
			return PatchReport{}, fmt.Errorf("patch %s: %w", declared, err)
		}
		path, err := documentPath(tree, p.File)
		if err != nil {
			return PatchReport{}, err
		}
		bytes, err := readBounded(path)
		if err != nil {
			return PatchReport{}, err
		}
		source, err := decodeDocument(bytes)
		if err != nil {
			return PatchReport{}, err
		}
		output, report, err := applyPatch(source, p)
		if err != nil {
			return report, fmt.Errorf("patch %s file %s: %w", declared, p.File, err)
		}
		report.Patch = declared
		return report, os.WriteFile(path, []byte(output), 0666)
	}
	f, err := os.CreateTemp("", "skills-sync-patch-")
	if err != nil {
		return PatchReport{}, err
	}
	defer os.Remove(f.Name())
	defer f.Close()
	if _, err := f.Write(data); err != nil {
		return PatchReport{}, err
	}
	if err := f.Close(); err != nil {
		return PatchReport{}, err
	}
	if _, err := git(tree, "apply", "--check", "--", f.Name()); err != nil {
		return PatchReport{}, err
	}
	_, err = git(tree, "apply", "--", f.Name())
	return PatchReport{}, err
}
