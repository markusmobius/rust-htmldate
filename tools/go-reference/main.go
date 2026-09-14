package main

import (
	"bytes"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

const referenceCommit = "a83e1a91e8e4a006f3d8b9f97328db09ba19751e"

func main() {
	source := flag.String("source", "../../../go-htmldate", "pinned Go main checkout")
	gitExecutable := flag.String("git", "git", "Git executable, optionally native Windows Git for a WSL-mounted checkout")
	corpus := flag.Bool("corpus", false, "also export saved-page outcomes without copying HTML")
	currentRules := flag.Bool("current-rules", false, "export only rules from the Python-qualified Go review candidate")
	flag.Parse()
	reference := referenceCommit
	dateparserVersion := "v1.4.5"
	if *currentRules {
		reference = "0f04a39fb476a75744ed948bfcd887306ca3f187"
		dateparserVersion = "v1.4.7"
		if *corpus {
			panic("current-rules cannot rewrite historical corpus results")
		}
	}
	root, err := os.Getwd()
	check(err)
	goRoot, err := filepath.Abs(*source)
	check(err)
	command := func(name string, args ...string) []byte {
		if name == "git" {
			name = *gitExecutable
		}
		cmd := exec.Command(name, args...)
		cmd.Dir = goRoot
		output, err := cmd.CombinedOutput()
		if err != nil {
			panic(fmt.Sprintf("%s: %s: %v", name, output, err))
		}
		return output
	}
	if strings.TrimSpace(string(command("git", "rev-parse", "HEAD"))) != reference {
		panic("Go checkout is not the pinned branch commit")
	}
	command("git", "diff", "--exit-code", "HEAD", "--", "*.go", "go.mod", "go.sum", "internal", "test-files")
	if strings.TrimSpace(string(command("go", "version"))) != "go version go1.27.1 linux/amd64" {
		panic("run this exporter in Linux/WSL with Go 1.27.1")
	}
	modules := json.NewDecoder(bytes.NewReader(command("go", "list", "-m", "-json", "all")))
	wanted := map[string]string{"github.com/markusmobius/go-dateparser": dateparserVersion, "golang.org/x/text": "v0.42.0"}
	if *currentRules {
		wanted["github.com/markusmobius/go-dateutil/v2"] = "v2.9.1"
	}
	for modules.More() {
		var module struct {
			Path, Version string
			Replace       *json.RawMessage
		}
		check(modules.Decode(&module))
		if version, ok := wanted[module.Path]; ok {
			if module.Version != version || module.Replace != nil {
				panic("unexpected dependency: " + module.Path)
			}
			delete(wanted, module.Path)
		}
	}
	if len(wanted) != 0 {
		panic("missing reference dependencies")
	}
	check(os.MkdirAll(filepath.Join(root, "target"), 0755))
	overlay := map[string]any{"Replace": map[string]string{filepath.Join(goRoot, "rust_export_test.go"): filepath.Join(root, "tools", "go-reference", "testdata", "export_test.go")}}
	encoded, err := json.Marshal(overlay)
	check(err)
	overlayPath := filepath.Join(root, "target", "go-overlay.json")
	check(os.WriteFile(overlayPath, encoded, 0644))
	cmd := exec.Command("go", "test", "-mod=readonly", "-overlay", overlayPath, "-run", "^TestRustExport$", "-count=1", "-timeout", "10m", "-v", ".")
	cmd.Dir = goRoot
	cmd.Env = append(os.Environ(), "RUST_HTMLDATE_ROOT="+root, fmt.Sprintf("RUST_HTMLDATE_CORPUS=%t", *corpus), fmt.Sprintf("RUST_HTMLDATE_RULES_ONLY=%t", *currentRules))
	cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
	check(cmd.Run())
}

func check(err error) {
	if err != nil {
		panic(err)
	}
}
