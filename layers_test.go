package main_test

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLocalOverrideOfGlobalManifest(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("config/skills-sync/local/a/SKILL.md", "a\n")
	f.write("config/skills-sync/skills.toml", "version = 1\ntargets = ['global-target']\n[sources.g]\npath = 'local'\n")
	f.write("skills.local.toml", "prefix = 'store'\ntargets = ['work-target']\n")

	f.cli(true, "sync")

	if f.read("work-target/a/SKILL.md") != "a\n" {
		t.Fatal("global source not installed into local target")
	}
	for _, path := range []string{"global-target", "config/skills-sync/global-target", "config/skills-sync/skills.lock"} {
		if _, err := os.Lstat(filepath.Join(f.root, path)); !os.IsNotExist(err) {
			t.Fatalf("%s should not exist", path)
		}
	}

	f.read("skills.lock")
	f.cli(true, "sync", "--locked", "--offline")
}

func TestManifestLayerPrecedence(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("config/skills-sync/g/ga/SKILL.md", "ga\n")
	f.write("p/pb/SKILL.md", "pb\n")
	f.write("l/lc/SKILL.md", "lc\n")
	f.write("config/skills-sync/skills.toml", "version = 1\ntargets = ['global-target']\n[sources.g]\npath = 'g'\n[sources.shared]\npath = 'g'\n")
	f.write("skills.toml", "prefix = 'store'\ntargets = ['project-target']\n[sources.p]\npath = 'p'\n[sources.shared]\npath = 'p'\nskills = ['pb']\n")
	f.write("skills.local.toml", "targets = ['local-target']\n[sources.shared]\npath = 'l'\n")

	f.cli(true, "sync")

	for _, name := range []string{"ga", "pb", "lc"} {
		if f.read("local-target/"+name+"/SKILL.md") != name+"\n" {
			t.Fatalf("%s not installed", name)
		}
	}

	if _, err := os.Lstat(filepath.Join(f.root, "project-target")); !os.IsNotExist(err) {
		t.Fatal("local targets did not replace project targets")
	}
}

func TestGlobalManifestAlone(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("config/skills-sync/local/a/SKILL.md", "a\n")
	f.write("config/skills-sync/skills.toml", "version = 1\ntargets = ['target']\n[sources.g]\npath = 'local'\n")

	f.cli(true, "sync")

	if f.read("config/skills-sync/target/a/SKILL.md") != "a\n" {
		t.Fatal("global manifest not installed relative to its own directory")
	}
	f.read("config/skills-sync/skills.lock")

	if _, err := os.Lstat(filepath.Join(f.root, "skills.lock")); !os.IsNotExist(err) {
		t.Fatal("lockfile written beside missing project manifest")
	}
}
