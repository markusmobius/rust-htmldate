#![forbid(unsafe_code)]

use chrono::DateTime;
use rust_htmldate::{DateParserConfiguration, Document, ExtractionResult, Options, Timezone};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Deserialize, Serialize)]
struct Fixture {
    file: String,
    bytes: usize,
    sha256: String,
}

struct Page {
    content: Vec<u8>,
    document: Document,
}

#[derive(Deserialize)]
struct Request {
    cohort: String,
    warmup: bool,
    #[serde(default)]
    accuracy: bool,
}

#[derive(Serialize)]
struct Outcome {
    file: String,
    date: String,
    error: String,
}

fn datetime(text: &str) -> DateTime<Timezone> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Timezone::Utc)
}

fn emit(output: &mut impl Write, value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.len() != 3 || std::env::var("TZ").as_deref() != Ok("UTC") {
        return Err("use benchmark <corpus-root> <fixture-manifest> with TZ=UTC".into());
    }
    let root = PathBuf::from(&arguments[1]);
    let fixtures: Vec<Fixture> = serde_json::from_slice(&std::fs::read(&arguments[2])?)?;
    if fixtures.len() != 1000 {
        return Err("benchmark requires all 1000 corpus entries".into());
    }
    let mut pages = Vec::with_capacity(fixtures.len());
    for fixture in &fixtures {
        let raw = std::fs::read(root.join(&fixture.file))?;
        let content: Vec<u8> = raw
            .iter()
            .enumerate()
            .filter_map(|(index, &byte)| {
                (byte != b'\r' || raw.get(index + 1) != Some(&b'\n')).then_some(byte)
            })
            .collect();
        if content.len() != fixture.bytes
            || format!("{:x}", Sha256::digest(&content)) != fixture.sha256
        {
            return Err(format!("corpus bytes changed: {}", fixture.file).into());
        }
        let document = Document::parse(&String::from_utf8_lossy(&content));
        pages.push(Page { content, document });
    }
    let corpus_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&fixtures)?));
    let corpus_bytes: usize = fixtures.iter().map(|fixture| fixture.bytes).sum();
    let mut output = io::BufWriter::new(io::stdout().lock());
    emit(
        &mut output,
        &json!({
            "metadata": {"cases": fixtures.len(), "corpus_bytes": corpus_bytes, "corpus_sha256": corpus_hash},
            "fixtures": fixtures,
        }),
    )?;
    let mut options = Options {
        min_date: Some(datetime("1995-01-01T00:00:00Z")),
        max_date: Some(datetime("2026-09-13T23:59:59.999999999Z")),
        date_parser_config: Some(DateParserConfiguration {
            current_time: Some(datetime("2026-09-13T12:00:00Z")),
            strict_parsing: true,
            preferred_date_source: rust_dateparser::PreferredDateSource::Past,
            ..DateParserConfiguration::default()
        }),
        ..Options::default()
    };
    let zero = ExtractionResult::default().date_time;
    let mut attempts = vec![zero.clone(); pages.len()];
    let mut expected = BTreeMap::new();
    for line in io::stdin().lock().lines() {
        let request: Request = serde_json::from_str(&line?)?;
        let (original, fast) = match request.cohort.as_str() {
            "document-original-fast" => (true, true),
            "document-original-extensive" => (true, false),
            "document-modified-fast" => (false, true),
            "document-modified-extensive" => (false, false),
            _ => return Err(format!("unknown cohort: {}", request.cohort).into()),
        };
        if !request.warmup && !expected.contains_key(&request.cohort) {
            return Err("warm up the mode before timing".into());
        }
        if request.accuracy && !request.warmup {
            return Err("accuracy requests must not be timed".into());
        }
        options.use_original_date = original;
        options.skip_extensive_search = fast;
        let started = (!request.warmup).then(Instant::now);
        for (index, page) in pages.iter().enumerate() {
            let result = if request.accuracy {
                rust_htmldate::from_reader(page.content.as_slice(), &options)?
            } else {
                rust_htmldate::from_document(&page.document, &options)
            };
            attempts[index] = result.date_time;
        }
        let elapsed = started.map_or(0.0, |started| started.elapsed().as_secs_f64() * 1000.0);
        let outcomes: Vec<Outcome> = attempts
            .iter()
            .zip(&fixtures)
            .map(|(date, fixture)| Outcome {
                file: fixture.file.clone(),
                date: if date == &zero {
                    String::new()
                } else {
                    date.format("%F").to_string()
                },
                error: String::new(),
            })
            .collect();
        let result_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&outcomes)?));
        if !request.accuracy {
            if request.warmup {
                expected.insert(request.cohort.clone(), result_hash.clone());
            } else if expected[&request.cohort] != result_hash {
                return Err(format!("outputs changed in {}", request.cohort).into());
            }
        }
        let parsed = outcomes
            .iter()
            .filter(|outcome| !outcome.date.is_empty())
            .count();
        let results = request.warmup.then_some(outcomes);
        emit(
            &mut output,
            &json!({
                "metadata": {
                    "cohort": request.cohort, "cases": fixtures.len(), "parsed": parsed,
                    "corpus_bytes": corpus_bytes, "corpus_sha256": corpus_hash,
                    "results_sha256": result_hash, "current_time": "2026-09-13T12:00:00Z",
                    "min_date": "1995-01-01T00:00:00Z", "max_date": "2026-09-13T23:59:59.999999999Z",
                    "timezone": "UTC",
                },
                "warmup": request.warmup, "accuracy": request.accuracy,
                "pass_ms": elapsed, "results": results,
            }),
        )?;
    }
    Ok(())
}
