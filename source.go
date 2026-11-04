package main

import (
	"archive/tar"
	"bytes"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"unicode/utf8"
)

type Prepared struct {
	Locked  LockedSource
	Tree    string
	Reports []PatchReport
}

func entries(root string, visit func(string, fs.DirEntry) error) error {
	return filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !utf8.ValidString(path) {
			return fmt.Errorf("paths must be valid UTF-8")
		}
		if path != root && entry.Name() == ".git" {
			if entry.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		return visit(path, entry)
	})
}
func treeHash(root string) (string, error) {
	records := map[string]any{}
	err := entries(root, func(path string, e fs.DirEntry) error {
		if path == root || e.IsDir() {
			return nil
		}
		name, err := filepath.Rel(root, path)
		if err != nil {
			return err
		}
		meta, err := e.Info()
		if err != nil {
			return err
		}
		if meta.Mode()&os.ModeSymlink != 0 {
			target, err := os.Readlink(path)
			if err != nil {
				return err
			}
			if !utf8.ValidString(target) {
				return fmt.Errorf("paths must be valid UTF-8")
			}
			records[name] = []any{"link", target, false}
		} else if meta.Mode().IsRegular() {
			data, err := os.ReadFile(path)
			if err != nil {
				return err
			}
			records[name] = []any{"file", hash(data), meta.Mode().Perm()&0111 != 0}
		} else {
			return fmt.Errorf("unsupported file type at %s", path)
		}
		return nil
	})
	if err != nil {
		return "", err
	}
	return jsonHash(records)
}
func validateLinks(root string) error {
	canonical, err := filepath.EvalSymlinks(root)
	if err != nil {
		return err
	}
	return entries(canonical, func(path string, e fs.DirEntry) error {
		if e.Type()&os.ModeSymlink == 0 {
			return nil
		}
		target, err := os.Readlink(path)
		if err != nil {
			return err
		}
		if filepath.IsAbs(target) {
			return fmt.Errorf("absolute symlink in source: %s", path)
		}
		resolved, err := filepath.EvalSymlinks(path)
		if err != nil {
			return fmt.Errorf("dangling or cyclic symlink: %s: %w", path, err)
		}
		if !within(resolved, canonical) {
			return fmt.Errorf("symlink escapes source: %s", path)
		}
		return nil
	})
}
func copyTree(from, to string) error {
	err := entries(from, func(path string, e fs.DirEntry) error {
		if path == from {
			return nil
		}
		rel, _ := filepath.Rel(from, path)
		target := filepath.Join(to, rel)
		meta, err := e.Info()
		if err != nil {
			return err
		}
		switch {
		case meta.IsDir():
			return os.MkdirAll(target, 0777)
		case meta.Mode().IsRegular():
			return copyFile(path, target, meta.Mode())
		case meta.Mode()&os.ModeSymlink != 0:
			link, err := os.Readlink(path)
			if err != nil {
				return err
			}
			return os.Symlink(link, target)
		default:
			return fmt.Errorf("unsupported file type at %s", path)
		}
	})
	if err != nil {
		return err
	}
	return validateLinks(to)
}
func unpack(data []byte, root string) error {
	reader := tar.NewReader(bytes.NewReader(data))
	for {
		h, err := reader.Next()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}
		if h.Typeflag == tar.TypeXGlobalHeader {
			continue
		}
		if filepath.IsAbs(h.Name) || !utf8.ValidString(h.Name) {
			return fmt.Errorf("unsafe path in Git archive: %s", h.Name)
		}
		for _, p := range strings.Split(h.Name, "/") {
			if p == ".." || p == ".git" {
				return fmt.Errorf("unsafe path in Git archive: %s", h.Name)
			}
		}
		target := filepath.Join(root, h.Name)
		parent, err := resolvedPath(root, filepath.Dir(target))
		if err != nil {
			return err
		}
		if !within(parent, root) {
			return fmt.Errorf("archive entry escapes snapshot")
		}
		if err := os.MkdirAll(filepath.Dir(target), 0777); err != nil {
			return err
		}
		switch h.Typeflag {
		case tar.TypeDir:
			if err := os.MkdirAll(target, os.FileMode(h.Mode)&0777); err != nil {
				return err
			}
		case tar.TypeReg, tar.TypeRegA:
			if info, err := os.Lstat(target); err == nil && info.Mode()&os.ModeSymlink != 0 {
				return fmt.Errorf("archive entry escapes snapshot")
			}
			f, err := os.OpenFile(target, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, os.FileMode(h.Mode)&0777)
			if err != nil {
				return err
			}
			_, err = io.Copy(f, reader)
			closeErr := f.Close()
			if err != nil {
				return err
			}
			if closeErr != nil {
				return closeErr
			}
		case tar.TypeSymlink:
			resolved, err := absolute(filepath.Dir(target), h.Linkname)
			if err != nil {
				return err
			}
			if filepath.IsAbs(h.Linkname) || !within(resolved, root) {
				return fmt.Errorf("symlink escapes archive: %s", h.Name)
			}
			if err := os.Symlink(h.Linkname, target); err != nil {
				return err
			}
		default:
			return fmt.Errorf("unsupported Git archive entry %s", h.Name)
		}
	}
}
func mirror(c *Loaded, upstream string, offline bool) (string, error) {
	cache := filepath.Join(c.Prefix, "repos")
	if !offline {
		if err := os.MkdirAll(cache, 0777); err != nil {
			return "", err
		}
	}
	path := filepath.Join(cache, hash([]byte(upstream))+".git")
	if !exists(path) {
		if offline {
			return "", fmt.Errorf("repository %s is not cached", upstream)
		}
		temp, err := os.MkdirTemp(cache, ".tmp")
		if err != nil {
			return "", err
		}
		defer os.RemoveAll(temp)
		if _, err := git("", "clone", "--mirror", "--", upstream, filepath.Join(temp, "repo")); err != nil {
			return "", err
		}
		if err := os.Rename(filepath.Join(temp, "repo"), path); err != nil {
			return "", err
		}
	}
	return path, nil
}
func validateRevision(commit string) error {
	if len(commit) == 40 || len(commit) == 64 {
		good := true
		for _, c := range commit {
			if !(c >= '0' && c <= '9' || c >= 'a' && c <= 'f' || c >= 'A' && c <= 'F') {
				good = false
			}
		}
		if good {
			return nil
		}
	}
	return fmt.Errorf("invalid commit hash %q", commit)
}
func prepare(c *Loaded, id string, s Source, old *LockedSource, update, offline bool, patches []PatchInput) (*Prepared, error) {
	store := filepath.Join(c.Prefix, "sources", id)
	if err := os.MkdirAll(store, 0777); err != nil {
		return nil, err
	}
	temp, err := os.MkdirTemp(store, ".tmp")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(temp)
	tree := filepath.Join(temp, "tree")
	if err := os.Mkdir(tree, 0777); err != nil {
		return nil, err
	}
	locked := LockedSource{Reference: s.Reference, Patches: []Patch{}}
	for _, p := range patches {
		locked.Patches = append(locked.Patches, p.Info)
	}
	if s.Path != nil {
		local, err := absolute(c.Base, *s.Path)
		if err != nil {
			return nil, err
		}
		local, err = filepath.EvalSymlinks(local)
		if err != nil {
			return nil, err
		}
		meta, err := os.Stat(local)
		if err != nil {
			return nil, err
		}
		if !meta.IsDir() {
			return nil, fmt.Errorf("local source must be a directory")
		}
		if overlaps(local, c.Prefix) {
			return nil, fmt.Errorf("local source must not overlap prefix or targets")
		}
		for _, target := range c.Targets {
			if overlaps(local, target) {
				return nil, fmt.Errorf("local source must not overlap prefix or targets")
			}
		}
		if err := copyTree(local, tree); err != nil {
			return nil, err
		}
		locked.Kind = "local"
		locked.Upstream = local
		locked.Revision, err = treeHash(tree)
		if err != nil {
			return nil, err
		}
	} else {
		upstream, err := c.repo(s)
		if err != nil {
			return nil, err
		}
		repo, err := mirror(c, upstream, offline)
		if err != nil {
			return nil, err
		}
		locked.Kind = "git"
		locked.Upstream = upstream
		if !update && old != nil && old.Kind == "git" && old.Upstream == upstream && reflect.DeepEqual(old.Reference, s.Reference) {
			locked.Revision = old.Revision
			if err := validateRevision(locked.Revision); err != nil {
				return nil, err
			}
			if _, err := git(repo, "cat-file", "-e", locked.Revision+"^{commit}"); err != nil {
				if offline {
					return nil, fmt.Errorf("commit %s is not cached; retry without --offline", locked.Revision)
				}
				if _, err := git(repo, "fetch", "origin", locked.Revision); err != nil {
					return nil, err
				}
			}
		} else {
			if offline {
				return nil, fmt.Errorf("source %s needs resolution; run sync without --offline first", id)
			}
			if _, err := git(repo, "fetch", "--force", "origin", "+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*", "+HEAD:refs/skills-sync/default"); err != nil {
				return nil, err
			}
			ref := "refs/skills-sync/default"
			if s.Reference != nil {
				ref = *s.Reference
			}
			out, err := git(repo, "rev-parse", "--verify", "--end-of-options", ref+"^{commit}")
			if err != nil {
				return nil, err
			}
			locked.Revision = strings.TrimSpace(string(out))
			if err := validateRevision(locked.Revision); err != nil {
				return nil, err
			}
		}
		listing, err := git(repo, "ls-tree", "-r", locked.Revision)
		if err != nil {
			return nil, err
		}
		for _, line := range strings.Split(string(listing), "\n") {
			if strings.HasPrefix(line, "160000 ") {
				return nil, fmt.Errorf("source %s contains submodules; declare each submodule as a separate source", id)
			}
		}
		archive, err := git(repo, "archive", "--format=tar", locked.Revision)
		if err != nil {
			return nil, err
		}
		if err := unpack(archive, tree); err != nil {
			return nil, err
		}
	}
	if err := validateLinks(tree); err != nil {
		return nil, err
	}
	if _, err := git(tree, "init", "--quiet"); err != nil {
		return nil, err
	}
	reports := []PatchReport{}
	for _, input := range patches {
		report, err := applyFile(tree, input.Info.Path, input.Bytes)
		if err != nil {
			return nil, fmt.Errorf("patch %s does not apply to %s@%s: %w", input.Info.Path, id, locked.Revision, err)
		}
		reports = append(reports, report)
	}
	if err := os.RemoveAll(filepath.Join(tree, ".git")); err != nil {
		return nil, err
	}
	if err := validateLinks(tree); err != nil {
		return nil, err
	}
	locked.Skills, err = selectSkills(tree, s)
	if err != nil {
		return nil, err
	}
	locked.TreeSHA256, err = treeHash(tree)
	if err != nil {
		return nil, err
	}
	generation, err := jsonHash([]any{locked.Kind, locked.Upstream, locked.Revision, locked.Patches, locked.TreeSHA256})
	if err != nil {
		return nil, err
	}
	destination := filepath.Join(store, generation)
	if exists(destination) {
		actual, err := treeHash(filepath.Join(destination, "tree"))
		if err != nil {
			return nil, err
		}
		if actual != locked.TreeSHA256 {
			return nil, fmt.Errorf("cached snapshot %s was modified; remove that snapshot and retry", destination)
		}
	} else {
		origin := map[string]any{"kind": locked.Kind, "upstream": locked.Upstream, "revision": locked.Revision, "patches": locked.Patches, "tree_sha256": locked.TreeSHA256}
		if err := writeJSON(filepath.Join(temp, "source.json"), origin); err != nil {
			return nil, err
		}
		if err := os.Rename(temp, destination); err != nil {
			return nil, err
		}
	}
	return &Prepared{locked, filepath.Join(destination, "tree"), reports}, nil
}
func lockedBaseline(c *Loaded, id string, lock *LockFile, inputs PatchInputs) (*Prepared, error) {
	spec, ok := c.Manifest.Sources[id]
	if !ok {
		return nil, fmt.Errorf("unknown source ID %q", id)
	}
	old, ok := lock.Sources[id]
	if !ok {
		return nil, fmt.Errorf("source missing from lockfile")
	}
	p, err := prepare(c, id, spec, &old, false, true, inputs[id])
	if err != nil {
		return nil, err
	}
	if !reflect.DeepEqual(p.Locked, old) {
		return nil, fmt.Errorf("resolved skills, local source content or effective tree differs from the lockfile; run sync and review the lockfile")
	}
	return p, nil
}
func selectSkills(root string, s Source) (map[string]string, error) {
	discovered := map[string]string{}
	err := entries(root, func(path string, e fs.DirEntry) error {
		if e.Name() != "SKILL.md" || !e.Type().IsRegular() {
			return nil
		}
		parent := filepath.Dir(path)
		relative, _ := filepath.Rel(root, parent)
		name := filepath.Base(parent)
		if relative == "." {
			name = "."
		}
		discovered[strings.ReplaceAll(relative, "\\", "/")] = name
		return nil
	})
	if err != nil {
		return nil, err
	}
	selected := map[string]string{}
	for _, pattern := range s.Skills {
		matcher, err := globPattern(pattern)
		if err != nil {
			return nil, fmt.Errorf("invalid selector %q: %w", pattern, err)
		}
		count := 0
		for path, name := range discovered {
			if pattern == "*" || matcher.MatchString(path) {
				count++
				if renamed, ok := s.Rename[path]; ok {
					name = renamed
				}
				selected[path] = name
			}
		}
		if count == 0 {
			return nil, fmt.Errorf("selector %q matches no skills (paths are relative to the source root)", pattern)
		}
	}
	for _, path := range sortedKeys(s.Rename) {
		if _, ok := selected[path]; !ok {
			return nil, fmt.Errorf("rename key %q is not a selected skill", path)
		}
	}
	for _, path := range sortedKeys(selected) {
		name := selected[path]
		if path == "." && name == "." {
			return nil, fmt.Errorf("root SKILL.md requires rename = { \".\" = \"name\" }")
		}
		if err := validName(name); err != nil {
			return nil, err
		}
	}
	return selected, nil
}
