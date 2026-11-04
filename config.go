package main

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"

	"github.com/pelletier/go-toml/v2"
)

type Manifest struct {
	Version  uint32            `toml:"version" json:"version"`
	Prefix   *string           `toml:"prefix" json:"prefix,omitempty"`
	Targets  []string          `toml:"targets" json:"targets"`
	Commands []string          `toml:"commands" json:"commands,omitempty"`
	Sources  map[string]Source `toml:"sources" json:"sources"`
}
type Source struct {
	Repo      *string           `toml:"repo" json:"repo"`
	Path      *string           `toml:"path" json:"path"`
	Reference *string           `toml:"ref" json:"ref"`
	Skills    []string          `toml:"skills" json:"skills"`
	Rename    map[string]string `toml:"rename" json:"rename"`
	Patches   []string          `toml:"patches" json:"patches"`
}
type Loaded struct {
	ManifestPath       string
	ManifestBytes      []byte
	Manifest           Manifest
	Base, Prefix, Lock string
	Targets            []string
}

func strictTOML(data []byte, out any) error {
	if !utf8.Valid(data) {
		return fmt.Errorf("invalid UTF-8")
	}
	return toml.NewDecoder(bytes.NewReader(data)).DisallowUnknownFields().Decode(out)
}
func loadConfig(path string) (*Loaded, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return nil, err
	}
	abs, err = filepath.EvalSymlinks(abs)
	if err != nil {
		return nil, fmt.Errorf("reading %s; run `skills-sync init` first: %w", path, err)
	}
	data, err := os.ReadFile(abs)
	if err != nil {
		return nil, err
	}
	var m Manifest
	if err := strictTOML(data, &m); err != nil {
		return nil, fmt.Errorf("invalid manifest: %w", err)
	}
	if m.Version != 1 {
		return nil, fmt.Errorf("unsupported manifest version %d", m.Version)
	}
	if len(m.Targets) == 0 {
		return nil, fmt.Errorf("at least one target is required")
	}
	for i, command := range m.Commands {
		if strings.TrimSpace(command) == "" || strings.ContainsRune(command, '\x00') {
			return nil, fmt.Errorf("command %d must not be blank or contain NUL", i+1)
		}
	}
	if m.Sources == nil {
		var raw map[string]any
		if err := toml.Unmarshal(data, &raw); err != nil {
			return nil, err
		}
		if _, ok := raw["sources"]; !ok {
			return nil, fmt.Errorf("invalid manifest: missing field sources")
		}
		m.Sources = map[string]Source{}
	}
	c := &Loaded{ManifestPath: abs, ManifestBytes: data, Manifest: m, Base: filepath.Dir(abs)}
	if m.Prefix != nil {
		c.Prefix, err = resolvedPath(c.Base, *m.Prefix)
	} else {
		root := os.Getenv("XDG_STATE_HOME")
		if !filepath.IsAbs(root) {
			home := os.Getenv("HOME")
			if !filepath.IsAbs(home) {
				return nil, fmt.Errorf("HOME must be a nonempty absolute path when XDG_STATE_HOME is unset, empty, or relative")
			}
			root = filepath.Join(home, ".local/state")
		}
		c.Prefix, err = resolvedPath(c.Base, filepath.Join(root, "skills-sync", hash([]byte(abs))))
	}
	if err != nil {
		return nil, err
	}
	for _, target := range m.Targets {
		resolved, err := resolvedPath(c.Base, target)
		if err != nil {
			return nil, err
		}
		c.Targets = append(c.Targets, resolved)
	}
	for i, target := range c.Targets {
		if overlaps(target, c.Prefix) {
			return nil, fmt.Errorf("prefix and targets must not overlap")
		}
		for j, other := range c.Targets {
			if i == j {
				continue
			}
			if target == other {
				return nil, fmt.Errorf("duplicate target %s", target)
			}
			if overlaps(target, other) {
				return nil, fmt.Errorf("targets must not overlap")
			}
		}
	}
	for _, id := range sortedKeys(m.Sources) {
		s := m.Sources[id]
		if err := validName(id); err != nil {
			return nil, err
		}
		if (s.Repo == nil) == (s.Path == nil) {
			return nil, fmt.Errorf("source %s: specify exactly one of repo or path", id)
		}
		if s.Path != nil && s.Reference != nil {
			return nil, fmt.Errorf("source %s: ref requires repo", id)
		}
		if s.Skills == nil {
			s.Skills = []string{"*"}
		}
		if len(s.Skills) == 0 {
			return nil, fmt.Errorf("source %s: skills cannot be empty; remove the source to uninstall it", id)
		}
		for _, value := range []*string{s.Repo, s.Reference} {
			if value != nil && (*value == "" || strings.HasPrefix(*value, "-")) {
				return nil, fmt.Errorf("source %s: invalid repo or ref", id)
			}
		}
		if s.Rename == nil {
			s.Rename = map[string]string{}
		}
		if s.Patches == nil {
			s.Patches = []string{}
		}
		for _, name := range s.Rename {
			if err := validName(name); err != nil {
				return nil, err
			}
		}
		c.Manifest.Sources[id] = s
	}
	extension := filepath.Ext(abs)
	if filepath.Base(abs) == extension {
		extension = ""
	}
	c.Lock = strings.TrimSuffix(abs, extension) + ".lock"
	if c.Lock == abs {
		return nil, fmt.Errorf("manifest cannot use the .lock extension; it would collide with its lockfile")
	}
	for _, name := range []string{"state.json", "sync.lock", "transaction.json"} {
		reserved := filepath.Join(c.Prefix, name)
		if abs == reserved || c.Lock == reserved {
			return nil, fmt.Errorf("manifest or lockfile collides with reserved prefix file %s", name)
		}
	}
	return c, nil
}
func (c *Loaded) repo(source Source) (string, error) {
	if strings.Contains(*source.Repo, ":") {
		return *source.Repo, nil
	}
	return absolute(c.Base, *source.Repo)
}
