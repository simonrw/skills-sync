package main

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"syscall"
)

type Options struct {
	Locked, Offline, DryRun bool
	Update                  []string
}
type Installed struct {
	Name        string `json:"name"`
	Source      string `json:"source"`
	Upstream    string `json:"upstream"`
	Revision    string `json:"revision"`
	Path        string `json:"path"`
	Destination string `json:"destination"`
}
type State struct {
	Version uint32            `json:"version"`
	Owner   string            `json:"owner"`
	Targets []string          `json:"targets"`
	Links   map[string]string `json:"links"`
	Skills  []Installed       `json:"skills"`
}
type Journal struct {
	Before     State   `json:"before"`
	After      State   `json:"after"`
	LockBefore *string `json:"lock_before"`
}

func (s *State) UnmarshalJSON(data []byte) error {
	type plain State
	var decoded plain
	if err := decodeRequiredJSON(data, &decoded, "version", "owner", "targets", "links", "skills"); err != nil {
		return err
	}
	var containers struct {
		Targets []*string          `json:"targets"`
		Links   map[string]*string `json:"links"`
	}
	if err := json.Unmarshal(data, &containers); err != nil {
		return err
	}
	for _, target := range containers.Targets {
		if target == nil {
			return fmt.Errorf("null installation target")
		}
	}
	for _, destination := range containers.Links {
		if destination == nil {
			return fmt.Errorf("null link destination")
		}
	}
	*s = State(decoded)
	return nil
}

func (s *Installed) UnmarshalJSON(data []byte) error {
	type plain Installed
	return decodeRequiredJSON(data, (*plain)(s), "name", "source", "upstream", "revision", "path", "destination")
}

func (j *Journal) UnmarshalJSON(data []byte) error {
	type plain Journal
	return decodeRequiredJSON(data, (*plain)(j), "before", "after")
}

func emptyState(c *Loaded) State {
	return State{1, c.Lock, []string{}, map[string]string{}, []Installed{}}
}
func prefixGuard(c *Loaded, create bool) (*os.File, error) {
	flags := os.O_RDWR
	if create {
		flags |= os.O_CREATE
	}
	f, err := os.OpenFile(filepath.Join(c.Prefix, "sync.lock"), flags, 0666)
	if !create && os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		f.Close()
		return nil, fmt.Errorf("another sync is already running for this prefix: %w", err)
	}
	return f, nil
}
func validateState(c *Loaded, s *State) error {
	if s.Version != 1 {
		return fmt.Errorf("unsupported installation state version")
	}
	if s.Owner != c.Lock {
		return fmt.Errorf("prefix belongs to manifest %s; use a different prefix", s.Owner)
	}
	for _, path := range sortedKeys(s.Links) {
		target := false
		for _, t := range s.Targets {
			if filepath.Dir(path) == t {
				target = true
			}
		}
		if !target || !within(s.Links[path], filepath.Join(c.Prefix, "sources")) {
			return fmt.Errorf("invalid managed link in installation state")
		}
		if err := validName(filepath.Base(path)); err != nil {
			return err
		}
	}
	return nil
}
func union(before, after map[string]string) []string {
	all := map[string]bool{}
	for p := range before {
		all[p] = true
	}
	for p := range after {
		all[p] = true
	}
	return sortedKeys(all)
}
func currentLink(path string) (string, error) {
	meta, err := os.Lstat(path)
	if os.IsNotExist(err) {
		return "", nil
	}
	if err != nil {
		return "", err
	}
	if meta.Mode()&os.ModeSymlink == 0 {
		return "", fmt.Errorf("refusing to overwrite unmanaged file or directory %s", path)
	}
	return os.Readlink(path)
}
func preflight(before, after map[string]string) error {
	for _, path := range union(before, after) {
		actual, err := currentLink(path)
		if err != nil {
			return err
		}
		if actual != "" && before[path] != actual {
			return fmt.Errorf("refusing to overwrite unmanaged or externally changed symlink %s", path)
		}
		for dir := filepath.Dir(path); ; dir = filepath.Dir(dir) {
			if meta, err := os.Stat(dir); err == nil && !meta.IsDir() {
				return fmt.Errorf("target ancestor is not a directory: %s", dir)
			}
			if filepath.Dir(dir) == dir {
				break
			}
		}
	}
	return nil
}
func replaceLink(path, destination string) error {
	parent := filepath.Dir(path)
	if err := os.MkdirAll(parent, 0777); err != nil {
		return err
	}
	staging, err := os.MkdirTemp(parent, ".tmp")
	if err != nil {
		return err
	}
	defer os.RemoveAll(staging)
	temporary := filepath.Join(staging, "link")
	if err := os.Symlink(destination, temporary); err != nil {
		return err
	}
	return os.Rename(temporary, path)
}
func reconcile(before, after map[string]string) error {
	for _, path := range union(before, after) {
		actual, err := currentLink(path)
		if err != nil {
			return err
		}
		if actual == after[path] {
			continue
		}
		if actual != "" && actual != before[path] {
			return fmt.Errorf("managed link changed during sync: %s", path)
		}
		if destination, ok := after[path]; ok {
			if err := replaceLink(path, destination); err != nil {
				return err
			}
		} else if actual != "" {
			if err := os.Remove(path); err != nil {
				return err
			}
		}
	}
	return nil
}
func recoverTransaction(c *Loaded) error {
	path := filepath.Join(c.Prefix, "transaction.json")
	var j Journal
	found, err := readJSON(path, &j)
	if err != nil || !found {
		return err
	}
	if err := validateState(c, &j.Before); err != nil {
		return err
	}
	if err := validateState(c, &j.After); err != nil {
		return err
	}
	fmt.Fprintln(os.Stderr, "Recovering interrupted sync ...")
	for _, path := range union(j.Before.Links, j.After.Links) {
		actual, err := currentLink(path)
		if err != nil {
			return err
		}
		if actual != "" && j.Before.Links[path] != actual && j.After.Links[path] != actual {
			return fmt.Errorf("cannot recover externally changed link %s", path)
		}
	}
	if err := reconcile(j.After.Links, j.Before.Links); err != nil {
		return err
	}
	if j.LockBefore != nil {
		if err := atomicWrite(c.Lock, []byte(*j.LockBefore)); err != nil {
			return err
		}
	} else if exists(c.Lock) {
		if err := os.Remove(c.Lock); err != nil {
			return err
		}
	}
	if err := writeJSON(filepath.Join(c.Prefix, "state.json"), j.Before); err != nil {
		return err
	}
	return os.Remove(path)
}
func syncRun(c *Loaded, o Options) error {
	if err := os.MkdirAll(c.Prefix, 0777); err != nil {
		return err
	}
	guard, err := prefixGuard(c, true)
	if err != nil {
		return err
	}
	defer guard.Close()
	if err := recoverTransaction(c); err != nil {
		return err
	}
	previous := emptyState(c)
	if _, err := readJSON(filepath.Join(c.Prefix, "state.json"), &previous); err != nil {
		return err
	}
	if err := validateState(c, &previous); err != nil {
		return err
	}
	old, err := readLock(c)
	if err != nil {
		return err
	}
	for _, id := range o.Update {
		if _, ok := c.Manifest.Sources[id]; !ok {
			return fmt.Errorf("unknown source ID %q", id)
		}
	}
	inputs, err := loadPatchInputs(c)
	if err != nil {
		return err
	}
	fp, err := fingerprint(c, inputs)
	if err != nil {
		return err
	}
	if o.Locked && (old == nil || old.ManifestSHA256 != fp) {
		return fmt.Errorf("lockfile is missing or manifest/patches changed; run sync without --locked and review the lockfile")
	}
	lock := &LockFile{1, fp, map[string]LockedSource{}}
	next := emptyState(c)
	next.Targets = c.Targets
	names := map[string]string{}
	reports := []PatchReport{}
	for _, id := range sortedKeys(c.Manifest.Sources) {
		fmt.Fprintf(os.Stderr, "Resolving %s ...\n", id)
		update := o.Update != nil && len(o.Update) == 0
		for _, name := range o.Update {
			if name == id {
				update = true
			}
		}
		var prior *LockedSource
		if old != nil {
			if p, ok := old.Sources[id]; ok {
				prior = &p
			}
		}
		prepared, err := prepare(c, id, c.Manifest.Sources[id], prior, update, o.Offline, inputs[id])
		if err != nil {
			return fmt.Errorf("preparing source %s: %w", id, err)
		}
		for _, path := range sortedKeys(prepared.Locked.Skills) {
			name := prepared.Locked.Skills[path]
			if other, ok := names[name]; ok {
				return fmt.Errorf("install name %q collides between %s and %s:%s; use rename", name, other, id, path)
			}
			names[name] = id + ":" + path
			destination := filepath.Join(prepared.Tree, path)
			for _, target := range c.Targets {
				next.Links[filepath.Join(target, name)] = destination
			}
			next.Skills = append(next.Skills, Installed{name, id, prepared.Locked.Upstream, prepared.Locked.Revision, path, destination})
		}
		reports = append(reports, prepared.Reports...)
		lock.Sources[id] = prepared.Locked
	}
	if o.Locked && !reflect.DeepEqual(old, lock) {
		return fmt.Errorf("resolved skills or local source content differs from the lockfile; run sync without --locked")
	}
	if err := preflight(previous.Links, next.Links); err != nil {
		return err
	}
	for _, report := range reports {
		report.Print(o.DryRun)
	}
	changes := 0
	for _, path := range union(previous.Links, next.Links) {
		actual, err := currentLink(path)
		if err != nil {
			return err
		}
		if actual == next.Links[path] {
			continue
		}
		changes++
		if destination, ok := next.Links[path]; ok {
			fmt.Printf("link %s -> %s\n", path, destination)
		} else {
			fmt.Printf("remove %s\n", path)
		}
	}
	if o.DryRun {
		for i, command := range c.Manifest.Commands {
			fmt.Printf("Would run command %d: %s\n", i+1, command)
		}
		fmt.Printf("Dry run: %d skills, %d link changes; lockfile and installed links unchanged\n", len(next.Skills), changes)
		return nil
	}
	before, err := readOptional(c.Lock)
	if err != nil {
		return err
	}
	journal := Journal{previous, next, before}
	journalPath := filepath.Join(c.Prefix, "transaction.json")
	if err := writeJSON(journalPath, journal); err != nil {
		return err
	}
	err = func() error {
		if err := reconcile(previous.Links, next.Links); err != nil {
			return err
		}
		if !o.Locked {
			if err := atomicWrite(c.Lock, serializeLock(lock)); err != nil {
				return err
			}
		}
		if err := writeJSON(filepath.Join(c.Prefix, "state.json"), next); err != nil {
			return err
		}
		return os.Remove(journalPath)
	}()
	if err != nil {
		if rollback := recoverTransaction(c); rollback != nil {
			return fmt.Errorf("sync failed: %v; rollback failed: %v; transaction journal retained", err, rollback)
		}
		return fmt.Errorf("sync failed; previous installation restored: %w", err)
	}
	if err := runCommands(c); err != nil {
		return fmt.Errorf("post-sync command failed; managed installation remains committed: %w", err)
	}
	fmt.Printf("Synced %d skills to %d targets (%d link changes)\n", len(next.Skills), len(c.Targets), changes)
	return nil
}
func runCommands(c *Loaded) error {
	for i, command := range c.Manifest.Commands {
		fmt.Fprintf(os.Stderr, "Running command %d: %s\n", i+1, command)
		cmd := exec.Command("/bin/sh", "-c", command)
		cmd.Dir = c.Base
		cmd.Stdin = os.Stdin
		cmd.Stdout = os.Stdout
		cmd.Stderr = os.Stderr

		if err := cmd.Run(); err != nil {
			if exit, ok := err.(*exec.ExitError); ok && exit.ExitCode() >= 0 {
				return fmt.Errorf("command %d failed (exit status: %d): %s", i+1, exit.ExitCode(), command)
			}
			return fmt.Errorf("running command %d: %s: %w", i+1, command, err)
		}
	}
	return nil
}

func list(c *Loaded, jsonOutput bool) error {
	if exists(filepath.Join(c.Prefix, "transaction.json")) {
		return fmt.Errorf("interrupted transaction; run sync to recover before listing")
	}
	var state State
	found, err := readJSON(filepath.Join(c.Prefix, "state.json"), &state)
	if err != nil {
		return err
	}
	if !found {
		return fmt.Errorf("no installed state; run sync first")
	}
	if err := validateState(c, &state); err != nil {
		return err
	}
	if jsonOutput {
		data, err := serdeJSON(state.Skills, true)
		if err != nil {
			return err
		}
		fmt.Println(string(data))
	} else {
		for _, skill := range state.Skills {
			fmt.Printf("%s\t%s:%s\t%s@%s\n", skill.Name, skill.Source, skill.Path, skill.Upstream, skill.Revision)
		}
	}
	return nil
}
func installedTargets(c *Loaded) ([]string, error) {
	var state State
	found, err := readJSON(filepath.Join(c.Prefix, "state.json"), &state)
	if err != nil || !found {
		return nil, err
	}
	if err := validateState(c, &state); err != nil {
		return nil, err
	}
	return state.Targets, nil
}
