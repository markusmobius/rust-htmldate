#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

mod dates;
mod dom;
mod extract;
mod rules;
mod search;

#[cfg(test)]
mod upstream_tests;

use chrono::{DateTime, TimeZone};
pub use dom::{Document, TreeAttribute, TreeAttributeRef, TreeNode, TreeNodeRef, TreeSource};
pub use rust_dateparser::{Configuration as DateParserConfiguration, Timezone};
use std::io::{self, Read};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Default)]
pub struct Options {
    pub extract_time: bool,
    pub use_original_date: bool,
    pub url: String,
    pub min_date: Option<DateTime<Timezone>>,
    pub max_date: Option<DateTime<Timezone>>,
    pub enable_log: bool,
    pub skip_extensive_search: bool,
    pub defer_url_extractor: bool,
    pub date_parser_config: Option<DateParserConfiguration>,
}

impl Options {
    fn with_defaults(&self) -> Self {
        let mut options = self.clone();
        if options.min_date.is_none() || options.max_date.is_none() {
            let (minimum, maximum) = dates::default_bounds();
            options.min_date.get_or_insert(minimum);
            options.max_date.get_or_insert(maximum);
        }
        options
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionResult {
    pub date_time: DateTime<Timezone>,
    pub has_time: bool,
    pub has_timezone: bool,
    pub src_string: String,
}

impl Default for ExtractionResult {
    fn default() -> Self {
        Self {
            date_time: Timezone::Utc.with_ymd_and_hms(1, 1, 1, 0, 0, 0).unwrap(),
            has_time: false,
            has_timezone: false,
            src_string: String::new(),
        }
    }
}

impl ExtractionResult {
    pub fn is_zero(&self) -> bool {
        self.date_time.timestamp() == -62_135_596_800
            && self.date_time.timestamp_subsec_nanos() == 0
    }
}

pub fn from_html(html: &str, options: &Options) -> ExtractionResult {
    let html: String = html
        .nfd()
        .filter(|character| *character != '\u{00ad}')
        .nfc()
        .collect();
    let html = dom::repair_html(html);
    extract::run(std::borrow::Cow::Owned(Document::parse(&html)), options)
}

pub fn from_document(document: &Document, options: &Options) -> ExtractionResult {
    extract::run(std::borrow::Cow::Borrowed(document), options)
}

pub fn from_tree_source(source: &impl TreeSource, options: &Options) -> ExtractionResult {
    let document = Document::import_source(source, |text| text);
    extract::run(std::borrow::Cow::Owned(document), options)
}

pub fn from_reader(mut reader: impl Read, options: &Options) -> io::Result<ExtractionResult> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let encoding = if let Some((encoding, _)) = encoding_rs::Encoding::for_bom(&bytes) {
        encoding
    } else {
        let mut detector = chardetng::EncodingDetector::new();
        detector.feed(&bytes, true);
        detector.guess(None, true)
    };
    let (html, _) = encoding.decode_without_bom_handling(&bytes);
    Ok(from_html(&html, options))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Offset, Timelike};

    #[test]
    fn local_default_bounds_use_python_microseconds() {
        let zone: Timezone = "America/New_York".parse().unwrap();
        dates::in_environment(dates::Environment::new(zone.clone()), || {
            let options = Options::default();
            let resolved = options.with_defaults();
            let minimum = resolved.min_date.unwrap();
            let maximum = resolved.max_date.unwrap();
            assert_eq!(minimum.to_rfc3339(), "1995-01-01T00:00:00-05:00");
            assert_eq!(maximum.timezone(), zone);
            assert_eq!(
                (
                    maximum.hour(),
                    maximum.minute(),
                    maximum.second(),
                    maximum.nanosecond()
                ),
                (23, 59, 59, 999_999_000)
            );
            assert!(options.min_date.is_none() && options.max_date.is_none());
            let resolved = Options {
                min_date: Some(minimum.clone()),
                max_date: Some(minimum.clone()),
                ..options
            }
            .with_defaults();
            assert_eq!(resolved.max_date, Some(minimum));
        });
    }

    #[test]
    fn public_apis_preserve_documents_and_time_extension() {
        fn shareable<Value: Send + Sync>() {}
        shareable::<Options>();
        shareable::<ExtractionResult>();
        shareable::<Document>();
        let html = "<html><body><div id='wm-ipp'>2001-01-01</div><div class='date'><svg><text>1999-01-01</text></svg>2017-09-01</div></body></html>";
        let document = Document::parse(html);
        let original = document.clone();
        for skip_extensive_search in [false, true] {
            let options = Options {
                use_original_date: true,
                skip_extensive_search,
                ..Options::default()
            };
            let result = from_document(&document, &options);
            assert_eq!(result.date_time.format("%F").to_string(), "2017-09-01");
            assert_eq!(result, from_html(html, &options));
            assert_eq!(result, from_reader(html.as_bytes(), &options).unwrap());
            assert_eq!(document, original);
        }
        let html = r#"<html><head><meta property="article:published_time" content="2020-01-02T13:14:15+02:00"></head></html>"#;
        for extract_time in [false, true] {
            let result = from_html(
                html,
                &Options {
                    extract_time,
                    use_original_date: true,
                    ..Options::default()
                },
            );
            assert_eq!(result.date_time.format("%F").to_string(), "2020-01-02");
            assert_eq!(result.has_time, extract_time);
            assert_eq!(result.has_timezone, extract_time);
            assert_eq!(result.date_time.hour(), if extract_time { 13 } else { 0 });
            assert_eq!(
                result.date_time.offset().fix().local_minus_utc(),
                if extract_time { 7200 } else { 0 }
            );
            assert_eq!(result.src_string, "2020-01-02T13:14:15+02:00");
        }
    }

    #[test]
    fn reader_errors_are_returned() {
        struct FailedReader;
        impl Read for FailedReader {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("reader failed"))
            }
        }
        assert_eq!(
            from_reader(FailedReader, &Options::default())
                .unwrap_err()
                .to_string(),
            "reader failed"
        );
        assert!(from_reader(&b""[..], &Options::default())
            .unwrap()
            .is_zero());
    }
}
