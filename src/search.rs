use std::{collections::HashMap, sync::OnceLock};

use chrono::Datelike;
use regex::bytes::{Regex, RegexBuilder};

use crate::{
    dates::{correct_year, date_from_parts, fast_parse, regex_parse},
    extract::Candidate,
    rules::{limit, regex},
    Options,
};

struct YearCandidate {
    pattern: String,
    count: usize,
    source: String,
}

fn scanner(name: &str) -> &'static Regex {
    static SCANNERS: OnceLock<HashMap<&'static str, Regex>> = OnceLock::new();
    &SCANNERS.get_or_init(|| [
        ("copyright", r"(?:\xC2\xA9|&copy;|Copyright|\(c\))[^0-9]*(?:199[0-9]|20[0-3][0-9])?-?(199[0-9]|20[0-3][0-9])[^0-9]"),
        ("three", r"/([0-9]{4}/[0-9]{2}/[0-9]{2})[01/]"),
        ("three_loose", r"[^0-9]([0-9]{4}[/.-][0-9]{2}[/.-][0-9]{2})[^0-9]"),
        ("select_ymd", r"[^0-9]([0-3]?[0-9][/.-][01]?[0-9][/.-][0-9]{4})[^0-9]"),
        ("date_strings", r"([^0-9](?:19|20)[0-9]{2}[01][0-9][0-3][0-9][^0-9])"),
        ("slashes", r"[^0-9]([0-3]?[0-9]/[01]?[0-9]/[0129][0-9]|[0-3][0-9]\.[01][0-9]\.[0129][0-9])[^0-9]"),
        ("yyyy_mm", r"[^0-9]([12][0-9]{3}[/.-](?:1[0-2]|0[1-9]))[^0-9]"),
        ("mm_yyyy", r"[^0-9]([01]?[0-9][/.-][12][0-9]{3})[^0-9]"),
        ("simple", r"[^0-9](199[0-9]|20[0-3][0-9])[^0-9]"),
    ].into_iter().map(|(name, pattern)| (name, RegexBuilder::new(pattern).unicode(false).build().unwrap())).collect())[name]
}

fn year(pattern: &str, expression: &str) -> Option<i32> {
    regex(expression)
        .captures(pattern)?
        .get(1)?
        .as_str()
        .parse()
        .ok()
}

fn plausible(
    html: &str,
    pattern: &str,
    year_pattern: &str,
    complete: bool,
    options: &Options,
) -> Vec<YearCandidate> {
    let mut candidates: Vec<YearCandidate> = Vec::new();
    let mut positions = HashMap::new();
    for parts in scanner(pattern).captures_iter(html.as_bytes()) {
        let matched = parts.get(1).unwrap_or_else(|| parts.get(0).unwrap());
        let pattern = String::from_utf8_lossy(matched.as_bytes()).into_owned();
        if let Some(index) = positions.get(&pattern).copied() {
            let candidate: &mut YearCandidate = &mut candidates[index];
            candidate.count += 1;
        } else {
            let source = String::from_utf8_lossy(&html.as_bytes()[parts.get(0).unwrap().start()..]);
            positions.insert(pattern.clone(), candidates.len());
            candidates.push(YearCandidate {
                pattern,
                count: 1,
                source: limit(&source, 100).into(),
            });
        }
    }
    let minimum = options.min_date.as_ref().unwrap().year();
    let maximum = options.max_date.as_ref().unwrap().year();
    candidates.retain(|candidate| {
        year(&candidate.pattern, year_pattern).is_some_and(|value| {
            let value = if complete { correct_year(value) } else { value };
            (minimum..=maximum).contains(&value)
        })
    });
    candidates
}

fn normalized(candidates: Vec<YearCandidate>, options: &Options) -> Vec<YearCandidate> {
    let mut normalized: Vec<YearCandidate> = Vec::new();
    let mut positions = HashMap::new();
    for candidate in candidates {
        let Some(date) = fast_parse(&candidate.pattern, options) else {
            continue;
        };
        let pattern = date.format("%Y-%m-%d").to_string();
        if let Some(index) = positions.get(&pattern).copied() {
            let existing: &mut YearCandidate = &mut normalized[index];
            existing.count += candidate.count;
        } else {
            positions.insert(pattern.clone(), normalized.len());
            normalized.push(YearCandidate {
                pattern,
                ..candidate
            });
        }
    }
    normalized
}

fn select(
    mut candidates: Vec<YearCandidate>,
    catcher: &str,
    year_pattern: &str,
    options: &Options,
) -> Option<(String, Vec<String>)> {
    if candidates.is_empty() || candidates.len() >= 1000 {
        return None;
    }
    let capture = |candidate: &YearCandidate| {
        regex(catcher).captures(&candidate.pattern).map(|parts| {
            (
                candidate.source.clone(),
                parts
                    .iter()
                    .map(|part| part.map_or("", |part| part.as_str()).to_owned())
                    .collect(),
            )
        })
    };
    if candidates.len() == 1 {
        return capture(&candidates[0]);
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.count));
    candidates.truncate(10);
    candidates.sort_by(|left, right| {
        if options.use_original_date {
            left.pattern.cmp(&right.pattern)
        } else {
            right.pattern.cmp(&left.pattern)
        }
    });
    candidates.truncate(2);
    let minimum = options.min_date.as_ref().unwrap().year();
    let maximum = options.max_date.as_ref().unwrap().year();
    let years: Vec<_> = candidates
        .iter()
        .map(|candidate| {
            year(&candidate.pattern, year_pattern).filter(|year| (minimum..=maximum).contains(year))
        })
        .collect();
    let chosen = match (years[0], years[1]) {
        (Some(first), Some(second)) => {
            if candidates[0].count != candidates[1].count
                && first != second
                && candidates[1].count as f64 / candidates[0].count as f64 > 0.5
            {
                1
            } else {
                0
            }
        }
        (Some(_), None) => 0,
        (None, Some(_)) => 1,
        (None, None) => return None,
    };
    capture(&candidates[chosen])
}

fn search(
    html: &str,
    pattern: &str,
    catcher: &str,
    year_pattern: &str,
    options: &Options,
) -> Option<(String, Vec<String>)> {
    select(
        plausible(html, pattern, year_pattern, false, options),
        catcher,
        year_pattern,
        options,
    )
}

fn ymd(candidate: (String, Vec<String>), copyright: i32, options: &Options) -> Option<Candidate> {
    let (source, parts) = candidate;
    let date = date_from_parts(
        parts.get(1)?.parse().ok()?,
        parts.get(2)?.parse().ok()?,
        parts.get(3)?.parse().ok()?,
        options,
    )?;
    (date.year() >= copyright).then_some((source, date))
}

fn search_normalized(
    html: &str,
    pattern: &str,
    year_pattern: &str,
    copyright: i32,
    complete: bool,
    options: &Options,
) -> Option<Candidate> {
    let candidates = normalized(
        plausible(html, pattern, year_pattern, complete, options),
        options,
    );
    ymd(
        select(candidates, "ymd", "ymd_year", options)?,
        copyright,
        options,
    )
}

pub(crate) fn page(html: &str, options: &Options) -> Option<Candidate> {
    let copyright =
        search(html, "copyright", "year", "year", options).and_then(|(source, parts)| {
            date_from_parts(parts.first()?.parse().ok()?, 1, 1, options).map(|date| (source, date))
        });
    let copyright_year = copyright.as_ref().map_or(0, |(_, date)| date.year());
    for (pattern, catcher) in [
        ("three", "three_catch"),
        ("three_loose", "three_loose_catch"),
    ] {
        if let Some(candidate) = search(html, pattern, catcher, "year", options)
            .and_then(|candidate| ymd(candidate, copyright_year, options))
        {
            return Some(candidate);
        }
    }
    if let Some(candidate) = search_normalized(
        html,
        "select_ymd",
        "select_ymd_year",
        copyright_year,
        false,
        options,
    ) {
        return Some(candidate);
    }
    if let Some(candidate) = search(html, "date_strings", "date_strings_catch", "year", options)
        .and_then(|candidate| ymd(candidate, copyright_year, options))
    {
        return Some(candidate);
    }
    if let Some(candidate) = search_normalized(
        html,
        "slashes",
        "slashes_year",
        copyright_year,
        true,
        options,
    ) {
        return Some(candidate);
    }
    if let Some((source, parts)) = search(html, "yyyy_mm", "yyyy_mm_catch", "year", options) {
        if let Some(date) = parts
            .get(1)
            .and_then(|year| year.parse().ok())
            .and_then(|year| date_from_parts(year, parts.get(2)?.parse().ok()?, 1, options))
            .filter(|date| date.year() >= copyright_year)
        {
            return Some((source, date));
        }
    }
    if let Some(candidate) = search_normalized(
        html,
        "mm_yyyy",
        "mm_yyyy_year",
        copyright_year,
        false,
        options,
    ) {
        return Some(candidate);
    }
    if let Some(date) = regex_parse(html, options).filter(|date| date.year() >= copyright_year) {
        return Some((html.into(), date));
    }
    if copyright.is_some() {
        return copyright;
    }
    let cleaned = regex("w3_cleaner").replace_all(html, " ");
    let (source, parts) = search(&cleaned, "simple", "year", "year", options)?;
    let date = date_from_parts(parts.get(1)?.parse().ok()?, 1, 1, options)?;
    Some((source, date))
}
