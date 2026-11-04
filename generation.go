package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"unicode"
	"unicode/utf8"
)

func firstDifference(a, b string) int {
	i, j := 0, 0
	for i < len(a) && j < len(b) {
		x, n := utf8.DecodeRuneInString(a[i:])
		y, m := utf8.DecodeRuneInString(b[j:])
		if x != y {
			return i
		}
		i += n
		j += m
	}
	return min(len(a), len(b))
}
func unsupported(source string, at int, message string) error {
	return fmt.Errorf("unsupported edit at %s: %s; use a Git patch", location(source, at), message)
}
func uniqueHeadings(outline *scanResult) error {
	seen := map[string]bool{}
	for _, h := range outline.Headings {
		key := scopeString(h.Path)
		if seen[key] {
			return fmt.Errorf("ambiguous heading path %s; use a Git patch", key)
		}
		seen[key] = true
	}
	return nil
}
func previousWord(source string, at int) int {
	for at > 0 {
		c, n := utf8.DecodeLastRuneInString(source[:at])
		if !unicode.IsSpace(c) {
			break
		}
		at -= n
	}
	for at > 0 {
		c, n := utf8.DecodeLastRuneInString(source[:at])
		if unicode.IsSpace(c) {
			break
		}
		at -= n
	}
	return at
}
func nextWord(source string, at int) int {
	for at < len(source) {
		c, n := utf8.DecodeRuneInString(source[at:])
		if !unicode.IsSpace(c) {
			break
		}
		at += n
	}
	for at < len(source) {
		c, n := utf8.DecodeRuneInString(source[at:])
		if unicode.IsSpace(c) {
			break
		}
		at += n
	}
	return at
}
func startsSpace(s string) bool { c, _ := utf8.DecodeRuneInString(s); return unicode.IsSpace(c) }
func endsSpace(s string) bool   { c, _ := utf8.DecodeLastRuneInString(s); return unicode.IsSpace(c) }
func lexicalRange(source string, r span) span {
	for r.Start > 0 {
		c, n := utf8.DecodeLastRuneInString(source[:r.Start])
		if unicode.IsSpace(c) {
			break
		}
		r.Start -= n
	}
	for r.End < len(source) {
		c, n := utf8.DecodeRuneInString(source[r.End:])
		if unicode.IsSpace(c) {
			break
		}
		r.End += n
	}
	if r.Start == r.End || startsSpace(source[r.Start:r.End]) {
		r.Start = previousWord(source, r.Start)
	}
	if r.Start == r.End || endsSpace(source[r.Start:r.End]) {
		r.End = nextWord(source, r.End)
	}
	return r
}

type diffWindow struct{ Before, After span }

func diffWindows(before, after string) ([]diffWindow, error) {
	prefix := firstDifference(before, after)
	suffix := 0
	for suffix < len(before)-prefix && suffix < len(after)-prefix {
		a, n := utf8.DecodeLastRuneInString(before[prefix : len(before)-suffix])
		b, m := utf8.DecodeLastRuneInString(after[prefix : len(after)-suffix])
		if a != b || n != m {
			break
		}
		suffix += n
	}
	a, b := []rune(before[prefix:len(before)-suffix]), []rune(after[prefix:len(after)-suffix])
	width := len(b) + 1
	if len(a)+1 > maxDiffCells/width {
		return nil, fmt.Errorf("literal diff exceeds %d cell limit; use a Git patch", maxDiffCells)
	}
	lcs := make([]uint32, (len(a)+1)*width)
	for i := len(a) - 1; i >= 0; i-- {
		for j := len(b) - 1; j >= 0; j-- {
			if a[i] == b[j] {
				lcs[i*width+j] = lcs[(i+1)*width+j+1] + 1
			} else {
				lcs[i*width+j] = max(lcs[(i+1)*width+j], lcs[i*width+j+1])
			}
		}
	}
	offsets := func(chars []rune) []int {
		result := []int{prefix}
		at := prefix
		for _, c := range chars {
			at += utf8.RuneLen(c)
			result = append(result, at)
		}
		return result
	}
	ax, bx := offsets(a), offsets(b)
	i, j := 0, 0
	windows := []diffWindow{}
	for i < len(a) || j < len(b) {
		if i < len(a) && j < len(b) && a[i] == b[j] {
			i++
			j++
			continue
		}
		ai, bj := i, j
		for i < len(a) || j < len(b) {
			if i < len(a) && j < len(b) && a[i] == b[j] {
				break
			}
			if j == len(b) || (i < len(a) && lcs[(i+1)*width+j] >= lcs[i*width+j+1]) {
				i++
			} else {
				j++
			}
		}
		w := diffWindow{lexicalRange(before, span{ax[ai], ax[i]}), lexicalRange(after, span{bx[bj], bx[j]})}
		if len(windows) > 0 {
			last := &windows[len(windows)-1]
			if last.Before.End > w.Before.Start || last.After.End > w.After.Start {
				last.Before.Start = min(last.Before.Start, w.Before.Start)
				last.Before.End = max(last.Before.End, w.Before.End)
				last.After.Start = min(last.After.Start, w.After.Start)
				last.After.End = max(last.After.End, w.After.End)
				continue
			}
		}
		windows = append(windows, w)
	}
	return windows, nil
}
func generatePatch(file, base, edited string) (*SkillPatch, error) {
	if err := validateDocumentPath(file); err != nil {
		return nil, err
	}
	if base == edited {
		return nil, fmt.Errorf("no changes; no patch written")
	}
	editedScan, err := scan(edited)
	if err != nil {
		return nil, err
	}
	if err := uniqueHeadings(editedScan); err != nil {
		return nil, err
	}
	current := base
	operations := []Operation{}
	initial, err := scan(base)
	if err != nil {
		return nil, err
	}
	if err := uniqueHeadings(initial); err != nil {
		return nil, err
	}
	contains := func(outline *scanResult, path []string) bool {
		for _, h := range outline.Headings {
			if reflect.DeepEqual(h.Path, path) {
				return true
			}
		}
		return false
	}
	for _, h := range editedScan.Headings {
		if contains(initial, h.Path) {
			continue
		}
		if h.Parent >= 0 && !contains(initial, editedScan.Headings[h.Parent].Path) {
			continue
		}
		anchor := -1
		for i := len(editedScan.Headings) - 1; i >= 0; i-- {
			p := editedScan.Headings[i]
			if p.Range.Start < h.Range.Start && p.Parent == h.Parent && p.Level == h.Level {
				anchor = i
				break
			}
		}
		if anchor < 0 || !contains(initial, editedScan.Headings[anchor].Path) {
			return nil, unsupported(edited, h.Range.Start, "new section has no existing preceding sibling anchor")
		}
		nl, err := newline(edited)
		if err != nil {
			return nil, err
		}
		body := strings.TrimPrefix(edited[h.Range.End:h.End], nl)
		op := Operation{Op: "insert_section", After: editedScan.Headings[anchor].Path, Heading: h.Title, Content: body}
		candidate := &SkillPatch{1, file, []Operation{op}}
		current, _, err = applyPatch(current, candidate)
		if err != nil {
			return nil, err
		}
		operations = append(operations, op)
		if len(operations) > maxOperations {
			return nil, unsupported(edited, h.Range.Start, "too many operations")
		}
		initial, err = scan(current)
		if err != nil {
			return nil, err
		}
	}
	currentScan, err := scan(current)
	if err != nil {
		return nil, err
	}
	if err := uniqueHeadings(currentScan); err != nil {
		return nil, err
	}
	outline := func(s *scanResult) []string {
		out := []string{}
		for _, h := range s.Headings {
			out = append(out, fmt.Sprintf("%d:%s", h.Level, scopeString(h.Path)))
		}
		return out
	}
	if !reflect.DeepEqual(outline(currentScan), outline(editedScan)) || !reflect.DeepEqual(skeleton(current, currentScan), skeleton(edited, editedScan)) {
		return nil, unsupported(edited, firstDifference(current, edited), "formatting, protected bytes, heading movement or prose block structure changed")
	}
	for ordinal := range editedScan.Runs {
		for {
			parsed, err := scan(current)
			if err != nil {
				return nil, err
			}
			if ordinal >= len(parsed.Runs) {
				return nil, fmt.Errorf("prose structure changed; use a Git patch")
			}
			r, e := parsed.Runs[ordinal], editedScan.Runs[ordinal]
			before, after := current[r.Start:r.End], edited[e.Start:e.End]
			if before == after {
				break
			}
			if normalize(before) == normalize(after) {
				return nil, unsupported(edited, e.Start, "whitespace-only changes")
			}
			windows, err := diffWindows(before, after)
			if err != nil {
				return nil, err
			}
			if len(windows) == 0 {
				return nil, fmt.Errorf("no literal difference window")
			}
			start, end := windows[0].Before.Start, windows[0].Before.End
			editedStart, editedEnd := windows[0].After.Start, windows[0].After.End
			rightLimit := len(before)
			if len(windows) > 1 {
				rightLimit = windows[1].Before.Start
			}
			section := []string{}
			for i := len(parsed.Headings) - 1; i >= 0; i-- {
				h := parsed.Headings[i]
				if h.Range.End <= r.Start && r.Start < h.End {
					section = h.Path
					break
				}
			}
			for attempts := 1; ; attempts++ {
				if attempts > 32 {
					return nil, unsupported(edited, e.Start, "unique literal context exceeds bounded search limit")
				}
				find := before[start:end]
				if normalize(find) != "" && !startsSpace(find) && !endsSpace(find) {
					found, err := matches(current, parsed, section, find, false)
					if err != nil {
						return nil, err
					}
					if len(found) == 1 {
						break
					}
				}
				left, right := previousWord(before, start), min(nextWord(before, end), rightLimit)
				if left == start && right == end {
					return nil, unsupported(edited, e.Start, "literal prose is not uniquely addressable without crossing another edit")
				}
				newStart, newEnd := editedStart-(start-left), editedEnd+(right-end)
				if newStart < 0 || newEnd > len(after) || before[left:start] != after[newStart:editedStart] || before[end:right] != after[editedEnd:newEnd] {
					return nil, unsupported(edited, e.Start, "context crosses an independent edit")
				}
				start, end, editedStart, editedEnd = left, right, newStart, newEnd
			}
			op := Operation{Op: "replace_text", Section: section, Find: before[start:end], Replace: after[editedStart:editedEnd], Expect: Count{1, 1}}
			next, _, err := applyPatch(current, &SkillPatch{1, file, []Operation{op}})
			if err != nil {
				return nil, err
			}
			if next == current {
				return nil, unsupported(edited, e.Start, "candidate makes no progress")
			}
			current = next
			operations = append(operations, op)
			if len(operations) > maxOperations {
				return nil, unsupported(edited, e.Start, "too many operations")
			}
		}
	}
	if current != edited {
		return nil, unsupported(edited, firstDifference(current, edited), "remaining bytes are not representable")
	}
	return &SkillPatch{1, file, operations}, nil
}
func tomlArray(items []string) string {
	if len(items) == 0 {
		return "[]"
	}
	if len(items) == 1 {
		return "[" + tomlString(items[0]) + "]"
	}
	var b strings.Builder
	b.WriteString("[\n")
	for _, item := range items {
		b.WriteString("    " + tomlString(item) + ",\n")
	}
	b.WriteString("]")
	return b.String()
}
func serializePatch(p *SkillPatch) []byte {
	var b strings.Builder
	fmt.Fprintf(&b, "version = %d\nfile = %s\n", p.Version, tomlString(p.File))
	for _, op := range p.Operations {
		fmt.Fprintf(&b, "\n[[operations]]\nop = %s\n", tomlString(op.Op))
		if op.Op == "replace_text" {
			fmt.Fprintf(&b, "section = %s\nfind = %s\nreplace = %s\nwhole_words = %t\n\n[operations.expect]\nmin = %d\nmax = %d\n", tomlArray(op.Section), tomlString(op.Find), tomlString(op.Replace), op.WholeWords, op.Expect.Min, op.Expect.Max)
		} else {
			fmt.Fprintf(&b, "after = %s\nheading = %s\ncontent = %s\n", tomlArray(op.After), tomlString(op.Heading), tomlString(op.Content))
		}
	}
	return []byte(b.String())
}
func generateFile(tree, file string, editedBytes []byte) ([]byte, PatchReport, error) {
	baseBytes, err := readDocument(tree, file)
	if err != nil {
		return nil, PatchReport{}, err
	}
	base, err := decodeDocument(baseBytes)
	if err != nil {
		return nil, PatchReport{}, err
	}
	edited, err := decodeDocument(editedBytes)
	if err != nil {
		return nil, PatchReport{}, err
	}
	p, err := generatePatch(file, base, edited)
	if err != nil {
		return nil, PatchReport{}, fmt.Errorf("unsupported edit at %s in %s; use a Git patch: %w", location(edited, firstDifference(base, edited)), file, err)
	}
	data := serializePatch(p)
	decoded, err := parsePatch(data)
	if err != nil {
		return nil, PatchReport{}, err
	}
	result, report, err := applyPatch(base, decoded)
	if err != nil {
		return nil, report, err
	}
	if result != edited {
		return nil, report, unsupported(edited, firstDifference(result, edited), "serialized patch does not reproduce all edited bytes")
	}
	return data, report, nil
}
func checkInputs(c *Loaded, lockBytes []byte, inputs PatchInputs) error {
	manifest, err := os.ReadFile(c.ManifestPath)
	if err != nil {
		return err
	}
	lock, err := os.ReadFile(c.Lock)
	if err != nil {
		return err
	}
	if !bytes.Equal(manifest, c.ManifestBytes) || !bytes.Equal(lock, lockBytes) {
		return fmt.Errorf("manifest or lockfile changed during generation; no output written")
	}
	for _, id := range sortedKeys(inputs) {
		for _, input := range inputs[id] {
			path, err := absolute(c.Base, input.Info.Path)
			if err != nil {
				return err
			}
			data, err := os.ReadFile(path)
			if err != nil {
				return err
			}
			if !bytes.Equal(data, input.Bytes) {
				return fmt.Errorf("patch %s changed during generation; no output written", input.Info.Path)
			}
		}
	}
	return nil
}
func validateOutput(c *Loaded, cwd, output string) (string, error) {
	if !strings.HasSuffix(output, ".skillpatch.toml") {
		return "", fmt.Errorf("output must end in .skillpatch.toml")
	}
	resolved, err := resolvedPath(cwd, output)
	if err != nil {
		return "", err
	}
	for _, path := range []string{c.ManifestPath, c.Lock} {
		reserved, err := resolvedPath(cwd, path)
		if err != nil {
			return "", err
		}
		if resolved == reserved {
			return "", fmt.Errorf("unsafe output: collides with manifest or lockfile")
		}
	}
	installed, err := installedTargets(c)
	if err != nil {
		return "", err
	}
	roots := append([]string{c.Prefix}, c.Targets...)
	roots = append(roots, installed...)
	for _, root := range roots {
		canonical, err := resolvedPath(cwd, root)
		if err != nil {
			return "", err
		}
		if within(resolved, canonical) {
			return "", fmt.Errorf("unsafe output: inside cache, prefix or installed target")
		}
	}
	return resolved, nil
}
func generateRun(c *Loaded, id, file, edited, output string) error {
	guard, err := prefixGuard(c, false)
	if err != nil {
		return err
	}
	if guard != nil {
		defer guard.Close()
	}
	if exists(filepath.Join(c.Prefix, "transaction.json")) {
		return fmt.Errorf("interrupted transaction; generation does not recover it; run sync first")
	}
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}
	outputPath, err := validateOutput(c, cwd, output)
	if err != nil {
		return err
	}
	lockBytes, err := os.ReadFile(c.Lock)
	if err != nil {
		return fmt.Errorf("lockfile is missing; run sync first: %w", err)
	}
	inputs, err := loadPatchInputs(c)
	if err != nil {
		return err
	}
	locked, err := requireCurrent(c, inputs)
	if err != nil {
		return err
	}
	if err := checkInputs(c, lockBytes, inputs); err != nil {
		return err
	}
	baseline, err := lockedBaseline(c, id, locked, inputs)
	if err != nil {
		return err
	}
	editedPath, err := absolute(cwd, edited)
	if err != nil {
		return err
	}
	editedBytes, err := readBounded(editedPath)
	if err != nil {
		return err
	}
	document, err := readDocument(baseline.Tree, file)
	if err != nil {
		return err
	}
	data, report, err := generateFile(baseline.Tree, file, editedBytes)
	if err != nil {
		return err
	}
	if err := checkInputs(c, lockBytes, inputs); err != nil {
		return err
	}
	currentOutput, err := validateOutput(c, cwd, output)
	if err != nil {
		return err
	}
	if currentOutput != outputPath {
		return fmt.Errorf("output ancestors changed during generation")
	}
	if err := createNew(outputPath, data); err != nil {
		return err
	}
	fmt.Printf("Baseline %s@%s\n", id, baseline.Locked.Revision)
	for _, input := range inputs[id] {
		fmt.Printf("Existing patch %s sha256 %s\n", input.Info.Path, input.Info.SHA256)
	}
	fmt.Printf("Baseline document %s sha256 %s\n", file, hash(document))
	report.Patch = output
	report.Print(false)
	fmt.Printf("Created %s (append to source patches)\n", output)
	return nil
}
