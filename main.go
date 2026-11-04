package main

import (
	_ "embed"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

//go:embed examples/skills.toml
var exampleManifest []byte

var version = "0.1.0"

type usageError struct{ message string }

func (e usageError) Error() string { return e.message }
func usageErrorf(format string, args ...any) error {
	return usageError{"error: " + fmt.Sprintf(format, args...)}
}

func main() {
	if err := run(os.Args[1:]); err != nil {
		if e, ok := err.(usageError); ok {
			fmt.Fprint(os.Stderr, e.message)
			if !strings.HasSuffix(e.message, "\n") {
				fmt.Fprintln(os.Stderr)
			}
			os.Exit(2)
		}
		fmt.Fprintln(os.Stderr, "error:", err)
		os.Exit(1)
	}
}
func run(args []string) error {
	manifest := "skills.toml"
	remaining := []string{}
	manifestSeen := false
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if arg == "--" {
			remaining = append(remaining, args[i:]...)
			break
		}
		if arg == "--help" || strings.HasPrefix(arg, "-h") {
			remaining = append(remaining, "--help")
			break
		}
		if arg == "--version" || strings.HasPrefix(arg, "-V") {
			remaining = append(remaining, "--version")
			break
		}
		isManifest := arg == "--manifest" || strings.HasPrefix(arg, "--manifest=") || strings.HasPrefix(arg, "-m")
		if isManifest {
			if manifestSeen {
				return usageErrorf("the argument '--manifest <MANIFEST>' cannot be used multiple times")
			}
			manifestSeen = true
			switch {
			case arg == "--manifest" || arg == "-m":
				i++
				if i == len(args) || strings.HasPrefix(args[i], "-") && args[i] != "-" {
					return usageErrorf("a value is required for '--manifest <MANIFEST>'")
				}
				manifest = args[i]
			case strings.HasPrefix(arg, "--manifest="):
				manifest = strings.TrimPrefix(arg, "--manifest=")
			default:
				manifest = strings.TrimPrefix(arg[2:], "=")
			}
			if manifest == "" {
				return usageErrorf("a value is required for '--manifest <MANIFEST>'")
			}
		} else {
			if len(remaining) == 0 || len(remaining) == 1 && remaining[0] == "patch" && arg == "generate" {
				manifestSeen = false
			}
			remaining = append(remaining, arg)
		}
	}
	if len(remaining) == 0 {
		return usageError{commandHelp("")}
	}
	command := remaining[0]
	args = remaining[1:]
	if command == "--version" || command == "-V" {
		fmt.Println("skills-sync " + version)
		return nil
	}
	if command == "--help" || command == "-h" {
		fmt.Print(commandHelp(""))
		return nil
	}
	if command == "help" {
		path := strings.Join(args, " ")
		help := commandHelp(path)
		if help == "" {
			return usageErrorf("unrecognized subcommand %q", path)
		}
		fmt.Print(help)
		return nil
	}
	if commandHelp(command) == "" {
		return usageErrorf("unrecognized subcommand %q", command)
	}
	options := Options{}
	jsonOutput := false
	positionals := []string{}
	output := ""
	outputSet := false
	literal := false
	seen := map[string]bool{}
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if literal {
			positionals = append(positionals, arg)
			continue
		}
		flag, _, _ := strings.Cut(arg, "=")
		if strings.HasPrefix(flag, "--") && flag != "--" {
			if seen[flag] {
				return usageErrorf("the argument '%s' cannot be used multiple times", flag)
			}
			seen[flag] = true
		}
		switch {
		case arg == "--":
			literal = true
		case arg == "--locked" && command == "sync":
			options.Locked = true
		case arg == "--offline" && command == "sync":
			options.Offline = true
		case arg == "--dry-run" && (command == "sync" || command == "update"):
			options.DryRun = true
		case arg == "--json" && command == "list":
			jsonOutput = true
		case arg == "--output" && command == "patch" && len(positionals) > 0 && positionals[0] == "generate":
			i++
			if i == len(args) || args[i] == "" || strings.HasPrefix(args[i], "-") && args[i] != "-" {
				return usageErrorf("--output requires a path")
			}
			output = args[i]
			outputSet = true
		case strings.HasPrefix(arg, "--output=") && command == "patch" && len(positionals) > 0 && positionals[0] == "generate":
			output = strings.TrimPrefix(arg, "--output=")
			if output == "" {
				return usageErrorf("--output requires a path")
			}
			outputSet = true
		case arg == "--help" || arg == "-h":
			path := command
			if command == "patch" && len(positionals) > 0 {
				path += " " + positionals[0]
			}
			help := commandHelp(path)
			if help == "" {
				return usageErrorf("unrecognized subcommand %q", path)
			}
			fmt.Print(help)
			return nil
		case strings.HasPrefix(arg, "-") && arg != "-":
			return usageErrorf("unexpected argument %q", arg)
		default:
			positionals = append(positionals, arg)
		}
	}
	if (command == "sync" || command == "list" || command == "init") && len(positionals) > 0 {
		return usageErrorf("unexpected argument %q", positionals[0])
	}
	if command == "init" {
		if err := createNew(manifest, exampleManifest); err != nil {
			return err
		}
		fmt.Printf("Created %s\n", manifest)
		return nil
	}
	if command == "update" {
		options.Update = positionals
	}
	if command == "patch" {
		if len(positionals) > 0 && positionals[0] == "help" {
			help := commandHelp("patch")
			if len(positionals) > 1 {
				help = commandHelp("patch " + positionals[1])
			}
			if help == "" {
				return usageErrorf("unknown patch subcommand")
			}
			fmt.Print(help)
			return nil
		}
		if len(positionals) != 4 || positionals[0] != "generate" || !outputSet {
			return usageErrorf("usage: skills-sync patch generate <SOURCE> <FILE> <EDITED> --output <OUTPUT>")
		}
	}
	config, err := loadConfig(manifest)
	if err != nil {
		return err
	}
	switch command {
	case "sync", "update":
		return syncRun(config, options)
	case "list":
		return list(config, jsonOutput)
	case "patch":
		return generateRun(config, positionals[1], positionals[2], positionals[3], output)
	}
	return nil
}

const manifestHelp = "  -m, --manifest <MANIFEST>  Relative manifest paths are resolved from the manifest directory [default: skills.toml]\n"
const helpOption = "  -h, --help                 Print help\n"

func commandHelp(command string) string {
	name := filepath.Base(os.Args[0])
	var text string
	switch command {
	case "":
		text = `Declarative, commit-pinned skills with managed symlinks and patches

Usage: %s [OPTIONS] <COMMAND>

Commands:
  init    Write an example manifest without overwriting an existing file
  sync    Reconcile skills, preserving already locked Git commits
  update  Advance commits and sync. Optional arguments limit the source IDs updated
  patch   Generate a verified scoped Markdown patch from the locked effective source
  list    Show the last successfully installed skills and their upstream origins
  help    Print this message or the help of the given subcommand(s)

Options:
` + manifestHelp + helpOption + "  -V, --version              Print version\n"
	case "init":
		text = "Write an example manifest without overwriting an existing file\n\nUsage: %s init [OPTIONS]\n\nOptions:\n" + manifestHelp + helpOption
	case "sync":
		text = "Reconcile skills, preserving already locked Git commits\n\nUsage: %s sync [OPTIONS]\n\nOptions:\n      --locked               Require unchanged manifest, patches, local content, and lockfile\n" + manifestHelp + "      --offline              Never fetch Git objects from upstream\n      --dry-run              Validate a plan without changing links or lockfile (may populate cache)\n" + helpOption
	case "update":
		text = "Advance commits and sync. Optional arguments limit the source IDs updated\n\nUsage: %s update [OPTIONS] [SOURCES]...\n\nArguments:\n  [SOURCES]...  \n\nOptions:\n      --dry-run              \n" + manifestHelp + helpOption
	case "list":
		text = "Show the last successfully installed skills and their upstream origins\n\nUsage: %s list [OPTIONS]\n\nOptions:\n      --json                 \n" + manifestHelp + helpOption
	case "patch":
		text = "Generate a verified scoped Markdown patch from the locked effective source\n\nUsage: %s patch [OPTIONS] <COMMAND>\n\nCommands:\n  generate  \n  help      Print this message or the help of the given subcommand(s)\n\nOptions:\n" + manifestHelp + helpOption
	case "patch generate":
		text = "Usage: %s patch generate [OPTIONS] --output <OUTPUT> <SOURCE> <FILE> <EDITED>\n\nArguments:\n  <SOURCE>  \n  <FILE>    \n  <EDITED>  \n\nOptions:\n" + manifestHelp + "      --output <OUTPUT>      \n" + helpOption
	default:
		return ""
	}
	return fmt.Sprintf(text, name)
}
