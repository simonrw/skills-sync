package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"unicode/utf8"
)

func hash(data []byte) string { sum := sha256.Sum256(data); return hex.EncodeToString(sum[:]) }

// serdeJSON preserves the serialization used in existing content addresses.
func serdeJSON(value any, pretty bool) ([]byte, error) {
	var b bytes.Buffer
	encoder := json.NewEncoder(&b)
	encoder.SetEscapeHTML(false)
	if pretty {
		encoder.SetIndent("", "  ")
	}
	if err := encoder.Encode(value); err != nil {
		return nil, err
	}
	data := bytes.TrimSuffix(b.Bytes(), []byte("\n"))
	var result bytes.Buffer
	for i := 0; i < len(data); i++ {
		if data[i] == '\\' && i+1 < len(data) {
			if i+6 <= len(data) && (string(data[i:i+6]) == `\u2028` || string(data[i:i+6]) == `\u2029`) {
				if data[i+5] == '8' {
					result.WriteRune('\u2028')
				} else {
					result.WriteRune('\u2029')
				}
				i += 5
				continue
			}
			result.Write(data[i : i+2])
			i++
			continue
		}
		result.WriteByte(data[i])
	}
	return result.Bytes(), nil
}
func jsonHash(value any) (string, error) {
	data, err := serdeJSON(value, false)
	if err != nil {
		return "", err
	}
	return hash(data), nil
}
func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
func validName(name string) error {
	if name != "" && name != "." && name != ".." {
		valid := true
		for _, c := range name {
			if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || strings.ContainsRune("-_.", c)) {
				valid = false
			}
		}
		if valid {
			return nil
		}
	}
	return fmt.Errorf("invalid ID or install name %q; use letters, digits, '.', '_' or '-'", name)
}
func absolute(base, path string) (string, error) {
	if !utf8.ValidString(path) {
		return "", fmt.Errorf("paths must be valid UTF-8")
	}
	if path == "~" || strings.HasPrefix(path, "~/") {
		home, ok := os.LookupEnv("HOME")
		if !ok {
			return "", fmt.Errorf("HOME is required for ~ paths")
		}
		path = filepath.Join(home, strings.TrimPrefix(strings.TrimPrefix(path, "~"), "/"))
	}
	if !filepath.IsAbs(path) {
		path = filepath.Join(base, path)
	}
	return filepath.Clean(path), nil
}
func resolvedPath(base, path string) (string, error) {
	full, err := absolute(base, path)
	if err != nil {
		return "", err
	}
	ancestor := full
	var tail []string
	for {
		_, err := os.Lstat(ancestor)
		if err == nil {
			resolved, err := filepath.EvalSymlinks(ancestor)
			if err != nil {
				return "", err
			}
			for i := len(tail) - 1; i >= 0; i-- {
				resolved = filepath.Join(resolved, tail[i])
			}
			return resolved, nil
		}
		if !os.IsNotExist(err) {
			return "", err
		}
		parent := filepath.Dir(ancestor)
		if parent == ancestor {
			return "", fmt.Errorf("path has no existing ancestor")
		}
		tail = append(tail, filepath.Base(ancestor))
		ancestor = parent
	}
}
func within(path, root string) bool {
	rel, err := filepath.Rel(root, path)
	return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}
func overlaps(a, b string) bool { return within(a, b) || within(b, a) }
func exists(path string) bool   { _, err := os.Stat(path); return err == nil }
func createNew(path string, data []byte) error {
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0666)
	if err != nil {
		return fmt.Errorf("creating %s: %w", path, err)
	}
	defer f.Close()
	if _, err := f.Write(data); err != nil {
		return err
	}
	return f.Sync()
}
func atomicWrite(path string, data []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0777); err != nil {
		return err
	}
	f, err := os.CreateTemp(filepath.Dir(path), ".tmp")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	defer f.Close()
	if _, err := f.Write(data); err != nil {
		return err
	}
	if err := f.Sync(); err != nil {
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	return os.Rename(f.Name(), path)
}
func writeJSON(path string, value any) error {
	data, err := serdeJSON(value, true)
	if err != nil {
		return err
	}
	return atomicWrite(path, data)
}
func readOptional(path string) (*string, error) {
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if !utf8.Valid(data) {
		return nil, fmt.Errorf("invalid UTF-8 in %s", path)
	}
	s := string(data)
	return &s, nil
}
func readJSON(path string, value any) (bool, error) {
	data, err := readOptional(path)
	if err != nil || data == nil {
		return false, err
	}
	if err := json.Unmarshal([]byte(*data), value); err != nil {
		return false, fmt.Errorf("invalid state %s: %w", path, err)
	}
	return true, nil
}
func decodeRequiredJSON(data []byte, value any, fields ...string) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	token, err := decoder.Token()
	if err != nil {
		return err
	}
	if token != json.Delim('{') {
		return fmt.Errorf("expected JSON object")
	}
	required := map[string]bool{}
	for _, field := range fields {
		required[field] = true
	}
	raw := map[string]json.RawMessage{}
	for decoder.More() {
		token, err := decoder.Token()
		if err != nil {
			return err
		}
		field, ok := token.(string)
		if !ok {
			return fmt.Errorf("expected JSON field name")
		}
		if _, exists := raw[field]; exists && required[field] {
			return fmt.Errorf("duplicate field %s", field)
		}
		var encoded json.RawMessage
		if err := decoder.Decode(&encoded); err != nil {
			return err
		}
		raw[field] = encoded
	}
	for _, field := range fields {
		encoded, ok := raw[field]
		if !ok || bytes.Equal(bytes.TrimSpace(encoded), []byte("null")) {
			return fmt.Errorf("missing or null field %s", field)
		}
	}
	return json.Unmarshal(data, value)
}

func git(directory string, args ...string) ([]byte, error) {
	cmd := exec.Command("git", append([]string{"-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false"}, args...)...)
	cmd.Dir = directory
	for _, env := range os.Environ() {
		key, _, _ := strings.Cut(env, "=")
		if key != "GIT_DIR" && key != "GIT_WORK_TREE" && key != "GIT_INDEX_FILE" && key != "GIT_TERMINAL_PROMPT" {
			cmd.Env = append(cmd.Env, env)
		}
	}
	cmd.Env = append(cmd.Env, "GIT_TERMINAL_PROMPT=0")
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	data, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("git %s failed: %s: %w", args[0], strings.TrimSpace(stderr.String()), err)
	}
	return data, nil
}
func copyFile(from, to string, mode os.FileMode) error {
	in, err := os.Open(from)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(to, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, mode)
	if err != nil {
		return err
	}
	defer out.Close()
	if _, err := io.Copy(out, in); err != nil {
		return err
	}
	return out.Chmod(mode)
}
