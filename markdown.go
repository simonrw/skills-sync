package main

import (
	"fmt"
	"reflect"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/clipperhouse/uax29/v2/words"
	"github.com/yuin/goldmark"
	"github.com/yuin/goldmark/ast"
	"github.com/yuin/goldmark/extension"
	"github.com/yuin/goldmark/text"
	"github.com/yuin/goldmark/util"
)

const maxBytes = 1024 * 1024
const maxOperations = 256
const maxMatchWork = 16 * maxBytes
const maxDiffCells = 1_048_576

type span struct{ Start, End int }
type heading struct {
	Level  int
	Title  string
	Path   []string
	Parent int
	Range  span
	End    int
}
type scanResult struct {
	Headings        []heading
	Runs            []span
	Structure       []string
	StructureRanges []span
}

func normalize(s string) string { return strings.Join(strings.Fields(s), " ") }
func frontmatter(source string) int {
	lines := strings.SplitAfter(source, "\n")
	if len(lines) == 0 || strings.TrimLeft(strings.TrimRightFunc(lines[0], unicode.IsSpace), "\ufeff") != "---" {
		return 0
	}
	end := len(lines[0])
	for _, line := range lines[1:] {
		end += len(line)
		trimmed := strings.TrimRightFunc(line, unicode.IsSpace)
		if trimmed == "---" || trimmed == "..." {
			return end
		}
	}
	return len(source)
}
func lineStart(source string, at int) int { return strings.LastIndex(source[:at], "\n") + 1 }
func lineEnd(source string, at int) int {
	if i := strings.IndexByte(source[at:], '\n'); i >= 0 {
		return at + i + 1
	}
	return len(source)
}
func nodeRange(n ast.Node, source []byte) span {
	if t, ok := n.(*ast.Text); ok {
		return span{t.Segment.Start, t.Segment.Stop}
	}
	if n.Type() == ast.TypeBlock && n.Lines().Len() > 0 {
		lines := n.Lines()
		return span{lines.At(0).Start, lines.At(lines.Len() - 1).Stop}
	}
	first, last := n.FirstChild(), n.LastChild()
	if first != nil {
		a, b := nodeRange(first, source), nodeRange(last, source)
		return span{a.Start, b.End}
	}
	return span{}
}

var escapedProse = regexp.MustCompile("\\\\[!\"#$%&'()*+,./:;<=>?@\\[\\]\\\\^_`{|}~-]|&(?:#[xX][0-9a-fA-F]+|#[0-9]+|[A-Za-z][A-Za-z0-9]+);")

func decodeMarkdownLiteral(raw string) string {
	if raw[0] == '\\' {
		return raw[1:]
	}
	name := raw[1 : len(raw)-1]
	if strings.HasPrefix(name, "#") {
		digits, base := name[1:], 10
		if strings.HasPrefix(digits, "x") || strings.HasPrefix(digits, "X") {
			digits, base = digits[1:], 16
		}
		codepoint, err := strconv.ParseUint(digits, base, 64)
		if err != nil || codepoint > utf8.MaxRune || codepoint >= 0xd800 && codepoint <= 0xdfff {
			return raw
		}
		if codepoint == 0 {
			return "\ufffd"
		}
		return string(rune(codepoint))
	}
	if entity, ok := util.LookUpHTML5EntityByName(name); ok {
		return string(entity.Characters)
	}
	return raw
}

func literalParts(source string, r span) []span {
	parts := []span{}
	start := r.Start
	for _, m := range escapedProse.FindAllStringIndex(source[r.Start:r.End], -1) {
		a, b := r.Start+m[0], r.Start+m[1]
		raw := source[a:b]
		if raw[0] == '&' && decodeMarkdownLiteral(raw) == raw {
			continue
		}
		if a > start {
			parts = append(parts, span{start, a})
		}
		start = b
	}
	if start < r.End {
		parts = append(parts, span{start, r.End})
	}
	return parts
}
func headingTitle(n ast.Node, source []byte) string {
	var b strings.Builder
	ast.Walk(n, func(child ast.Node, enter bool) (ast.WalkStatus, error) {
		if !enter {
			return ast.WalkContinue, nil
		}
		switch t := child.(type) {
		case *ast.Text:
			s := string(t.Value(source))
			if !t.IsRaw() {
				s = escapedProse.ReplaceAllStringFunc(s, decodeMarkdownLiteral)
			}
			b.WriteString(s)
			if t.SoftLineBreak() {
				b.WriteByte(' ')
			}
		case *ast.String:
			b.Write(t.Value)
		}
		return ast.WalkContinue, nil
	})
	return normalize(b.String())
}
func scan(source string) (*scanResult, error) {
	metadata := frontmatter(source)
	data := []byte(source)
	parser := goldmark.New(goldmark.WithExtensions(extension.Table, extension.Strikethrough)).Parser()
	document := parser.Parse(text.NewReader(data))
	result := &scanResult{Headings: []heading{}, Runs: []span{}, Structure: []string{}, StructureRanges: []span{}}
	barriers := map[ast.Node]bool{}
	ast.Walk(document, func(n ast.Node, enter bool) (ast.WalkStatus, error) {
		if enter && n.Kind() == ast.KindRawHTML {
			for p := n.Parent(); p != nil; p = p.Parent() {
				if p.Kind() == ast.KindParagraph || p.Kind() == ast.KindListItem || p.Kind() == ast.KindTextBlock {
					barriers[p] = true
				}
			}
		}
		return ast.WalkContinue, nil
	})
	var run *span
	flush := func() {
		if run != nil {
			if normalize(source[run.Start:run.End]) != "" {
				result.Runs = append(result.Runs, *run)
			}
			run = nil
		}
	}
	appendRun := func(r span) {
		if r.End <= r.Start {
			return
		}
		if run != nil && (run.End == r.Start || run.End < r.Start && normalize(source[run.End:r.Start]) == "") {
			run.End = r.End
		} else {
			flush()
			copy := r
			run = &copy
		}
	}
	structureBytes := 0
	err := ast.Walk(document, func(n ast.Node, enter bool) (ast.WalkStatus, error) {
		if n == document {
			return ast.WalkContinue, nil
		}
		r := nodeRange(n, data)
		if enter && r.End > 0 && r.Start < metadata {
			return ast.WalkSkipChildren, nil
		}
		if t, ok := n.(*ast.Text); ok {
			if !enter {
				return ast.WalkContinue, nil
			}
			eligible := false
			protected := false
			for p := n.Parent(); p != nil; p = p.Parent() {
				switch p.Kind() {
				case ast.KindParagraph, ast.KindListItem, ast.KindTextBlock:
					eligible = true
				case ast.KindHeading, ast.KindCodeBlock, ast.KindFencedCodeBlock, ast.KindCodeSpan, ast.KindImage, ast.KindAutoLink:
					protected = true
				}
				if barriers[p] {
					protected = true
				}
			}
			if eligible && !protected {
				parts := literalParts(source, r)
				cursor := r.Start
				for _, part := range parts {
					if part.Start != cursor {
						flush()
					}
					appendRun(part)
					cursor = part.End
				}
				if cursor != r.End {
					flush()
				}
				if t.SoftLineBreak() && run != nil {
					end := lineEnd(source, r.End)
					if normalize(source[r.End:end]) == "" {
						run.End = end
					} else {
						flush()
					}
				}
				if t.HardLineBreak() {
					flush()
				}
			} else {
				flush()
			}
			return ast.WalkContinue, nil
		}
		flush()
		switch n.Kind() {
		case ast.KindString, ast.KindCodeSpan, ast.KindAutoLink, ast.KindRawHTML, ast.KindHTMLBlock, ast.KindThematicBreak:
			return ast.WalkContinue, nil
		}
		if n.Kind() == ast.KindTextBlock {
			return ast.WalkContinue, nil
		}
		tag := n.Kind().String()
		switch t := n.(type) {
		case *ast.Heading:
			tag = fmt.Sprintf("Heading(%d)", t.Level)
			if enter {
				start := lineStart(source, r.Start)
				for start < r.Start && (source[start] == ' ' || source[start] == '\t') {
					start++
				}
				end := lineEnd(source, r.End)
				if r.End > 0 && source[r.End-1] == '\n' {
					end = r.End
				}
				if !strings.HasPrefix(source[start:], "#") {
					end = lineEnd(source, end)
				}
				result.Headings = append(result.Headings, heading{Level: t.Level, Title: headingTitle(n, data), Parent: -1, Range: span{start, end}, End: len(source)})
			}
		case *ast.Emphasis:
			tag = fmt.Sprintf("Emphasis(%d)", t.Level)
		case *ast.Link:
			tag = fmt.Sprintf("Link(%q,%q)", t.Destination, t.Title)
		case *ast.Image:
			tag = fmt.Sprintf("Image(%q,%q)", t.Destination, t.Title)
		case *ast.List:
			tag = fmt.Sprintf("List(%d)", t.Start)
		case *ast.FencedCodeBlock:
			if t.Info != nil {
				tag += "(" + string(t.Info.Value(data)) + ")"
			}
		}
		event := "end " + tag
		if enter {
			event = "start " + tag
		}
		structureBytes += len(event)
		if structureBytes > maxMatchWork {
			return ast.WalkStop, fmt.Errorf("Markdown structure exceeds bounded work limit; use a Git patch")
		}
		result.Structure = append(result.Structure, event)
		result.StructureRanges = append(result.StructureRanges, r)
		return ast.WalkContinue, nil
	})
	if err != nil {
		return nil, err
	}
	flush()
	title := len(result.Headings) > 0 && result.Headings[0].Level == 1
	count := 0
	for _, h := range result.Headings {
		if h.Level == 1 {
			count++
		}
	}
	title = title && count == 1
	stack := []int{}
	for i := range result.Headings {
		h := &result.Headings[i]
		for len(stack) > 0 && result.Headings[stack[len(stack)-1]].Level >= h.Level {
			j := stack[len(stack)-1]
			stack = stack[:len(stack)-1]
			result.Headings[j].End = h.Range.Start
		}
		path := []string{}
		if len(stack) > 0 {
			h.Parent = stack[len(stack)-1]
			path = append(path, result.Headings[h.Parent].Path...)
		}
		if !(title && i == 0) {
			path = append(path, h.Title)
		}
		h.Path = path
		stack = append(stack, i)
	}
	return result, nil
}
func scopeString(path []string) string {
	quoted := make([]string, len(path))
	for i, part := range path {
		var b strings.Builder
		b.WriteByte('"')
		for _, c := range part {
			switch c {
			case 0:
				b.WriteString(`\0`)
			case '\t':
				b.WriteString(`\t`)
			case '\n':
				b.WriteString(`\n`)
			case '\r':
				b.WriteString(`\r`)
			case '\\', '"':
				b.WriteByte('\\')
				b.WriteRune(c)
			default:
				if !strconv.IsPrint(c) || unicode.Is(unicode.Mn, c) || unicode.Is(unicode.Me, c) || unicode.Is(unicode.Other_Grapheme_Extend, c) {
					fmt.Fprintf(&b, "\\u{%x}", c)
				} else {
					b.WriteRune(c)
				}
			}
		}
		b.WriteByte('"')
		quoted[i] = b.String()
	}
	return "[" + strings.Join(quoted, ", ") + "]"
}
func resolve(outline *scanResult, path []string, source string) (int, error) {
	normalized := make([]string, len(path))
	for i, p := range path {
		normalized[i] = normalize(p)
	}
	found := []int{}
	for i, h := range outline.Headings {
		if reflect.DeepEqual(h.Path, normalized) {
			found = append(found, i)
		}
	}
	if len(found) > 1 {
		locations := []string{}
		for _, i := range found {
			locations = append(locations, location(source, outline.Headings[i].Range.Start))
		}
		return -1, fmt.Errorf("ambiguous heading path %s: expected 1 heading, actual %d, candidates [%s]", scopeString(normalized), len(found), strings.Join(locations, ", "))
	}
	if len(found) == 0 {
		return -1, nil
	}
	return found[0], nil
}

type windowMapping struct{ Normal, Original span }

func window(source string, r span) (string, []windowMapping) {
	var b strings.Builder
	mapping := []windowMapping{}
	lastSpace := false
	for i, c := range source[r.Start:r.End] {
		start := r.Start + i
		end := start + utf8.RuneLen(c)
		if unicode.IsSpace(c) {
			if lastSpace {
				mapping[len(mapping)-1].Original.End = end
				continue
			}
			n := b.Len()
			b.WriteByte(' ')
			mapping = append(mapping, windowMapping{span{n, b.Len()}, span{start, end}})
			lastSpace = true
		} else {
			n := b.Len()
			b.WriteRune(c)
			mapping = append(mapping, windowMapping{span{n, b.Len()}, span{start, end}})
			lastSpace = false
		}
	}
	return b.String(), mapping
}
func matches(source string, outline *scanResult, section []string, find string, wholeWords bool) ([]span, error) {
	scope := span{0, len(source)}
	if len(section) > 0 {
		i, err := resolve(outline, section, source)
		if err != nil {
			return nil, err
		}
		if i < 0 {
			return []span{}, nil
		}
		h := outline.Headings[i]
		scope = span{h.Range.End, h.End}
	}
	find = normalize(find)
	found := []span{}
	work := 0
	for _, run := range outline.Runs {
		if run.Start < scope.Start || run.End > scope.End {
			continue
		}
		text, mapping := window(source, run)
		boundaries := map[int]bool{}
		if wholeWords {
			tokens := words.FromString(text)
			at := 0
			boundaries[0] = true
			for tokens.Next() {
				at += len(tokens.Value())
				boundaries[at] = true
			}
		}
		cursor := 0
		for cursor <= len(text) {
			relative := strings.Index(text[cursor:], find)
			if relative < 0 {
				break
			}
			start := cursor + relative
			end := start + len(find)
			_, size := utf8.DecodeRuneInString(text[start:])
			cursor = start + size
			work += len(find)
			if work > maxMatchWork {
				return nil, fmt.Errorf("matching exceeds bounded work limit; use a Git patch")
			}
			if wholeWords && !(boundaries[start] && boundaries[end]) {
				continue
			}
			aIndex := sort.Search(len(mapping), func(i int) bool { return mapping[i].Normal.Start >= start })
			bIndex := sort.Search(len(mapping), func(i int) bool { return mapping[i].Normal.End >= end })
			if aIndex == len(mapping) || bIndex == len(mapping) || mapping[aIndex].Normal.Start != start || mapping[bIndex].Normal.End != end {
				return nil, fmt.Errorf("invalid match mapping")
			}
			a, b := mapping[aIndex].Original.Start, mapping[bIndex].Original.End
			if normalize(source[a:b]) != find {
				return nil, fmt.Errorf("unsafe normalized source mapping")
			}
			found = append(found, span{a, b})
		}
	}
	return found, nil
}

type documentSkeleton struct{ Structure, Protected []string }

func skeleton(source string, outline *scanResult) documentSkeleton {
	result := []string{}
	previous := 0
	for _, r := range outline.Runs {
		result = append(result, source[previous:r.Start])
		previous = r.End
	}
	result = append(result, source[previous:])
	return documentSkeleton{outline.Structure, result}
}
func location(source string, offset int) string {
	return fmt.Sprintf("%d:%d", strings.Count(source[:offset], "\n")+1, utf8.RuneCountInString(source[lineStart(source, offset):offset])+1)
}
func ruleContext(index int, scope []string, expected string, found []span, source string) string {
	candidates := []string{}
	for _, r := range found {
		candidates = append(candidates, location(source, r.Start))
	}
	return fmt.Sprintf("operation %d scope %s expected %s, actual %d, candidates [%s]", index+1, scopeString(scope), expected, len(found), strings.Join(candidates, ", "))
}
func newline(source string) (string, error) {
	crlf := strings.Count(source, "\r\n")
	lf := strings.Count(source, "\n")
	if strings.Count(source, "\r") != crlf || crlf > 0 && crlf != lf {
		return "", fmt.Errorf("mixed newline styles are unsupported; use a Git patch")
	}
	if crlf > 0 {
		return "\r\n", nil
	}
	return "\n", nil
}
