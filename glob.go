package main

import (
	"fmt"
	"regexp"
	"strings"
)

type globToken struct {
	kind, text   string
	alternatives [][]globToken
}
type globMatcher struct{ expression *regexp.Regexp }

// Globset matches UTF-8 paths as bytes, including for '?' and character classes.
func byteString(value string) string {
	var b strings.Builder
	for _, c := range []byte(value) {
		b.WriteRune(rune(c))
	}
	return b.String()
}
func (m *globMatcher) MatchString(path string) bool {
	return m.expression.MatchString(byteString(path))
}
func globPattern(pattern string) (*globMatcher, error) {
	chars := []rune(pattern)
	at := 0
	var parse func(bool) ([][]globToken, error)
	parse = func(group bool) ([][]globToken, error) {
		branches := [][]globToken{{}}
		for at < len(chars) {
			i := at
			c := chars[at]
			at++
			tokens := &branches[len(branches)-1]
			push := func(kind, text string) { *tokens = append(*tokens, globToken{kind: kind, text: text}) }
			switch c {
			case '{':
				alternatives, err := parse(true)
				if err != nil {
					return nil, err
				}
				*tokens = append(*tokens, globToken{kind: "alternatives", alternatives: alternatives})
			case '}':
				if !group {
					return nil, fmt.Errorf("unopened alternates")
				}
				return branches, nil
			case ',':
				if group {
					branches = append(branches, []globToken{})
				} else {
					push("literal", ",")
				}
			case '\\':
				if at == len(chars) {
					return nil, fmt.Errorf("dangling escape")
				}
				push("literal", string(chars[at]))
				at++
			case '?':
				push("any", "")
			case '*':
				if at == len(chars) || chars[at] != '*' {
					push("star", "")
					continue
				}
				at++
				if len(*tokens) == 0 {
					if at == len(chars) || chars[at] == '/' {
						push("prefix", "")
						if at < len(chars) {
							at++
						}
					} else {
						push("star", "")
						push("star", "")
					}
					continue
				}
				previous := rune(0)
				if i > 0 {
					previous = chars[i-1]
				}
				if previous != '/' && !(group && (previous == ',' || previous == '{')) {
					push("star", "")
					push("star", "")
					continue
				}
				suffix := at == len(chars) || group && (chars[at] == ',' || chars[at] == '}')
				if !suffix && (at == len(chars) || chars[at] != '/') {
					push("star", "")
					push("star", "")
					continue
				}
				if !suffix {
					at++
				}
				last := (*tokens)[len(*tokens)-1]
				*tokens = (*tokens)[:len(*tokens)-1]
				if last.kind == "prefix" || last.kind == "suffix" {
					*tokens = append(*tokens, last)
				} else if suffix {
					push("suffix", "")
				} else {
					push("recursive", "")
				}
			case '[':
				negated := false
				if at < len(chars) && (chars[at] == '!' || chars[at] == '^') {
					negated = true
					at++
				}
				ranges := [][2]rune{}
				first, inRange, closed := true, false, false
				for at < len(chars) {
					c := chars[at]
					at++
					if c == ']' && !first {
						closed = true
						break
					}
					if c == '-' && !first && !inRange {
						inRange = true
					} else if inRange {
						if c < ranges[len(ranges)-1][0] {
							return nil, fmt.Errorf("invalid character range")
						}
						ranges[len(ranges)-1][1] = c
						inRange = false
					} else {
						ranges = append(ranges, [2]rune{c, c})
					}
					first = false
				}
				if !closed {
					return nil, fmt.Errorf("unclosed character class")
				}
				if inRange {
					ranges = append(ranges, [2]rune{'-', '-'})
				}
				var class strings.Builder
				class.WriteByte('[')
				if negated {
					class.WriteByte('^')
				}
				for _, r := range ranges {
					escape := func(c rune) string {
						var s strings.Builder
						for _, b := range []byte(string(c)) {
							fmt.Fprintf(&s, `\x{%X}`, b)
						}
						return s.String()
					}
					class.WriteString(escape(r[0]))
					if r[0] != r[1] {
						class.WriteByte('-')
						class.WriteString(escape(r[1]))
					}
				}
				class.WriteByte(']')
				push("class", class.String())
			default:
				push("literal", string(c))
			}
		}
		if group {
			return nil, fmt.Errorf("unclosed alternates")
		}
		return branches, nil
	}
	branches, err := parse(false)
	if err != nil {
		return nil, err
	}
	var render func([]globToken) string
	render = func(tokens []globToken) string {
		var b strings.Builder
		for _, t := range tokens {
			switch t.kind {
			case "literal":
				b.WriteString(regexp.QuoteMeta(byteString(t.text)))
			case "any":
				b.WriteString("[^/]")
			case "star":
				b.WriteString("[^/]*")
			case "prefix":
				b.WriteString("(?:/?|.*/)")
			case "suffix":
				b.WriteString("/.*")
			case "recursive":
				b.WriteString("(?:/|/.*/)")
			case "class":
				b.WriteString(t.text)
			case "alternatives":
				parts := []string{}
				for _, alt := range t.alternatives {
					if s := render(alt); s != "" {
						parts = append(parts, s)
					}
				}
				if len(parts) > 0 {
					b.WriteString("(?:" + strings.Join(parts, "|") + ")")
				}
			}
		}
		return b.String()
	}
	expression := render(branches[0])
	if len(branches[0]) == 1 && branches[0][0].kind == "prefix" {
		expression = ".*"
	}
	compiled, err := regexp.Compile("(?s)^(?:" + expression + ")$")
	if err != nil {
		return nil, err
	}
	return &globMatcher{compiled}, nil
}
