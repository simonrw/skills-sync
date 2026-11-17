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
	Layers             []Layer
	Manifest           Manifest
	Base, Prefix, Lock string
	Targets            []string
}

// Layer is one manifest file merged into the effective manifest.
type Layer struct {
	Path string
	Data []byte
}

func strictTOML(data []byte, out any) error {
	if !utf8.Valid(data) {
		return fmt.Errorf("invalid UTF-8")
	}
	return toml.NewDecoder(bytes.NewReader(data)).DisallowUnknownFields().Decode(out)
}
func loadConfig(path string) (*Loaded, error) {
	abs, layers, err := manifestLayers(path)
	if err != nil {
		return nil, err
	}
	base := filepath.Dir(abs)

	var m Manifest
	sourcesDeclared := false
	for _, layer := range layers {
		var next Manifest
		if err := strictTOML(layer.Data, &next); err != nil {
			return nil, fmt.Errorf("invalid manifest %s: %w", layer.Path, err)
		}
		var raw map[string]any
		if err := toml.Unmarshal(layer.Data, &raw); err != nil {
			return nil, err
		}
		if _, ok := raw["sources"]; ok {
			sourcesDeclared = true
		}

		if dir := filepath.Dir(layer.Path); dir != base {
			next.rebase(dir)
		}
		m.override(next)
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
	if !sourcesDeclared {
		return nil, fmt.Errorf("invalid manifest: missing field sources")
	}
	if m.Sources == nil {
		m.Sources = map[string]Source{}
	}
	c := &Loaded{ManifestPath: abs, Layers: layers, Manifest: m, Base: base}
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
		for _, path := range c.inputFiles() {
			if path == reserved {
				return nil, fmt.Errorf("manifest or lockfile collides with reserved prefix file %s", name)
			}
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

// inputFiles returns every manifest layer and the lockfile.
func (c *Loaded) inputFiles() []string {
	paths := []string{c.ManifestPath, c.Lock}
	for _, layer := range c.Layers {
		paths = append(paths, layer.Path)
	}
	return paths
}

// manifestLayers finds the existing global, project, and project-local
// manifests for path, lowest precedence first. It also returns the canonical
// manifest path that owns the lockfile and default prefix: the project
// manifest when any project file exists, otherwise the global manifest.
func manifestLayers(path string) (string, []Layer, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", nil, err
	}
	extension := filepath.Ext(abs)
	if filepath.Base(abs) == extension {
		extension = ""
	}
	local := strings.TrimSuffix(abs, extension) + ".local" + extension

	var layers []Layer
	seen := map[string]bool{}
	for _, candidate := range []string{globalManifest(), abs, local} {
		if candidate == "" || !exists(candidate) {
			continue
		}
		canonical, err := filepath.EvalSymlinks(candidate)
		if err != nil {
			return "", nil, err
		}
		if seen[canonical] {
			continue
		}
		seen[canonical] = true

		data, err := os.ReadFile(canonical)
		if err != nil {
			return "", nil, err
		}
		layers = append(layers, Layer{canonical, data})
	}

	switch {
	case exists(abs):
		primary, err := filepath.EvalSymlinks(abs)
		return primary, layers, err
	case exists(local):
		dir, err := filepath.EvalSymlinks(filepath.Dir(abs))
		return filepath.Join(dir, filepath.Base(abs)), layers, err
	case len(layers) > 0:
		return layers[0].Path, layers, nil
	}
	_, err = os.Stat(abs)
	return "", nil, fmt.Errorf("reading %s; run `skills-sync init` first: %w", path, err)
}

// globalManifest returns the path of the user-wide manifest, or "" when no
// configuration directory can be determined.
func globalManifest() string {
	root := os.Getenv("XDG_CONFIG_HOME")
	if !filepath.IsAbs(root) {
		home := os.Getenv("HOME")
		if !filepath.IsAbs(home) {
			return ""
		}
		root = filepath.Join(home, ".config")
	}
	return filepath.Join(root, "skills-sync", "skills.toml")
}

// override applies the settings declared in next on top of m. Top-level
// values replace earlier ones, and each source table replaces any earlier
// source with the same ID.
func (m *Manifest) override(next Manifest) {
	if next.Version != 0 {
		m.Version = next.Version
	}
	if next.Prefix != nil {
		m.Prefix = next.Prefix
	}
	if next.Targets != nil {
		m.Targets = next.Targets
	}
	if next.Commands != nil {
		m.Commands = next.Commands
	}
	if next.Sources != nil {
		if m.Sources == nil {
			m.Sources = map[string]Source{}
		}
		for id, source := range next.Sources {
			m.Sources[id] = source
		}
	}
}

// rebase makes the relative paths in m absolute against dir.
func (m *Manifest) rebase(dir string) {
	// Layers outside the project directory must keep resolving paths from
	// the directory they were written in.
	if m.Prefix != nil {
		prefix := rebased(dir, *m.Prefix)
		m.Prefix = &prefix
	}
	for i, target := range m.Targets {
		m.Targets[i] = rebased(dir, target)
	}
	for id, s := range m.Sources {
		if s.Path != nil {
			path := rebased(dir, *s.Path)
			s.Path = &path
		}
		if s.Repo != nil && !strings.Contains(*s.Repo, ":") {
			repo := rebased(dir, *s.Repo)
			s.Repo = &repo
		}
		if s.Patches != nil {
			patches := make([]string, len(s.Patches))
			for i, patch := range s.Patches {
				patches[i] = rebased(dir, patch)
			}
			s.Patches = patches
		}
		m.Sources[id] = s
	}
}

func rebased(dir, path string) string {
	if path == "" || filepath.IsAbs(path) || path == "~" || strings.HasPrefix(path, "~/") {
		return path
	}
	return filepath.Join(dir, path)
}
