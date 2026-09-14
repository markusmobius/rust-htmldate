use crate::rules::{limit, normalize, regex, rules};
use crate::{Options, Timezone};
use chrono::{
    DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike, Utc,
};
use chrono_tz::OffsetComponents;
use regex::Regex;
use rust_dateparser::{Configuration, Parser, ParserType, PreferredDateSource};
use rust_dateutil::{
    compat::datetime::{self, ParsedDateTime},
    lexer::{ascii_decimal, is_digit, is_digits},
    parser::{self as dateutil, LocalTimezone},
};
use std::sync::OnceLock;

#[derive(Clone)]
pub(crate) struct Environment {
    pub zone: Timezone,
    pub names: [String; 2],
    pub offsets: [i32; 2],
    pub parser_year: i32,
}

impl Environment {
    pub(crate) fn new(zone: Timezone) -> Self {
        let parser_year = Utc::now().with_timezone(&zone).year();
        let season = |month| {
            let wall = NaiveDate::from_ymd_opt(parser_year, month, 1)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap();
            let offset = zone.offset_from_utc_datetime(&wall);
            (offset.to_string(), offset.fix().local_minus_utc())
        };
        let (mut standard, mut daylight) = (season(1), season(7));
        if standard.1 > daylight.1 {
            std::mem::swap(&mut standard, &mut daylight);
        }
        Self {
            zone,
            names: [standard.0, daylight.0],
            offsets: [standard.1, daylight.1],
            parser_year,
        }
    }

    fn system() -> Self {
        let zone = std::env::var("TZ")
            .ok()
            .and_then(|name| name.trim_start_matches(':').parse().ok())
            .or_else(|| {
                iana_time_zone::get_timezone()
                    .ok()
                    .and_then(|name| name.parse().ok())
            })
            .unwrap_or(Timezone::Local);
        let environment = Self::new(zone);
        #[cfg(windows)]
        let environment = environment.with_windows_names();
        environment
    }

    #[cfg(windows)]
    fn with_windows_names(mut self) -> Self {
        use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};
        let snapshot = || -> std::io::Result<[String; 2]> {
            let machine = RegKey::predef(HKEY_LOCAL_MACHINE);
            let current =
                machine.open_subkey(r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation")?;
            let key: String = current.get_value("TimeZoneKeyName")?;
            let definitions = machine.open_subkey(format!(
                r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Time Zones\{key}"
            ))?;
            Ok([definitions.get_value("Std")?, definitions.get_value("Dlt")?])
        };
        if let Ok(names) = snapshot() {
            self.names = names;
        }
        self
    }

    fn is_dst(&self, instant: NaiveDateTime) -> bool {
        match &self.zone {
            Timezone::Iana(zone) => {
                zone.offset_from_utc_datetime(&instant).dst_offset() != Duration::zero()
            }
            _ => false,
        }
    }
}

#[cfg(test)]
thread_local! {
    static TEST_ENVIRONMENT: std::cell::RefCell<Option<Environment>> = const { std::cell::RefCell::new(None) };
}

fn with_environment<Result>(callback: impl FnOnce(&Environment) -> Result) -> Result {
    #[cfg(test)]
    if let Some(environment) = TEST_ENVIRONMENT.with(|value| value.borrow().clone()) {
        return callback(&environment);
    }
    static ENVIRONMENT: OnceLock<Environment> = OnceLock::new();
    callback(ENVIRONMENT.get_or_init(Environment::system))
}

pub(crate) fn default_bounds() -> (DateTime<Timezone>, DateTime<Timezone>) {
    with_environment(|environment| {
        let local = |wall: NaiveDateTime| {
            let seconds = datetime::timestamp(
                &ParsedDateTime {
                    time: wall.with_nanosecond(0).unwrap(),
                    offset: None,
                },
                &environment.zone,
                false,
            )
            .expect("local default bound is in Python's calendar range");
            environment
                .zone
                .timestamp_opt(seconds as i64, wall.nanosecond())
                .single()
                .unwrap()
        };
        let today = Utc::now().with_timezone(&environment.zone).date_naive();
        (
            local(
                NaiveDate::from_ymd_opt(1995, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            ),
            local(today.and_hms_micro_opt(23, 59, 59, 999_999).unwrap()),
        )
    })
}

#[cfg(test)]
pub(crate) fn in_environment<Result>(
    environment: Environment,
    callback: impl FnOnce() -> Result,
) -> Result {
    struct Restore(Option<Environment>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_ENVIRONMENT.with(|value| value.replace(self.0.take()));
        }
    }
    let _restore = Restore(TEST_ENVIRONMENT.with(|value| value.replace(Some(environment))));
    callback()
}

pub(crate) fn valid_date(date: &DateTime<Timezone>, options: &Options) -> bool {
    with_environment(|environment| {
        valid_parsed_date(
            &ParsedDateTime {
                time: date.naive_local(),
                offset: None,
            },
            options,
            false,
            &environment.zone,
        )
    })
}

pub(crate) fn reference_timestamp(date: &DateTime<Timezone>) -> Option<i64> {
    with_environment(|environment| {
        datetime::timestamp(
            &ParsedDateTime {
                time: date.naive_local(),
                offset: None,
            },
            &environment.zone,
            false,
        )
        .ok()
        .map(|timestamp| timestamp as i64)
    })
}

pub(crate) fn reference_date(timestamp: i64, options: &Options) -> Option<DateTime<Timezone>> {
    with_environment(|environment| {
        let local = environment.zone.timestamp_opt(timestamp, 0).single()?;
        date_from_parts(local.year(), local.month(), local.day(), options)
    })
}

fn valid_parsed_date(
    value: &ParsedDateTime,
    options: &Options,
    fold: bool,
    zone: &Timezone,
) -> bool {
    if value.time
        == NaiveDate::from_ymd_opt(1, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
    {
        return false;
    }
    let Ok(candidate) = datetime::timestamp(value, zone, fold) else {
        return false;
    };
    let bound = |date: &DateTime<Timezone>| {
        datetime::timestamp(
            &ParsedDateTime {
                time: date.naive_local(),
                offset: Some(Duration::seconds(i64::from(
                    date.offset().fix().local_minus_utc(),
                ))),
            },
            zone,
            false,
        )
    };
    options.min_date.as_ref().is_none_or(|minimum| {
        value.time.year() >= minimum.year()
            && bound(minimum).is_ok_and(|minimum| candidate >= minimum)
    }) && options.max_date.as_ref().is_none_or(|maximum| {
        value.time.year() <= maximum.year()
            && bound(maximum).is_ok_and(|maximum| candidate <= maximum)
    })
}

pub(crate) fn date_from_parts(
    year: i32,
    month: u32,
    day: u32,
    options: &Options,
) -> Option<DateTime<Timezone>> {
    if !(1..=9999).contains(&year) {
        return None;
    }
    let day = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(0, 0, 0)?;
    let date = Timezone::Utc.from_utc_datetime(&day);
    valid_date(&date, options).then_some(date)
}

pub(crate) fn correct_year(year: i32) -> i32 {
    if year < 100 {
        year + if year >= 90 { 1900 } else { 2000 }
    } else {
        year
    }
}

pub(crate) fn url_date(url: &str, options: &Options) -> Option<DateTime<Timezone>> {
    let parts = regex("complete_url").captures(url)?;
    date_from_parts(
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
        parts[3].parse().ok()?,
        options,
    )
}

pub(crate) fn fast_parse(text: &str, options: &Options) -> Option<DateTime<Timezone>> {
    let compact = |text: &str| {
        let text = ascii_decimal(text)?;
        date_from_parts(
            text.get(..4)?.parse().ok()?,
            text.get(4..6)?.parse().ok()?,
            text.get(6..)?.parse().ok()?,
            options,
        )
    };
    let prefix = limit(text, 8);
    let year = limit(prefix, 4);
    if is_digits(year) {
        if is_digits(&prefix[year.len()..]) {
            if let Some(date) = compact(prefix) {
                return Some(date);
            }
        } else {
            match datetime::from_isoformat(text) {
                Ok(parsed) => {
                    if with_environment(|environment| {
                        valid_parsed_date(&parsed, options, false, &environment.zone)
                    }) {
                        return Some(
                            Timezone::Utc
                                .from_utc_datetime(&parsed.time.date().and_hms_opt(0, 0, 0)?),
                        );
                    }
                }
                Err(_) => {
                    if let Some(date) = dateutil_fallback(text, options) {
                        return Some(date);
                    }
                }
            }
        }
    }
    if let Some(parts) = regex("ymd_no_sep").captures(text) {
        if let Some(date) = compact(&parts[1]) {
            return Some(date);
        }
    }
    if let Some(parts) = regex("ymd").captures(text) {
        let candidate = if parts.get(1).is_some() {
            date_from_parts(
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
                parts[3].parse().ok()?,
                options,
            )
        } else {
            let mut day = parts[4].parse().ok()?;
            let mut month = parts[5].parse().ok()?;
            let year = correct_year(parts[6].parse().ok()?);
            swap_parts(&mut day, &mut month);
            date_from_parts(year, month, day, options)
        };
        if candidate.is_some() {
            return candidate;
        }
    }
    if let Some(parts) = regex("ym").captures(text) {
        let candidate = if parts.get(1).is_some() {
            date_from_parts(parts[1].parse().ok()?, parts[2].parse().ok()?, 1, options)
        } else {
            date_from_parts(parts[4].parse().ok()?, parts[3].parse().ok()?, 1, options)
        };
        if candidate.is_some() {
            return candidate;
        }
    }
    regex_parse(text, options)
}

pub(crate) fn regex_parse(text: &str, options: &Options) -> Option<DateTime<Timezone>> {
    static PATTERNS: OnceLock<[Regex; 2]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        let month = r"January?|February?|March|A[pvPV]ril|Ma[iyIY]|Jun[eiEI]|Jul[iyIY]|August|September|O[ckCK]tober|November|De[cszCSZ]ember|Jan|Feb|M[a\x{00e4}A\x{00c4}]r|Apr|Jun|Jul|Aug|Sep|O[ckCK]t|Nov|De[czCZ]|Januari|Februari|Maret|Mei|Agustus|J[\x{00e4}\x{00c4}]nner|Feber|M[\x{00e4}\x{00c4}]rz|janvier|f[\x{00e9}\x{00c9}]vrier|mars|juin|juillet|aout|septembre|octobre|novembre|d[\x{00e9}\x{00c9}]cembre|Ocak|[\x{015f}\x{015e}]ubat|Mart|Nisan|May[\x{0131}I]s|Haziran|Temmuz|A[\x{011f}\x{011e}]ustos|Eyl[\x{00fc}\x{00dc}]l|Ekim|Kas[\x{0131}I]m|Aral[\x{0131}I]k|Oca|[\x{015f}\x{015e}]ub|Mar|Nis|Haz|Tem|A[\x{011f}\x{011e}]u|Eyl|Eki|Kas|Ara";
        [
            Regex::new(&format!(r"(?i)({month})[\t\n\x0c\r ]([0-3]?[0-9])(?:st|nd|rd|th)?,?[\t\n\x0c\r ](199[0-9]|20[0-3][0-9])")).unwrap(),
            Regex::new(&format!(r"(?i)([0-3]?[0-9])(?:st|nd|rd|th|\.)?[\t\n\x0c\r ](?:of[\t\n\x0c\r ])?({month})[,.]?[\t\n\x0c\r ](199[0-9]|20[0-3][0-9])")).unwrap(),
        ]
    });
    let (index, parts) = patterns
        .iter()
        .enumerate()
        .filter_map(|(index, pattern)| pattern.captures(text).map(|parts| (index, parts)))
        .min_by_key(|(index, parts)| {
            (
                parts.get(0).unwrap().start(),
                std::cmp::Reverse(parts.get(0).unwrap().len()),
                *index,
            )
        })?;
    let month_text = &parts[if index == 0 { 1 } else { 2 }];
    let mut month = *rules().months.get(&month_text.to_lowercase())?;
    let mut day = parts[if index == 0 { 2 } else { 1 }].parse().ok()?;
    swap_parts(&mut day, &mut month);
    date_from_parts(correct_year(parts[3].parse().ok()?), month, day, options)
}

pub(crate) fn try_date(text: &str, options: &Options) -> (String, Option<DateTime<Timezone>>) {
    let text = normalize(text);
    let text = limit(&text, 52).to_owned();
    let digits = text
        .chars()
        .filter(|character| is_digit(*character))
        .count();
    if !(4..=18).contains(&digits) || regex("discard").is_match(&text) {
        return (text, None);
    }
    let mut parsed = fast_parse(&text, options);
    if parsed.is_none() && !options.skip_extensive_search && regex("text_date").is_match(&text) {
        parsed = external_parse(&text, options);
    }
    (text, parsed)
}

fn dateutil_fallback(text: &str, options: &Options) -> Option<DateTime<Timezone>> {
    with_environment(|environment| {
        let now = options
            .date_parser_config
            .as_ref()
            .and_then(|configuration| configuration.current_time.clone())
            .unwrap_or_else(|| Utc::now().with_timezone(&environment.zone));
        let naive = ParsedDateTime {
            time: now.naive_local(),
            offset: None,
        };
        let aware = ParsedDateTime {
            offset: Some(Duration::seconds(i64::from(
                now.offset().fix().local_minus_utc(),
            ))),
            ..naive
        };
        let fold = match (
            datetime::timestamp(&aware, &environment.zone, false),
            datetime::timestamp(&naive, &environment.zone, false),
        ) {
            (Ok(instant), Ok(first)) if instant != first => {
                datetime::timestamp(&naive, &environment.zone, true)
                    .is_ok_and(|second| instant == second)
            }
            _ => false,
        };
        let is_dst = |instant| environment.is_dst(instant);
        let local = LocalTimezone {
            standard_name: &environment.names[0],
            daylight_name: &environment.names[1],
            standard_offset: environment.offsets[0],
            daylight_offset: environment.offsets[1],
            is_dst: &is_dst,
        };
        let parsed = dateutil::parse_with_local_timezone(
            text,
            now.date_naive().and_hms_opt(0, 0, 0)?,
            environment.parser_year,
            &local,
            fold,
        )
        .ok()?;
        let candidate = ParsedDateTime {
            time: parsed.time,
            offset: parsed
                .offset
                .map(|offset| Duration::seconds(i64::from(offset))),
        };
        valid_parsed_date(&candidate, options, parsed.fold, &environment.zone).then(|| {
            Timezone::Utc.from_utc_datetime(&parsed.time.date().and_hms_opt(0, 0, 0).unwrap())
        })
    })
}

pub(crate) fn external_parse(text: &str, options: &Options) -> Option<DateTime<Timezone>> {
    static PARSER: OnceLock<Parser> = OnceLock::new();
    let parser = PARSER.get_or_init(|| {
        let mut parser = Parser::new();
        parser.parser_types = vec![ParserType::CustomFormat, ParserType::AbsoluteTime];
        parser
    });
    let default_config = Configuration {
        strict_parsing: true,
        preferred_date_source: PreferredDateSource::Past,
        ..Configuration::default()
    };
    let configuration = options
        .date_parser_config
        .as_ref()
        .unwrap_or(&default_config);
    let date = parser.parse(configuration, text, &[]).ok()?.time;
    let date = Timezone::Utc.from_utc_datetime(&date.date_naive().and_hms_opt(0, 0, 0)?);
    valid_date(&date, options).then_some(date)
}

pub(crate) fn swap_parts(day: &mut u32, month: &mut u32) {
    if *month > 12 && *day <= 12 {
        std::mem::swap(day, month);
    }
}

#[derive(Default)]
pub(crate) struct Clock {
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub timezone: Option<Timezone>,
    pub found: bool,
}

impl Clock {
    pub fn apply(&self, date: DateTime<Timezone>) -> DateTime<Timezone> {
        let date = if self.found {
            date + Duration::seconds(self.hour * 3600 + self.minute * 60 + self.second)
        } else {
            date
        };
        match &self.timezone {
            Some(timezone) => timezone
                .from_local_datetime(&date.naive_local().with_nanosecond(0).unwrap())
                .single()
                .unwrap(),
            None => date,
        }
    }
}

pub(crate) fn timezone_code(code: &str) -> Option<Timezone> {
    let code = code.to_uppercase();
    if code == "Z" {
        return Some(Timezone::Utc);
    }
    let parts = regex("tz_code").captures(&code)?;
    let hour: i32 = parts[2].parse().ok()?;
    let minute: i32 = parts
        .get(3)
        .and_then(|part| part.as_str().parse().ok())
        .unwrap_or(0);
    let offset = (hour * 3600 + minute * 60) * if &parts[1] == "-" { -1 } else { 1 };
    Timezone::fixed(code, offset).ok()
}

pub(crate) fn find_time(text: &str) -> Clock {
    let mut text = normalize(text);
    let mut clock = Clock::default();
    let number = |parts: &regex::Captures<'_>, index| {
        parts
            .get(index)
            .and_then(|part| part.as_str().parse().ok())
            .unwrap_or(0)
    };
    if let Some(parts) = regex("iso_time").captures(&text) {
        clock.hour = number(&parts, 1);
        clock.minute = number(&parts, 2);
        clock.second = number(&parts, 3);
        clock.timezone = timezone_code(&parts[4]);
        clock.found = true;
        text = regex("iso_time").replace_all(&text, " ").into_owned();
    }
    if clock.found && clock.timezone.is_some() {
        return clock;
    }
    if clock.timezone.is_none() {
        for matched in regex("tz_code").find_iter(&text) {
            if let Some(timezone) = timezone_code(matched.as_str()) {
                clock.timezone = Some(timezone);
                break;
            }
        }
        text = regex("tz_code").replace_all(&text, " ").into_owned();
    }
    if clock.timezone.is_none() {
        clock.timezone = text.split_whitespace().find_map(|word| {
            rules()
                .timezones
                .get(word)
                .and_then(|offset| Timezone::fixed(word, *offset).ok())
        });
    }
    if !clock.found {
        if let Some(parts) = regex("common_time").captures(&text) {
            clock.hour = number(&parts, 1);
            clock.minute = number(&parts, 2);
            clock.second = number(&parts, 3);
            let meridiem = parts
                .get(4)
                .map_or("", |part| part.as_str())
                .to_lowercase()
                .replace('.', "");
            if meridiem == "pm" {
                clock.hour += 12;
            }
            clock.found = true;
        }
    }
    clock
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_environment_preserves_configured_offsets() {
        let actual = Environment::system();
        let expected = Environment::new(actual.zone.clone());
        assert_eq!(actual.offsets, expected.offsets);
    }

    #[test]
    fn python_dateutil_defaults() {
        in_environment(Environment::new(Timezone::Utc), || {
            let options = Options {
                date_parser_config: Some(Configuration {
                    current_time: Some(
                        Timezone::Utc
                            .with_ymd_and_hms(2026, 9, 13, 12, 0, 0)
                            .unwrap(),
                    ),
                    ..Configuration::default()
                }),
                ..Options::default()
            };
            for (input, expected) in [
                ("01", "2026-09-01"),
                ("1998", "1998-09-13"),
                ("1998-01", "1998-01-13"),
                ("2020.01", "2020-09-13"),
                ("2020.13", "2020-09-13"),
            ] {
                assert_eq!(
                    fast_parse(input, &options)
                        .map(|date| date.format("%F").to_string())
                        .as_deref(),
                    Some(expected),
                    "{input}"
                );
            }
        });
    }

    #[test]
    fn python_iso_week_shortcut() {
        let options = Options {
            min_date: Some(Timezone::Utc.with_ymd_and_hms(1995, 1, 1, 0, 0, 0).unwrap()),
            max_date: Some(Timezone::Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap()),
            ..Options::default()
        };
        for (input, expected) in [
            ("2020-W53-7", "2021-01-03"),
            ("2020W537", "2021-01-03"),
            ("2020-W53", "2020-12-28"),
            ("2017123", "2017-12-03"),
        ] {
            let actual = fast_parse(input, &options).map(|date| date.format("%F").to_string());
            assert_eq!(actual.as_deref(), Some(expected), "{input}");
        }
    }

    #[test]
    fn calendar_validation_rejects_rollover_and_respects_inclusive_bounds() {
        let minimum = Timezone::Utc
            .with_ymd_and_hms(2020, 2, 29, 0, 0, 0)
            .unwrap();
        let options = Options {
            min_date: Some(minimum.clone()),
            max_date: Some(minimum.clone()),
            ..Options::default()
        };
        assert_eq!(date_from_parts(2020, 2, 29, &options), Some(minimum));
        assert!(date_from_parts(2020, 2, 28, &options).is_none());
        assert!(date_from_parts(2020, 3, 1, &options).is_none());
        for (year, month, day) in [
            (0, 12, 31),
            (2019, 2, 29),
            (2020, 2, 30),
            (2020, 4, 31),
            (2020, 13, 1),
            (2020, 1, 0),
        ] {
            assert!(date_from_parts(year, month, day, &Options::default()).is_none());
        }
        let resolved = Options::default().with_defaults();
        assert_eq!(
            resolved.min_date.unwrap().to_rfc3339(),
            "1995-01-01T00:00:00+00:00"
        );
        assert!(Options::default().min_date.is_none());
    }

    #[test]
    fn url_extraction_preserves_go_pattern_and_year_rules() {
        let options = Options::default();
        assert_eq!(
            url_date("https://example.com/2020/2/29/article", &options)
                .unwrap()
                .format("%F")
                .to_string(),
            "2020-02-29"
        );
        for text in [
            "2020/02/29",
            "https://example.com/2040/01/01",
            "https://example.com/2019/02/29",
            "/2020/13/01/2020/01/01",
        ] {
            assert!(url_date(text, &options).is_none(), "{text}");
        }
        assert_eq!(correct_year(89), 2089);
        assert_eq!(correct_year(90), 1990);
        assert_eq!(correct_year(2020), 2020);
    }
}
