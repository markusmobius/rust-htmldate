# RustHtmlDate

Native Rust publication and modification date extraction, following
[Python htmldate](https://github.com/adbar/htmldate) through the Python-qualified
[Go-HtmlDate](https://github.com/markusmobius/go-htmldate) implementation.

**v1.10.1 review candidate, not tagged or released.** This is a Git source
distribution; there is no crates.io publication. Python decides date behavior.
The implementation runs on the caller's thread, with no runtime Go/Python bridge,
unsafe Rust, internal worker threads, or parallel batch processing.

| Component | Reference |
| --- | --- |
| Go-HtmlDate review | `0f04a39fb476a75744ed948bfcd887306ca3f187` |
| Go DateParser / Dateutil | Published v1.4.7 / v2.9.1 |
| Python htmldate | v1.10.0, `b8952828329abaeeb3be21387b526f2be614ce67` |
| Python dateparser | v1.4.3 |
| CPython / python-dateutil | 3.14.6 / 2.9.0.post0 |
| Rust DateParser | Published v1.4.7, `1e3e2feccd8662113e4092ca87a5567e4e23bc3c` |
| Rust Dateutil | Published v2.9.1, `3a7537dd3a4e223756fa31fb1941a10da4c78b30` |
| Rust toolchain | 1.98.1 |

## Usage

For review, use the Git branch rather than a nonexistent release tag:

```toml
[dependencies]
rust-htmldate = { git = "https://github.com/markusmobius/rust-htmldate", branch = "main" }
```

```rust
use rust_htmldate::{from_html, Options};

let html = r#"<html><head>
	<meta property="article:published_time" content="2020-01-02T13:14:15+02:00">
	</head><body>Article</body></html>"#;
let options = Options {
	use_original_date: true,
	..Options::default()
};
let result = from_html(html, &options);
assert!(!result.is_zero());
assert_eq!(result.date_time.format("%F").to_string(), "2020-01-02");
```

`from_reader` accepts an `std::io::Read` and returns its I/O errors.
`from_document` borrows a parsed `Document` without changing it, including when
fallback extraction prunes nodes. The owned `Document`, `Options`, and
`ExtractionResult` types are `Send + Sync`; callers own any concurrency.

By default, extraction searches extensively and prefers a modification date.
Set `use_original_date` for publication dates, `skip_extensive_search` for fast
mode, and `url` / `defer_url_extractor` to control URL precedence. No URL is
fetched by this library.

Without `extract_time`, successful results carry the selected wall-calendar date
at UTC midnight. The optional Go-derived time extension also fills `has_time`,
`has_timezone`, and the selected time/offset; Python htmldate has no corresponding
time-extraction API. `src_string` is the selected native source fragment, not a
Python return value. `is_zero()` indicates no date.

## Date Semantics

The fast path follows Python's Unicode character slices and compact-date gate.
Otherwise it calls shared `rust_dateutil::compat::datetime::from_isoformat`, then
the shared Dateutil parser only if ISO parsing failed. A parsed ISO value outside
the bounds continues to the later regex stages instead of invoking Dateutil.
There is no consumer-local substitute ISO or Dateutil parser.

Validation checks the candidate's wall year, then compares inclusive CPython-style
floating timestamps at microsecond precision. Aware input uses its offset; naive
input uses the local timezone. Output retains the selected wall date, not the UTC
date of its instant. External DateParser results are formatted to a date before
validation, following Python's custom/absolute strict/past configuration.

Unix references such as `abbr[data-utime]` are converted to their local calendar
date before validation. Text candidates in the same reference selection use
local-midnight timestamps, following Python in both directions.

Default bounds are local 1995-01-01 midnight through the current local day's
23:59:59.999999. Explicit `min_date` / `max_date` values are aware instants and
their submicrosecond precision is truncated. `date_parser_config.current_time`
can freeze incomplete Dateutil inputs as well as external DateParser input; it
does not freeze the default maximum bound.

The local timezone and Dateutil parser-year/name/offset snapshot are initialized
on first use. Set `TZ` before calling the library; changing process timezone
configuration afterward is unsupported. On Windows, native registry metadata
supplies the long Dateutil names; offsets remain those of the configured timezone.
The shared exact-context API preserves ambiguous-time folds. No Python parsing
caches are copied.

## Verification

Normal tests require neither Python nor a Go checkout. The independent fixture
contains **9,614 Python cases**: 992 fast, 929 regex, 1,876 try-expression, 929 URL,
848 synthetic HTML, and 4,040 saved-page cases. All 9,533 historical inputs remain
present with their options and raw source hashes; 81 audit cases cover ISO weeks,
Unicode, offset bounds, local-name snapshots, and folds. Four Python exceptions
are checked for a safe no-date result, not identical exception text.

The bundled compressed saved pages are replayed with CRLF normalized to LF and
verified against separate canonical hashes. Original raw hashes remain as
provenance. The saved pages are upstream test inputs containing third-party
content; this project does not relicense that content.

The original Go fixtures retain their `a83e1a9` / DateParser v1.4.5 provenance.
They now guard input inventory and 929 Go-only time cases, not obsolete Go date
quirks. Runtime rules were regenerated from the qualified `0f04a39` Go candidate.
No new benchmarks or performance claims accompany this candidate.

Another 24 Python-checked HTML regressions cover local Unix references, mixed
text/timestamp selection and date-only bounds in UTC, Eastern and Kolkata
contexts. These are additional unit cases, not changes to the 9,614-case fixture.
Native startup tests also ensure Windows name discovery preserves configured
offsets; CPython's effective offsets were checked in those three contexts.

```sh
cargo fmt --all -- --check
cargo test --locked
cargo test --locked --release
cargo clippy --locked --all-targets -- -D warnings
```

To independently re-execute Python, use CPython 3.14.6 and the exact versions in
`tools/python-requirements.txt`:

```sh
python -m pip install -r tools/python-requirements.txt
python tools/python-reference/verify.py --check
```

The verifier checks all eight pinned HtmlDate module hashes and all expected
results without rewriting the fixture. It uses fresh UTC/Eastern subprocesses
and clears caches between independent contexts. `--kind fast --kind try` checks
only those slices. The older `tools/python-reference.py` is a historical
comparison utility, not the current oracle.

The fixture importer requires the exact qualified Go commit and copies existing
Python evidence; it never derives expected dates from Go or Rust. The separate
Go exporter has `-current-rules` mode, which updates rule data only and leaves
historical result fixtures intact. Development tools are not runtime dependencies.

The Git-only DateParser/Dateutil dependencies currently prevent crates.io package
resolution. Verify the intended source distribution with a clean Git archive
and `cargo test --locked`; do not replace dependencies to claim registry packaging
support.

## API And Environment

Date selection follows the pinned Python algorithm for equivalent inputs and
options. A different selected date under an equivalent timezone context is a bug,
not a supported deviation. The tests establish agreement on their inputs, not a
proof for every possible HTML document or environment.

The API is native Rust, not a drop-in Python signature: it uses typed bounds,
a year-1 zero sentinel and a typed result that callers format themselves. It does
not reproduce Python's string-bound, arbitrary-output-format or input-object APIs,
and it does not fetch URLs. The optional time extraction is a Go-derived extension.
`enable_log` is retained for option compatibility and currently has no logging
side effects. These interface differences do not relax date-selection semantics.

Set timezone configuration before first use; changing it afterward is unsupported.
Comparisons with Python must use equivalent timezone names, rules, database
versions and supported timestamp ranges. Matching a zone name alone does not
establish that context. Native Windows discovery and localized names also require
platform checks; injected fixture contexts do not test discovery. The IANA
database is pinned through `chrono-tz` in the lockfile. HTML parsing and byte
decoding use native libraries, so untested inputs still need differential checks.