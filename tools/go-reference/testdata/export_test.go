package htmldate

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"regexp"
	"regexp/syntax"
	"sort"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/go-shiori/dom"
)

type rustOptions struct {
	Original bool   `json:"original"`
	Fast     bool   `json:"fast"`
	Time     bool   `json:"time"`
	URL      string `json:"url,omitempty"`
	Defer    bool   `json:"defer,omitempty"`
	Min      string `json:"min"`
	Max      string `json:"max"`
}

type rustExpected struct {
	Date     string `json:"date"`
	Unix     int64  `json:"unix"`
	Nano     int    `json:"nano"`
	Offset   int    `json:"offset"`
	Zone     string `json:"zone"`
	Time     bool   `json:"time"`
	Timezone bool   `json:"timezone"`
	Source   string `json:"source"`
	Error    string `json:"error"`
}

type rustCase struct {
	Kind     string       `json:"kind"`
	Input    string       `json:"input"`
	File     string       `json:"file,omitempty"`
	SHA256   string       `json:"sha256,omitempty"`
	Options  rustOptions  `json:"options"`
	Expected rustExpected `json:"expected"`
}

func TestRustExport(t *testing.T) {
	root := os.Getenv("RUST_HTMLDATE_ROOT")
	if root == "" {
		t.Fatal("missing destination")
	}
	rulesOnly := os.Getenv("RUST_HTMLDATE_RULES_ONLY") == "true"
	write := func(path string, value any) {
		encoded, err := json.MarshalIndent(value, "", "  ")
		if err != nil {
			t.Fatal(err)
		}
		path = filepath.Join(root, path)
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, append(encoded, '\n'), 0644); err != nil {
			t.Fatal(err)
		}
	}
	license, err := os.ReadFile("LICENSE")
	if err != nil {
		t.Fatal(err)
	}
	if old, err := os.ReadFile(filepath.Join(root, "LICENSE")); err == nil && string(old) != string(license) {
		t.Fatal("refusing to overwrite a different license")
	}
	if err := os.WriteFile(filepath.Join(root, "LICENSE"), license, 0644); err != nil {
		t.Fatal(err)
	}
	hashes := map[string]string{}
	for _, pattern := range []string{"*.go", "internal/selector/*.go", "internal/re2go/*.re", "go.mod", "go.sum", "LICENSE"} {
		paths, err := filepath.Glob(pattern)
		if err != nil {
			t.Fatal(err)
		}
		for _, path := range paths {
			if rulesOnly && filepath.Base(path) == "rust_export_test.go" {
				continue
			}
			data, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if rulesOnly && path != "LICENSE" {
				data = []byte(strings.ReplaceAll(string(data), "\r\n", "\n"))
			}
			hashes[filepath.ToSlash(path)] = fmt.Sprintf("%x", sha256.Sum256(data))
		}
	}
	patterns := map[string]*regexp.Regexp{
		"ymd_no_sep": rxYmdNoSepPattern, "ymd": rxYmdPattern, "ym": rxYmPattern,
		"complete_url": rxCompleteUrl, "text_date": rxTextDatePattern, "discard": rxDiscardPattern,
		"year": rxYearPattern, "three_catch": rxThreeCatch, "three_loose_catch": rxThreeLooseCatch,
		"select_ymd_year": rxSelectYmdYear, "ymd_year": rxYmdYear, "date_strings_catch": rxDateStringsCatch,
		"slashes_year": rxSlashesYear, "yyyy_mm_catch": rxYyyyMmCatch, "mm_yyyy_year": rxMmYyyyYear,
		"w3_cleaner": rxSimpleW3Cleaner, "common_time": rxCommonTime, "tz_code": rxTzCode, "iso_time": rxIsoTime,
	}
	rustPatterns := map[string]string{}
	for name, pattern := range patterns {
		expression, err := syntax.Parse(pattern.String(), syntax.Perl)
		if err != nil {
			t.Fatal(err)
		}
		var unnamed func(*syntax.Regexp)
		unnamed = func(expression *syntax.Regexp) {
			expression.Name = ""
			for _, child := range expression.Sub {
				unnamed(child)
			}
		}
		unnamed(expression)
		rustPatterns[name] = expression.String()
	}
	keys := func(values map[string]struct{}) []string {
		result := make([]string, 0, len(values))
		for key := range values {
			result = append(result, key)
		}
		sort.Strings(result)
		return result
	}
	sourceCommit := "a83e1a91e8e4a006f3d8b9f97328db09ba19751e"
	if rulesOnly {
		sourceCommit = "0f04a39fb476a75744ed948bfcd887306ca3f187"
	}
	write("data/rules.json", map[string]any{"source_commit": sourceCommit, "source_sha256": hashes, "patterns": rustPatterns, "months": monthNumber, "timezones": mapTimezoneNames, "date_attributes": keys(dateAttributes), "modified_properties": keys(propertyModified), "modified_names": keys(attrModifiedNames)})
	if rulesOnly {
		return
	}
	frozen := time.Date(2026, 9, 13, 12, 0, 0, 0, time.UTC)
	externalDpsConfig.CurrentTime = frozen
	minimum := time.Date(1995, 1, 1, 0, 0, 0, 0, time.UTC)
	maximum := time.Date(2026, 9, 13, 23, 59, 59, 999999999, time.UTC)
	result := func(date time.Time, source string, hasTime, hasTimezone bool, err error) rustExpected {
		expected := rustExpected{Source: source, Time: hasTime, Timezone: hasTimezone}
		if err != nil {
			expected.Error = err.Error()
		}
		if !date.IsZero() {
			expected.Date = date.Format("2006-01-02T15:04:05.999999999Z07:00")
			expected.Unix = date.Unix()
			expected.Nano = date.Nanosecond()
			_, expected.Offset = date.Zone()
			expected.Zone = date.Location().String()
		}
		return expected
	}
	var cases []rustCase
	add := func(kind, input, file, hash string, opts Options) {
		entry := rustCase{Kind: kind, Input: input, File: file, SHA256: hash, Options: rustOptions{Original: opts.UseOriginalDate, Fast: opts.SkipExtensiveSearch, Time: opts.ExtractTime, URL: opts.URL, Defer: opts.DeferUrlExtractor, Min: opts.MinDate.Format(time.RFC3339Nano), Max: opts.MaxDate.Format(time.RFC3339Nano)}}
		var source string
		var date time.Time
		switch kind {
		case "fast":
			date = fastParse(input, opts)
		case "regex":
			date = regexParse(input, opts)
		case "url":
			date = extractUrlDate(input, opts)
		case "try":
			source, date = tryDateExpr(input, opts)
		case "html", "file":
			text := input
			if file != "" {
				data, err := os.ReadFile(file)
				if err != nil {
					t.Fatal(err)
				}
				text = string(data)
			}
			actual, err := FromReader(strings.NewReader(text), opts)
			entry.Expected = result(actual.DateTime, actual.SrcString, actual.HasTime, actual.HasTimezone, err)
			cases = append(cases, entry)
			return
		case "time":
			hour, minute, second, zone, found := findTime(input)
			if zone == nil {
				zone = time.UTC
			}
			date = time.Date(2020, 1, 1, hour, minute, second, 0, zone)
			_, _, _, detected, _ := findTime(input)
			entry.Expected = result(date, "", found, detected != nil, nil)
			cases = append(cases, entry)
			return
		}
		entry.Expected = result(date, source, false, false, nil)
		cases = append(cases, entry)
	}
	literals := map[string]bool{}
	for _, path := range []string{"core_test.go", "extractors_test.go", "validators_test.go", "timezone_test.go", "internal/re2go/re2go_test.go"} {
		file, err := parser.ParseFile(token.NewFileSet(), path, nil, 0)
		if err != nil {
			t.Fatal(err)
		}
		ast.Inspect(file, func(node ast.Node) bool {
			literal, ok := node.(*ast.BasicLit)
			if !ok || literal.Kind != token.STRING {
				return true
			}
			text, err := strconv.Unquote(literal.Value)
			if err != nil {
				t.Fatal(err)
			}
			if len(text) <= 15000 && (strings.ContainsAny(text, "0123456789") || strings.Contains(text, "<")) {
				literals[text] = true
			}
			return true
		})
	}
	inputs := make([]string, 0, len(literals))
	for text := range literals {
		inputs = append(inputs, text)
	}
	sort.Strings(inputs)
	for _, input := range inputs {
		for _, fast := range []bool{false, true} {
			opts := Options{MinDate: minimum, MaxDate: maximum, SkipExtensiveSearch: fast}
			if strings.Contains(input, "<") {
				for _, original := range []bool{false, true} {
					for _, withTime := range []bool{false, true} {
						opts.UseOriginalDate, opts.ExtractTime = original, withTime
						add("html", input, "", "", opts)
					}
				}
			} else {
				add("try", input, "", "", opts)
				if !fast {
					for _, kind := range []string{"fast", "regex", "url", "time"} {
						add(kind, input, "", "", opts)
					}
				}
			}
		}
	}
	write("testdata/go-reference.json", map[string]any{"source_commit": "a83e1a91e8e4a006f3d8b9f97328db09ba19751e", "dateparser": "v1.4.5", "current_time": frozen.Format(time.RFC3339Nano), "cases": cases})
	t.Logf("exported %d helper/HTML cases", len(cases))
	if os.Getenv("RUST_HTMLDATE_CORPUS") == "true" {
		cases = nil
		for _, folder := range []string{"comparison", "mediacloud", "mock"} {
			files, err := filepath.Glob(filepath.Join("test-files", folder, "*.html"))
			if err != nil {
				t.Fatal(err)
			}
			for _, file := range files {
				data, err := os.ReadFile(file)
				if err != nil {
					t.Fatal(err)
				}
				hash := fmt.Sprintf("%x", sha256.Sum256(data))
				for _, original := range []bool{false, true} {
					for _, fast := range []bool{false, true} {
						add("file", "", filepath.ToSlash(file), hash, Options{MinDate: minimum, MaxDate: maximum, UseOriginalDate: original, SkipExtensiveSearch: fast})
					}
				}
			}
		}
		write("testdata/go-corpus.json", map[string]any{"source_commit": "a83e1a91e8e4a006f3d8b9f97328db09ba19751e", "current_time": frozen.Format(time.RFC3339Nano), "cases": cases})
		t.Logf("exported %d saved-page cases", len(cases))
	}
	_ = dom.TextContent
}

func TestRustInspect(t *testing.T) {
	for _, input := range []string{
		`<a rel="license" href="/license" class="copyright">Copyright 2020</a>`,
		`<div rel="license" href="/license" class="copyright">Copyright 2020</div>`,
		"\ufeff<!--before--><html><body id='news20190624'>content</body></html>",
	} {
		document, err := dom.Parse(strings.NewReader(input))
		if err != nil {
			t.Fatal(err)
		}
		t.Logf("DOM %q -> %q", input, dom.InnerHTML(document))
	}
	for _, file := range []string{"test-files/comparison/chicagotribune.com-Biden.html", "test-files/mediacloud/1727473717.html"} {
		data, err := os.ReadFile(file)
		if err != nil {
			t.Fatal(err)
		}
		counts := map[string]int{}
		for iteration := 0; iteration < 40; iteration++ {
			actual, err := FromReader(strings.NewReader(string(data)), Options{UseOriginalDate: true, SkipExtensiveSearch: true})
			if err != nil {
				t.Fatal(err)
			}
			counts[actual.SrcString]++
		}
		t.Logf("JSON %s: %v", file, counts)
	}
}
