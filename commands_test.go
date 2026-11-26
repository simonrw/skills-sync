package main_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestSettingsKeepLockfileCurrent(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("local/a/SKILL.md", "a\n")
	f.manifest("[sources.x]\npath = 'local'")
	f.cli(true, "sync", "--offline")
	lock := f.read("skills.lock")

	f.write("skills.toml", "version = 1\nprefix = 'other-store'\ntargets = ['target-c']\ncommands = ['touch ran']\n[sources.x]\npath = 'local'\n")
	f.cli(true, "sync", "--locked", "--offline")

	if f.read("target-c/a/SKILL.md") != "a\n" {
		t.Fatal("new target not installed")
	}
	f.read("ran")
	if f.read("skills.lock") != lock {
		t.Fatal("settings changed the lockfile")
	}
}

func TestCommandsLifecycle(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("local/a/SKILL.md", "a\n")
	commands := `commands = [
  'test -f skills.lock && test -f store/state.json && test ! -e store/transaction.json && test -f target-a/a/SKILL.md && printf a >> runs',
  'printf b >> runs',
]
`
	source := "[sources.x]\npath = 'local'"
	f.manifest(commands + source)

	for _, command := range []string{"sync", "update"} {
		output := f.cli(true, command, "--dry-run")
		if !strings.Contains(output, "Would run command 1:") || !strings.Contains(output, "Would run command 2:") {
			t.Fatal(output)
		}
		if _, err := os.Stat(filepath.Join(f.root, "runs")); !os.IsNotExist(err) {
			t.Fatal("dry run executed commands")
		}
	}

	f.cli(true, "sync")
	lock := f.read("skills.lock")
	f.cli(true, "sync", "--locked", "--offline")
	f.cli(true, "update", "x")
	f.cli(true, "list", "--json")
	if f.read("runs") != "ababab" || f.read("skills.lock") != lock {
		t.Fatal("commands did not repeat in order or changed the lockfile")
	}

	f.manifest(strings.Replace(commands, "printf b", "printf c", 1) + source)
	f.cli(true, "sync", "--locked", "--offline")
	if f.read("runs") != "abababac" || f.read("skills.lock") != lock {
		t.Fatal("locked sync did not run edited commands with the same lockfile")
	}
}

func TestCommandFailureKeepsInstallation(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	f.write("local/a/SKILL.md", "a\n")
	f.manifest("commands = ['printf x >> runs', 'test -f allow || exit 7', 'touch third']\n[sources.x]\npath = 'local'")
	output := f.cli(false, "sync")
	for _, message := range []string{"command 2", "exit status: 7", "test -f allow || exit 7", "managed installation remains committed"} {
		if !strings.Contains(output, message) {
			t.Fatalf("missing %q in %s", message, output)
		}
	}
	if f.read("target-a/a/SKILL.md") != "a\n" || f.read("runs") != "x" {
		t.Fatal("command failure rolled back installation or earlier commands")
	}
	for _, path := range []string{"third", "store/transaction.json"} {
		if _, err := os.Stat(filepath.Join(f.root, path)); !os.IsNotExist(err) {
			t.Fatalf("unexpected %s after command failure", path)
		}
	}

	lock := f.read("skills.lock")
	f.write("allow", "")
	f.cli(true, "sync", "--locked", "--offline")
	if f.read("runs") != "xx" || f.read("skills.lock") != lock {
		t.Fatal("retry did not restart commands with the committed lock")
	}
	f.read("third")
}

func TestInvalidCommandsFailBeforeInstallation(t *testing.T) {
	for _, commands := range []string{`['touch ran', '']`, `['touch ran', '   ']`, `['touch ran', "\u0000"]`, `['touch ran', 42]`, `'touch ran'`} {
		t.Run(commands, func(t *testing.T) {
			t.Parallel()
			f := newFixture(t)
			f.manifest("commands = " + commands + "\n[sources]")
			f.cli(false, "sync")
			for _, path := range []string{"ran", "skills.lock", "store", "target-a"} {
				if _, err := os.Stat(filepath.Join(f.root, path)); !os.IsNotExist(err) {
					t.Fatalf("invalid commands created %s", path)
				}
			}
		})
	}
}
