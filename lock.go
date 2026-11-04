package main

import (
	"fmt"
	"os"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

type Patch struct {
	Path   string `toml:"path" json:"path"`
	SHA256 string `toml:"sha256" json:"sha256"`
}
type LockedSource struct {
	Kind       string            `toml:"kind" json:"kind"`
	Upstream   string            `toml:"upstream" json:"upstream"`
	Reference  *string           `toml:"reference" json:"reference"`
	Revision   string            `toml:"revision" json:"revision"`
	Patches    []Patch           `toml:"patches" json:"patches"`
	Skills     map[string]string `toml:"skills" json:"skills"`
	TreeSHA256 string            `toml:"tree_sha256" json:"tree_sha256"`
}
type LockFile struct {
	Version        uint32                  `toml:"version" json:"version"`
	ManifestSHA256 string                  `toml:"manifest_sha256" json:"manifest_sha256"`
	Sources        map[string]LockedSource `toml:"sources" json:"sources"`
}
type PatchInput struct {
	Info  Patch
	Bytes []byte
}
type PatchInputs map[string][]PatchInput

func loadPatchInputs(c *Loaded) (PatchInputs, error) {
	inputs := PatchInputs{}
	for _, id := range sortedKeys(c.Manifest.Sources) {
		stack := []PatchInput{}
		for _, path := range c.Manifest.Sources[id].Patches {
			full, err := absolute(c.Base, path)
			if err != nil {
				return nil, err
			}
			var data []byte
			if strings.HasSuffix(path, ".skillpatch.toml") {
				data, err = readBounded(full)
			} else {
				data, err = os.ReadFile(full)
			}
			if err != nil {
				return nil, fmt.Errorf("reading patch %s: %w", full, err)
			}
			stack = append(stack, PatchInput{Patch{path, hash(data)}, data})
		}
		inputs[id] = stack
	}
	return inputs, nil
}
func fingerprint(c *Loaded, inputs PatchInputs) (string, error) {
	hashes := map[string][]Patch{}
	for id, stack := range inputs {
		hashes[id] = []Patch{}
		for _, p := range stack {
			hashes[id] = append(hashes[id], p.Info)
		}
	}
	return jsonHash([]any{c.Manifest, hashes})
}
func readLock(c *Loaded) (*LockFile, error) {
	data, err := readOptional(c.Lock)
	if err != nil || data == nil {
		return nil, err
	}
	var lock LockFile
	if err := toml.Unmarshal([]byte(*data), &lock); err != nil {
		return nil, fmt.Errorf("invalid lockfile: %w", err)
	}
	if lock.Version != 1 {
		return nil, fmt.Errorf("unsupported lockfile version")
	}
	var raw map[string]any
	if err := toml.Unmarshal([]byte(*data), &raw); err != nil {
		return nil, err
	}
	if raw["manifest_sha256"] == nil || raw["sources"] == nil {
		return nil, fmt.Errorf("invalid lockfile: missing fields")
	}
	if lock.Sources == nil {
		lock.Sources = map[string]LockedSource{}
	}
	sources, ok := raw["sources"].(map[string]any)
	if !ok {
		return nil, fmt.Errorf("invalid lockfile sources")
	}
	for id, value := range sources {
		source, ok := value.(map[string]any)
		if !ok {
			return nil, fmt.Errorf("invalid locked source %s", id)
		}
		for _, field := range []string{"kind", "upstream", "revision", "patches", "skills", "tree_sha256"} {
			if source[field] == nil {
				return nil, fmt.Errorf("invalid lockfile: source %s missing field %s", id, field)
			}
		}
		if patches, ok := source["patches"].([]any); ok {
			for _, value := range patches {
				patch, ok := value.(map[string]any)
				if !ok || patch["path"] == nil || patch["sha256"] == nil {
					return nil, fmt.Errorf("invalid lockfile patch")
				}
			}
		}
		s := lock.Sources[id]
		if s.Skills == nil {
			s.Skills = map[string]string{}
		}
		lock.Sources[id] = s
	}
	return &lock, nil
}
func requireCurrent(c *Loaded, inputs PatchInputs) (*LockFile, error) {
	lock, err := readLock(c)
	if err != nil {
		return nil, err
	}
	if lock == nil {
		return nil, fmt.Errorf("lockfile is missing; run sync first")
	}
	fp, err := fingerprint(c, inputs)
	if err != nil {
		return nil, err
	}
	if fp != lock.ManifestSHA256 || strings.Join(sortedKeys(lock.Sources), "\x00") != strings.Join(sortedKeys(c.Manifest.Sources), "\x00") {
		return nil, fmt.Errorf("manifest/patches changed or source membership differs from lockfile; run sync and review the lockfile")
	}
	return lock, nil
}
func tomlKey(s string) string {
	if s != "" {
		good := true
		for _, r := range s {
			if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '_' || r == '-') {
				good = false
			}
		}
		if good {
			return s
		}
	}
	return encodeTOMLString(s, true)
}
func tomlString(s string) string {
	return encodeTOMLString(s, false)
}

func encodeTOMLString(s string, key bool) string {
	control := false
	for _, c := range s {
		if (c < 32 && c != '\t' && (c != '\n' || key)) || c == 127 {
			control = true
		}
	}
	newline := strings.Contains(s, "\n")
	escape := strings.Contains(s, "\\")
	delimiter, literal := "\"", false
	switch {
	case !control && !escape && !strings.Contains(s, "\"") && (!newline || key):
	case !control && !strings.Contains(s, "'") && (!newline || key):
		delimiter, literal = "'", true
	case !key && !control && !escape && !strings.Contains(s, "\"\"\""):
		delimiter = "\"\"\""
	case !key && !control && !strings.Contains(s, "'''"):
		delimiter, literal = "'''", true
	case !key && newline:
		delimiter = "\"\"\""
	}
	multiline := len(delimiter) == 3
	var b strings.Builder
	b.WriteString(delimiter)
	if multiline && newline {
		b.WriteByte('\n')
	}
	if literal {
		b.WriteString(s)
	} else {
		quotes := 0
		for _, c := range s {
			if c == '"' {
				quotes++
			} else {
				quotes = 0
			}
			switch {
			case c == '"' && (!multiline || quotes > 2):
				b.WriteString(`\"`)
				quotes = 0
			case c == '\\':
				b.WriteString(`\\`)
				quotes = 0
			case c == '\b':
				b.WriteString(`\b`)
			case c == '\t':
				b.WriteString(`\t`)
			case c == '\n' && !multiline:
				b.WriteString(`\n`)
			case c == '\r':
				b.WriteString(`\r`)
			case c == '\f':
				b.WriteString(`\f`)
			case (c < 32 && c != '\n') || c == 127:
				fmt.Fprintf(&b, "\\u%04X", c)
			default:
				b.WriteRune(c)
			}
		}
	}
	b.WriteString(delimiter)
	return b.String()
}
func serializeLock(lock *LockFile) []byte {
	var b strings.Builder
	fmt.Fprintf(&b, "version = %d\nmanifest_sha256 = %s\n", lock.Version, tomlString(lock.ManifestSHA256))
	if len(lock.Sources) == 0 {
		b.WriteString("\n[sources]\n")
	}
	for _, id := range sortedKeys(lock.Sources) {
		s := lock.Sources[id]
		table := "sources." + tomlKey(id)
		fmt.Fprintf(&b, "\n[%s]\nkind = %s\nupstream = %s\n", table, tomlString(s.Kind), tomlString(s.Upstream))
		if s.Reference != nil {
			fmt.Fprintf(&b, "reference = %s\n", tomlString(*s.Reference))
		}
		fmt.Fprintf(&b, "revision = %s\n", tomlString(s.Revision))
		if len(s.Patches) == 0 {
			b.WriteString("patches = []\n")
		}
		fmt.Fprintf(&b, "tree_sha256 = %s\n", tomlString(s.TreeSHA256))
		for _, p := range s.Patches {
			fmt.Fprintf(&b, "\n[[%s.patches]]\npath = %s\nsha256 = %s\n", table, tomlString(p.Path), tomlString(p.SHA256))
		}
		fmt.Fprintf(&b, "\n[%s.skills]\n", table)
		for _, path := range sortedKeys(s.Skills) {
			fmt.Fprintf(&b, "%s = %s\n", tomlKey(path), tomlString(s.Skills[path]))
		}
	}
	return []byte(b.String())
}
