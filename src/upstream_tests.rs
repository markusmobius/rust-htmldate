use crate::{dates, Options, Timezone};
use chrono::{DateTime, Offset};
use serde::Deserialize;

#[derive(Deserialize)]
struct PythonFixture {
    python: String,
    source_commit: String,
    current_time: String,
    dependencies: std::collections::BTreeMap<String, String>,
    corpus_sha256_lf: std::collections::BTreeMap<String, String>,
    timezone_context: PythonEnvironment,
    cases: Vec<PythonCase>,
}

#[derive(Deserialize)]
struct PythonEnvironment {
    zone: String,
    names: [String; 2],
    offsets: [i32; 2],
    parser_year: i32,
}

impl PythonEnvironment {
    fn environment(&self) -> dates::Environment {
        dates::Environment {
            zone: self.zone.parse().unwrap(),
            names: self.names.clone(),
            offsets: self.offsets,
            parser_year: self.parser_year,
        }
    }
}

#[derive(Deserialize)]
struct PythonCase {
    kind: String,
    input: String,
    file: String,
    #[serde(default)]
    current_time: String,
    environment: Option<PythonEnvironment>,
    options: PythonOptions,
    expected: PythonExpected,
}

#[derive(Deserialize)]
struct PythonOptions {
    original: bool,
    fast: bool,
    #[serde(default)]
    url: String,
    #[serde(default)]
    defer: bool,
    min: String,
    max: String,
}

#[derive(Deserialize)]
struct PythonExpected {
    date: String,
    error: String,
}

fn python_datetime(text: &str) -> DateTime<Timezone> {
    use chrono::TimeZone;
    if let Ok(date) = DateTime::parse_from_rfc3339(text) {
        let zone =
            Timezone::fixed(date.offset().to_string(), date.offset().local_minus_utc()).unwrap();
        date.with_timezone(&zone)
    } else {
        Timezone::Utc.from_utc_datetime(
            &chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").unwrap(),
        )
    }
}

fn python_cases(kinds: &[&str]) {
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, io::Read};
    let encoded = include_str!("../testdata/python-reference.json");
    assert_eq!(
        format!("{:x}", Sha256::digest(encoded)),
        "17195764141ba7d8b16a51ad514d9913f70e79aa59efe109d68da852a06d7c93"
    );
    let fixture: PythonFixture = serde_json::from_str(encoded).unwrap();
    assert_eq!(fixture.python, "3.14.6");
    assert_eq!(
        fixture.source_commit,
        "b8952828329abaeeb3be21387b526f2be614ce67"
    );
    assert_eq!(fixture.dependencies["htmldate"], "1.10.0");
    assert_eq!(fixture.dependencies["dateparser"], "1.4.3");
    assert_eq!(fixture.dependencies["python-dateutil"], "2.9.0.post0");
    assert_eq!(fixture.cases.len(), 9614);
    let mut pages = BTreeMap::new();
    if kinds.contains(&"file") {
        let compressed = std::fs::File::open(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/python-pages.tar.gz"
        ))
        .unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(compressed));
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().replace('\\', "/");
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(&bytes)),
                fixture.corpus_sha256_lf[&name],
                "{name}"
            );
            assert!(pages.insert(name, bytes).is_none());
        }
        assert_eq!(pages.len(), fixture.corpus_sha256_lf.len());
    }
    let mut failures = Vec::new();
    let mut checked = 0;
    let mut exceptions = 0;
    for (index, case) in fixture
        .cases
        .iter()
        .enumerate()
        .filter(|(_, case)| kinds.contains(&case.kind.as_str()))
    {
        let now = if case.current_time.is_empty() {
            &fixture.current_time
        } else {
            &case.current_time
        };
        let options = Options {
            use_original_date: case.options.original,
            skip_extensive_search: case.options.fast,
            url: case.options.url.clone(),
            defer_url_extractor: case.options.defer,
            min_date: Some(python_datetime(&case.options.min)),
            max_date: Some(python_datetime(&case.options.max)),
            date_parser_config: Some(crate::DateParserConfiguration {
                current_time: Some(python_datetime(now)),
                strict_parsing: true,
                preferred_date_source: rust_dateparser::PreferredDateSource::Past,
                ..crate::DateParserConfiguration::default()
            }),
            ..Options::default()
        };
        let environment = case
            .environment
            .as_ref()
            .unwrap_or(&fixture.timezone_context)
            .environment();
        let date = dates::in_environment(environment, || match case.kind.as_str() {
            "fast" => dates::fast_parse(&case.input, &options),
            "regex" => dates::regex_parse(&case.input, &options),
            "try" => dates::try_date(&case.input, &options).1,
            "url" => dates::url_date(&case.input, &options),
            "html" | "file" => {
                let bytes = if case.kind == "file" {
                    pages[&case.file].as_slice()
                } else {
                    case.input.as_bytes()
                };
                let result = crate::from_reader(bytes, &options).unwrap();
                (!result.is_zero()).then_some(result.date_time)
            }
            _ => unreachable!(),
        });
        let actual = date
            .map(|date| date.format("%F").to_string())
            .unwrap_or_default();
        if actual != case.expected.date {
            failures.push(format!(
                "{index} {} {:?} {} (original={}, fast={}): {actual:?} != {:?}",
                case.kind,
                case.input,
                case.file,
                options.use_original_date,
                options.skip_extensive_search,
                case.expected.date
            ));
        }
        checked += 1;
        exceptions += usize::from(!case.expected.error.is_empty());
    }
    assert!(checked > 0);
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures
            .iter()
            .take(35)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!("Matched {checked} independent Python cases; {exceptions} Python exceptions checked for no date");
}

#[test]
fn python_date_helpers() {
    python_cases(&["fast", "regex", "try", "url"]);
}

#[test]
fn python_html() {
    python_cases(&["html"]);
}

#[test]
#[ignore = "requires testdata/python-pages.tar.gz from the GitHub source release"]
fn python_saved_pages() {
    python_cases(&["file"]);
}

#[test]
fn python_fixture_retains_all_historical_inputs() {
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    let python: Value =
        serde_json::from_str(include_str!("../testdata/python-reference.json")).unwrap();
    let identity = |case: &Value| {
        serde_json::to_string(&json!([
            case["kind"],
            case["input"],
            case["file"].as_str().unwrap_or(""),
            case["sha256"].as_str().unwrap_or(""),
            case["options"],
        ]))
        .unwrap()
    };
    let mut expected = BTreeMap::new();
    let mut actual = BTreeMap::new();
    for encoded in [
        include_str!("../testdata/go-reference.json"),
        include_str!("../testdata/go-corpus.json"),
    ] {
        let historical: Value = serde_json::from_str(encoded).unwrap();
        assert_eq!(
            historical["source_commit"],
            "a83e1a91e8e4a006f3d8b9f97328db09ba19751e"
        );
        for case in historical["cases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|case| case["kind"] != "time")
        {
            *expected.entry(identity(case)).or_insert(0_usize) += 1;
        }
    }
    for case in python["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case.get("audit_id").is_none())
    {
        *actual.entry(identity(case)).or_insert(0_usize) += 1;
    }
    assert_eq!(expected.values().sum::<usize>(), 9533);
    assert_eq!(expected, actual);
}

#[derive(Deserialize)]
struct Fixture {
    source_commit: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    kind: String,
    input: String,
    expected: Expected,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
struct Expected {
    date: String,
    unix: i64,
    nano: u32,
    offset: i32,
    zone: String,
    time: bool,
    timezone: bool,
    source: String,
    error: String,
}

fn datetime(value: &str) -> DateTime<Timezone> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Timezone::Utc)
}

fn expected(date: Option<DateTime<Timezone>>, source: String) -> Expected {
    let mut value = Expected {
        source,
        ..Expected::default()
    };
    if let Some(date) = date {
        value.date = date.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
        value.unix = date.timestamp();
        value.nano = date.timestamp_subsec_nanos();
        value.offset = date.offset().fix().local_minus_utc();
        value.zone = match date.timezone() {
            Timezone::Utc => "UTC".into(),
            Timezone::Local => "Local".into(),
            Timezone::Fixed { name, .. } => name.to_string(),
            Timezone::Iana(zone) => zone.to_string(),
        };
    }
    value
}

#[test]
fn go_time_helpers() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("../testdata/go-reference.json")).unwrap();
    assert_eq!(
        fixture.source_commit,
        "a83e1a91e8e4a006f3d8b9f97328db09ba19751e"
    );
    let mut failures = Vec::new();
    let mut checked = 0;
    for (index, case) in fixture
        .cases
        .iter()
        .enumerate()
        .filter(|(_, case)| case.kind == "time")
    {
        checked += 1;
        let clock = dates::find_time(&case.input);
        let mut actual = expected(
            Some(clock.apply(datetime("2020-01-01T00:00:00Z"))),
            String::new(),
        );
        actual.time = clock.found;
        actual.timezone = clock.timezone.is_some();
        if actual != case.expected {
            failures.push(format!(
                "{index} {:?}: {actual:?} != {:?}",
                case.input, case.expected
            ));
        }
    }
    assert_eq!(checked, 929);
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    println!("Matched {checked} pinned Go time helper cases");
}
