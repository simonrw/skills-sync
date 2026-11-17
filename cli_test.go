package main_test

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

var binary string

func TestMain(m *testing.M) {
	dir, err := os.MkdirTemp("", "skills-sync-tests-")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	binary = filepath.Join(dir, "skills-sync")
	command := exec.Command("go", "build", "-o", binary, ".")
	if output, err := command.CombinedOutput(); err != nil {
		fmt.Fprintf(os.Stderr, "build: %v\n%s", err, output)
		os.RemoveAll(dir)
		os.Exit(1)
	}
	code := m.Run()
	os.RemoveAll(dir)
	os.Exit(code)
}

type fixture struct {
	t    *testing.T
	root string
	env  []string
}

func newFixture(t *testing.T) *fixture {
	t.Helper()
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	env := []string{}
	for _, v := range os.Environ() {
		key, _, _ := strings.Cut(v, "=")
		if key != "HOME" && key != "XDG_STATE_HOME" && key != "XDG_CONFIG_HOME" && key != "GIT_CONFIG_GLOBAL" && key != "GIT_CONFIG_NOSYSTEM" {
			env = append(env, v)
		}
	}
	env = append(env, "HOME="+filepath.Join(root, "home"), "XDG_STATE_HOME="+filepath.Join(root, "state"), "XDG_CONFIG_HOME="+filepath.Join(root, "config"), "GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_NOSYSTEM=1")
	return &fixture{t, root, env}
}
func (f *fixture) write(path, text string) {
	f.t.Helper()
	full := filepath.Join(f.root, path)
	if err := os.MkdirAll(filepath.Dir(full), 0755); err != nil {
		f.t.Fatal(err)
	}
	if err := os.WriteFile(full, []byte(text), 0644); err != nil {
		f.t.Fatal(err)
	}
}
func (f *fixture) read(path string) string {
	f.t.Helper()
	data, err := os.ReadFile(filepath.Join(f.root, path))
	if err != nil {
		f.t.Fatal(err)
	}
	return string(data)
}
func (f *fixture) cli(success bool, args ...string) string {
	f.t.Helper()
	cmd := exec.Command(binary, args...)
	cmd.Dir = f.root
	cmd.Env = f.env
	output, err := cmd.CombinedOutput()
	if (err == nil) != success {
		f.t.Fatalf("%v: %v\n%s", args, err, output)
	}
	return string(output)
}
func (f *fixture) manifest(source string) {
	f.write("skills.toml", "version = 1\nprefix = 'store'\ntargets = ['target-a', 'target-b']\n"+source+"\n")
}
func (f *fixture) git(args ...string) string {
	f.t.Helper()
	cmd := exec.Command("git", append([]string{"-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false", "-c", "user.name=Test", "-c", "user.email=test@example.com"}, args...)...)
	cmd.Dir = filepath.Join(f.root, "upstream")
	cmd.Env = f.env
	output, err := cmd.CombinedOutput()
	if err != nil {
		f.t.Fatalf("git %v: %v\n%s", args, err, output)
	}
	return strings.TrimSpace(string(output))
}
func TestLocalLifecycle(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("local/deep/alpha/SKILL.md", "alpha v1\n")
	f.write("local/.hidden/beta/SKILL.md", "beta\n")
	f.manifest("[sources.personal]\npath = 'local'")
	f.cli(true, "sync", "--dry-run")
	if _, err := os.Lstat(filepath.Join(f.root, "skills.lock")); !os.IsNotExist(err) {
		t.Fatal("dry run wrote lockfile")
	}

	f.cli(true, "sync", "--offline")
	lock, state := f.read("skills.lock"), f.read("store/state.json")
	if f.read("target-a/alpha/SKILL.md") != "alpha v1\n" {
		t.Fatal("wrong installed content")
	}
	if output := f.cli(true, "sync", "--locked", "--offline"); !strings.Contains(output, "0 link changes") {
		t.Fatal(output)
	}
	if f.read("skills.lock") != lock || f.read("store/state.json") != state {
		t.Fatal("locked replay changed state")
	}

	f.write("local/deep/alpha/SKILL.md", "alpha v2\n")
	f.cli(false, "sync", "--locked")
	if f.read("skills.lock") != lock || f.read("target-a/alpha/SKILL.md") != "alpha v1\n" {
		t.Fatal("failed plan changed installation")
	}
	f.cli(true, "sync")
	if f.read("target-a/alpha/SKILL.md") != "alpha v2\n" {
		t.Fatal("did not update local source")
	}

	f.write("target-a/unmanaged/SKILL.md", "keep\n")
	f.manifest("[sources.personal]\npath = 'local'\nskills = ['deep/**']\nrename = {'deep/alpha' = 'custom'}")
	f.cli(true, "sync")
	if f.read("target-b/custom/SKILL.md") != "alpha v2\n" {
		t.Fatal("rename not installed")
	}
	if _, err := os.Lstat(filepath.Join(f.root, "target-a/beta")); !os.IsNotExist(err) {
		t.Fatal("owned link not pruned")
	}
	f.manifest("[sources]")
	f.cli(true, "sync")
	if f.read("target-a/unmanaged/SKILL.md") != "keep\n" {
		t.Fatal("unmanaged file changed")
	}
}
func TestGitPinningAndUpdates(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("upstream/a/SKILL.md", "first\n")
	f.git("init", "-b", "main")
	f.git("add", ".")
	f.git("commit", "-m", "first")
	f.manifest("[sources.x]\nrepo = 'upstream'\nref = 'main'")
	f.cli(false, "sync", "--offline")
	f.cli(true, "sync")
	lock := f.read("skills.lock")

	f.write("upstream/a/SKILL.md", "second\n")
	f.git("add", ".")
	f.git("commit", "-m", "second")
	f.cli(true, "sync")
	f.cli(true, "sync", "--locked", "--offline")
	f.cli(true, "update", "--dry-run")
	if f.read("skills.lock") != lock || f.read("target-a/a/SKILL.md") != "first\n" {
		t.Fatal("ordinary sync advanced a locked commit")
	}
	f.cli(true, "update", "x")
	if f.read("target-a/a/SKILL.md") != "second\n" {
		t.Fatal("update did not advance")
	}

	for _, path := range []string{"store", "target-a", "target-b"} {
		if err := os.RemoveAll(filepath.Join(f.root, path)); err != nil {
			t.Fatal(err)
		}
	}
	f.cli(true, "sync", "--locked")
	if err := os.RemoveAll(filepath.Join(f.root, "upstream")); err != nil {
		t.Fatal(err)
	}
	f.cli(true, "sync", "--locked", "--offline")
	if f.read("target-b/a/SKILL.md") != "second\n" {
		t.Fatal("offline replay failed")
	}
}
func TestPatchGenerationAndProtectedContent(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	source := "# Title\n\n## Tools\nUse old tools. Keep `old` code.\n"
	edited := strings.Replace(source, "Use old", "Use new", 1)
	f.write("local/a/SKILL.md", source)
	f.write("edited.md", edited)
	f.manifest("[sources.x]\npath = 'local'")
	f.cli(true, "sync")
	lock := f.read("skills.lock")
	f.cli(true, "patch", "generate", "x", "a/SKILL.md", "edited.md", "--output", "edit.skillpatch.toml")
	expected := "version = 1\nfile = \"a/SKILL.md\"\n\n[[operations]]\nop = \"replace_text\"\nsection = [\"Tools\"]\nfind = \"old\"\nreplace = \"new\"\nwhole_words = false\n\n[operations.expect]\nmin = 1\nmax = 1\n"
	if f.read("edit.skillpatch.toml") != expected {
		t.Fatalf("unexpected patch:\n%s", f.read("edit.skillpatch.toml"))
	}
	if f.read("skills.lock") != lock || f.read("target-a/a/SKILL.md") != source {
		t.Fatal("generation changed installation")
	}

	f.manifest("[sources.x]\npath = 'local'\npatches = ['edit.skillpatch.toml']")
	f.cli(true, "sync")
	if f.read("target-a/a/SKILL.md") != edited {
		t.Fatal("generated patch does not replay")
	}
	f.cli(true, "sync", "--locked", "--offline")
	f.write("edited.md", strings.Replace(edited, "`old`", "`unsafe`", 1))
	f.cli(false, "patch", "generate", "x", "a/SKILL.md", "edited.md", "--output", "unsafe.skillpatch.toml")
	if _, err := os.Lstat(filepath.Join(f.root, "unsafe.skillpatch.toml")); !os.IsNotExist(err) {
		t.Fatal("unsafe patch was written")
	}
}
func TestScopedPatchBoundaries(t *testing.T) {
	cases := []struct {
		name, source, find, section, expected string
		count                                 int
		words                                 bool
	}{
		{"protected syntax", "target `target` [target](target) *target*\n", "target", "[]", "chosen `target` [chosen](target) *chosen*\n", 3, false},
		{"inline HTML", "Before <span>target</span> target.\n\nSeparate target.\n", "target", "[]", "Before <span>target</span> target.\n\nSeparate chosen.\n", 1, false},
		{"Unicode 17 words", "aᲉb\n", "Ᲊ", "[]", "aᲉb\n", 0, true},
		{"unknown entities", "&notit; notit\n", "notit", "[]", "&chosen; chosen\n", 2, false},
		{"scoped rewrapping", "# T\n\n## Tools\nold\n  tool here.\n\n## Other\nold tool\n", "old tool", "[\"Tools\"]", "# T\n\n## Tools\nchosen here.\n\n## Other\nold tool\n", 1, false},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			f := newFixture(t)
			f.write("local/a/SKILL.md", test.source)
			f.write("edit.skillpatch.toml", fmt.Sprintf("version = 1\nfile = 'a/SKILL.md'\n[[operations]]\nop = 'replace_text'\nsection = %s\nfind = %q\nreplace = 'chosen'\nwhole_words = %t\nexpect = {min = %d, max = %d}\n", test.section, test.find, test.words, test.count, test.count))
			f.manifest("[sources.x]\npath = 'local'\npatches = ['edit.skillpatch.toml']")
			f.cli(true, "sync")
			if actual := f.read("target-a/a/SKILL.md"); actual != test.expected {
				t.Fatalf("installed document:\n%q\nwant:\n%q", actual, test.expected)
			}
		})
	}
}

func TestOwnershipAndRecovery(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("local/a/SKILL.md", "a\n")
	f.manifest("[sources.x]\npath = 'local'")
	f.write("target-b/a/SKILL.md", "mine\n")
	f.cli(false, "sync")
	if _, err := os.Lstat(filepath.Join(f.root, "target-a/a")); !os.IsNotExist(err) {
		t.Fatal("partial installation")
	}
	if err := os.RemoveAll(filepath.Join(f.root, "target-b/a")); err != nil {
		t.Fatal(err)
	}
	f.cli(true, "sync")

	lock := f.read("skills.lock")
	var before, after map[string]any
	if err := json.Unmarshal([]byte(f.read("store/state.json")), &before); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal([]byte(f.read("store/state.json")), &after); err != nil {
		t.Fatal(err)
	}
	links := after["links"].(map[string]any)
	destination := links[filepath.Join(f.root, "target-a/a")].(string)
	extra := filepath.Join(f.root, "target-a/extra")
	links[extra] = destination
	if err := os.Symlink(destination, extra); err != nil {
		t.Fatal(err)
	}
	journal, err := json.Marshal(map[string]any{"before": before, "after": after, "lock_before": lock})
	if err != nil {
		t.Fatal(err)
	}
	f.write("store/transaction.json", string(journal))
	f.write("skills.lock", "interrupted")
	f.cli(false, "list")
	f.cli(true, "sync", "--dry-run")
	if f.read("skills.lock") != lock {
		t.Fatal("lockfile not recovered")
	}
	if _, err := os.Lstat(extra); !os.IsNotExist(err) {
		t.Fatal("extra link not recovered")
	}
}
func TestGlobalPrefixAndRootSkill(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("local/SKILL.md", "root\n")
	f.write("skills.toml", "version=1\ntargets=['target-a']\n[sources.x]\npath='local'\nrename={'.'='root'}\n")
	f.cli(true, "sync")
	destination, err := os.Readlink(filepath.Join(f.root, "target-a/root"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(destination, filepath.Join(f.root, "state", "skills-sync")+string(filepath.Separator)) {
		t.Fatal(destination)
	}
	f.cli(true, "sync", "--locked", "--offline")
	if f.read("target-a/root/SKILL.md") != "root\n" {
		t.Fatal("root skill missing")
	}
}
func TestSourceAndManifestValidation(t *testing.T) {
	for _, source := range []string{"[sources.x]", "[sources.x]\npath='local'\nrepo='local'", "[sources.x]\npath='local'\nref='main'", "[sources.x]\npath='local'\nskills=[]", "[sources.x]\npath='local'\nskills=['missing']", "[sources.x]\npath='local'\nrename={a='..'}", "[sources.x]\npath='local'\nunknown=true"} {
		t.Run(source, func(t *testing.T) {
			t.Parallel()
			f := newFixture(t)
			f.write("local/a/SKILL.md", "a\n")
			f.manifest(source)
			f.cli(false, "sync")
			if _, err := os.Lstat(filepath.Join(f.root, "skills.lock")); !os.IsNotExist(err) {
				t.Fatal("invalid plan wrote lockfile")
			}
		})
	}
}
func TestDifferentialReference(t *testing.T) {
	reference := os.Getenv("SKILLS_SYNC_RUST")
	if reference == "" {
		t.Skip("set SKILLS_SYNC_RUST to run the Rust/Go differential suite")
	}
	reference, err := filepath.Abs(reference)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command("python3", "tests/parity.py", "--rust", reference, "--go", binary)
	if output, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("differential parity: %v\n%s", err, output)
	}
}
