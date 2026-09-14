use std::{borrow::Cow, sync::OnceLock};

use chrono::{DateTime, TimeZone};
use regex::Regex;

use crate::{
    dates::{
        correct_year, date_from_parts, fast_parse, find_time, swap_parts, try_date, url_date,
        valid_date,
    },
    dom::Node,
    rules::{limit, normalize, rules},
    Document, ExtractionResult, Options, Timezone,
};

pub(crate) type Candidate = (String, DateTime<Timezone>);

pub(crate) fn run(mut document: Cow<'_, Document>, options: &Options) -> ExtractionResult {
    let mut options = options.with_defaults();
    if options.url.is_empty() {
        options.url = document
            .tagged("link")
            .into_iter()
            .filter(|index| document.nodes[*index].attr("rel") == "canonical")
            .map(|index| document.nodes[index].attr("href").trim())
            .find(|href| !href.is_empty())
            .unwrap_or("")
            .to_owned();
    }
    let Some((source, date)) = find(&mut document, &options) else {
        return ExtractionResult::default();
    };
    let mut result = ExtractionResult {
        date_time: date,
        src_string: normalize(&source),
        ..ExtractionResult::default()
    };
    if options.extract_time {
        let clock = find_time(&source);
        result.has_time = clock.found;
        result.has_timezone = clock.timezone.is_some();
        result.date_time = clock.apply(result.date_time);
    }
    result
}

fn find(document: &mut Cow<'_, Document>, options: &Options) -> Option<Candidate> {
    let url = url_date(&options.url, options).map(|date| (options.url.clone(), date));
    if !options.defer_url_extractor && url.is_some() {
        return url;
    }
    if let Some(candidate) = metadata(document, options) {
        return Some(candidate);
    }
    if let Some(candidate) = json(document, options) {
        return Some(candidate);
    }
    if options.defer_url_extractor && url.is_some() {
        return url;
    }
    if let Some(candidate) = abbreviations(document, options) {
        return Some(candidate);
    }
    let document = document.to_mut();
    let unwanted: Vec<_> = document
        .elements()
        .filter(|index| {
            let node = &document.nodes[*index];
            matches!(
                node.tag.as_str(),
                "object"
                    | "embed"
                    | "applet"
                    | "frame"
                    | "frameset"
                    | "noframes"
                    | "iframe"
                    | "label"
                    | "map"
                    | "math"
                    | "audio"
                    | "canvas"
                    | "datalist"
                    | "picture"
                    | "rdf"
                    | "svg"
                    | "track"
                    | "video"
            ) || (node.tag == "div" && matches!(node.attr("id"), "wm-ipp-base" | "wm-ipp"))
        })
        .collect();
    for index in unwanted {
        document.remove(index);
    }
    let elements: Vec<_> = document
        .elements()
        .filter(|index| date_element(&document.nodes[*index], options.skip_extensive_search))
        .collect();
    if let Some(candidate) = other_elements(document, &elements, options) {
        return Some(candidate);
    }
    let titles: Vec<_> = document
        .elements()
        .filter(|index| matches!(document.nodes[*index].tag.as_str(), "title" | "h1"))
        .collect();
    if let Some(candidate) = other_elements(document, &titles, options) {
        return Some(candidate);
    }
    if let Some(candidate) = time_elements(document, options) {
        return Some(candidate);
    }
    let html = document
        .tagged("html")
        .first()
        .map_or_else(|| document.inner_html(0), |root| document.outer_html(*root));
    if let Some(candidate) = timestamp(&html, options) {
        return Some(candidate);
    }
    for index in document.tagged("meta") {
        let node = &document.nodes[index];
        let content = node.attr("content").trim();
        if node.attr("property") == "og:image" {
            if let Some(date) = url_date(content, options) {
                return Some((content.into(), date));
            }
        }
    }
    if let Some(candidate) = idiosyncrasies(&html, options) {
        return Some(candidate);
    }
    if !options.skip_extensive_search {
        let mut reference = Reference::default();
        for index in document
            .elements()
            .filter(|index| free_text_tag(&document.nodes[*index].tag))
        {
            for child in &document.nodes[index].children {
                let node = &document.nodes[*child];
                if node.removed || node.kind != crate::dom::Kind::Text {
                    continue;
                }
                let text = normalize(&node.data);
                if (7..52).contains(&text.chars().count()) {
                    reference.text(&text, options);
                }
            }
        }
        if let Some(candidate) = reference.finish(options) {
            return Some(candidate);
        }
        return crate::search::page(&html, options);
    }
    None
}

fn attempted(text: &str, options: &Options) -> Option<Candidate> {
    let (source, date) = try_date(text, options);
    date.map(|date| (source, date))
}

fn metadata(document: &Document, options: &Options) -> Option<Candidate> {
    let mut reserve = None;
    for index in document.tagged("meta") {
        let node = &document.nodes[index];
        let content = node.attr("content").trim();
        let datetime = node.attr("datetime").trim();
        if content.is_empty() && datetime.is_empty() {
            continue;
        }
        let name = node.attr("name").trim().to_lowercase();
        let property = node.attr("property").trim().to_lowercase();
        let itemprop = node.attr("itemprop").trim().to_lowercase();
        let equiv = node.attr("http-equiv").trim().to_lowercase();
        let mut primary = None;
        if !name.is_empty() && !content.is_empty() {
            if name == "og:url" {
                reserve = url_date(content, options).map(|date| (content.into(), date));
            } else if rules().date_attributes.contains(&name) {
                primary = attempted(content, options);
            } else if rules().modified_names.contains(&name) {
                if options.use_original_date {
                    reserve = attempted(content, options);
                } else {
                    primary = attempted(content, options);
                }
            }
        } else if !property.is_empty() && !content.is_empty() {
            let original = rules().date_attributes.contains(&property);
            let modified = rules().modified_properties.contains(&property);
            if original || modified {
                if let Some(candidate) = attempted(content, options) {
                    if (original && options.use_original_date)
                        || (modified && !options.use_original_date)
                    {
                        primary = Some(candidate);
                    } else {
                        reserve = Some(candidate);
                    }
                }
            }
        } else if !itemprop.is_empty() {
            let original = matches!(
                itemprop.as_str(),
                "datecreated" | "datepublished" | "pubyear"
            );
            let modified = matches!(itemprop.as_str(), "datemodified" | "dateupdate");
            if (original && options.use_original_date) || (modified && !options.use_original_date) {
                primary = attempted(
                    if datetime.is_empty() {
                        content
                    } else {
                        datetime
                    },
                    options,
                );
            } else if itemprop == "copyrightyear" && content.len() == 4 {
                if let Some(date) = content
                    .parse()
                    .ok()
                    .and_then(|year| date_from_parts(year, 1, 1, options))
                {
                    reserve = Some((content.into(), date));
                }
            }
        } else if node.attr("pubdate").trim().eq_ignore_ascii_case("pubdate") {
            primary = attempted(content, options);
        } else if !content.is_empty() && matches!(equiv.as_str(), "date" | "last-modified") {
            if (equiv == "date") == options.use_original_date {
                primary = attempted(content, options);
            } else {
                reserve = attempted(content, options);
            }
        }
        if primary.is_some() {
            return primary;
        }
    }
    reserve
}

fn json(document: &Document, options: &Options) -> Option<Candidate> {
    static PATTERNS: OnceLock<[Regex; 2]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        ["dateModified", "datePublished"].map(|name| {
            Regex::new(&format!(
                r#"(?i)"{name}": ?"((?:199[0-9]|20[0-3][0-9])-[01]?[0-9]-[0-3]?[0-9])"#
            ))
            .unwrap()
        })
    });
    let pattern = &patterns[usize::from(options.use_original_date)];
    for index in document.tagged("script") {
        if !matches!(
            document.nodes[index].attr("type"),
            "application/ld+json" | "application/settings+json"
        ) {
            continue;
        }
        let text = document.text(index);
        if !text.contains("\"date") {
            continue;
        }
        let Some(parts) = pattern.captures(&text) else {
            continue;
        };
        let matched = parts.get(1).unwrap();
        let Ok(day) = chrono::NaiveDate::parse_from_str(matched.as_str(), "%Y-%m-%d") else {
            continue;
        };
        let date = Timezone::Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0).unwrap());
        if !valid_date(&date, options) {
            continue;
        }
        let end = text[matched.start()..]
            .find('"')
            .map_or(matched.end(), |offset| matched.start() + offset);
        return Some((normalize(&text[matched.start()..end]), date));
    }
    None
}

#[derive(Default)]
pub(crate) struct Reference {
    value: i64,
    source: String,
}

impl Reference {
    fn compare(&mut self, value: i64, source: &str, options: &Options) {
        if if options.use_original_date {
            self.value == 0 || value < self.value
        } else {
            value > self.value
        } {
            self.value = value;
            self.source = source.into();
        }
    }

    pub fn text(&mut self, text: &str, options: &Options) {
        if let Some((source, date)) = attempted(text, options) {
            if let Some(timestamp) = crate::dates::reference_timestamp(&date) {
                self.compare(timestamp, &source, options);
            }
        }
    }

    pub fn finish(self, options: &Options) -> Option<Candidate> {
        if self.value <= 0 {
            return None;
        }
        crate::dates::reference_date(self.value, options).map(|date| (self.source, date))
    }
}

fn abbreviations(document: &Document, options: &Options) -> Option<Candidate> {
    let elements = document.tagged("abbr");
    if elements.is_empty() || elements.len() >= 1000 {
        return None;
    }
    let mut reference = Reference::default();
    for index in &elements {
        let node = &document.nodes[*index];
        let unix = node.attr("data-utime").trim();
        if !unix.is_empty() {
            if let Ok(value) = unix.parse() {
                reference.compare(value, unix, options);
            }
        } else if matches!(
            node.attr("class").trim(),
            "published" | "date-published" | "time-published"
        ) {
            let title = node.attr("title").trim();
            if !title.is_empty() {
                if options.use_original_date {
                    if let Some((_, date)) = attempted(title, options) {
                        return Some((title.into(), date));
                    }
                } else {
                    reference.text(title, options);
                    if reference.value > 0 {
                        break;
                    }
                }
            } else {
                let text = normalize(&document.initial_text(*index));
                if text.chars().count() > 10 {
                    reference.text(text.strip_prefix("am ").unwrap_or(&text), options);
                }
            }
        }
    }
    reference
        .finish(options)
        .or_else(|| other_elements(document, &elements, options))
}

fn time_elements(document: &Document, options: &Options) -> Option<Candidate> {
    let elements = document.tagged("time");
    if elements.is_empty() || elements.len() >= 1000 {
        return None;
    }
    let mut reference = Reference::default();
    for index in elements {
        let node = &document.nodes[index];
        let datetime = node.attr("datetime").trim();
        let class = node.attr("class").trim();
        if datetime.chars().count() > 6 {
            let shortcut = if options.use_original_date {
                node.attr("pubdate").trim().eq_ignore_ascii_case("pubdate")
                    || class.starts_with("entry-date")
                    || class.starts_with("entry-time")
            } else {
                class == "updated"
            };
            if shortcut {
                if let Some((_, date)) = attempted(datetime, options) {
                    return Some((datetime.into(), date));
                }
            } else {
                reference.text(datetime, options);
            }
        } else {
            let text = normalize(&document.initial_text(index));
            if text.chars().count() > 6 {
                reference.text(&text, options);
            }
        }
    }
    reference.finish(options)
}

fn other_elements(document: &Document, elements: &[usize], options: &Options) -> Option<Candidate> {
    if elements.is_empty() || elements.len() >= 1000 {
        return None;
    }
    for index in elements {
        for text in [
            document.text(*index),
            document.nodes[*index].attr("title").into(),
        ] {
            let normalized = normalize(&text);
            if normalized.chars().count() <= 6 {
                continue;
            }
            let shortened = limit(&normalized, 52)
                .trim_end_matches(|character: char| !character.is_ascii_digit());
            if let Some((_, date)) = attempted(shortened, options) {
                return Some((text, date));
            }
        }
    }
    None
}

pub(crate) fn free_text_tag(tag: &str) -> bool {
    matches!(
        tag,
        "div" | "h2" | "h3" | "h4" | "li" | "p" | "span" | "time" | "ul"
    )
}

fn date_element(node: &Node, fast: bool) -> bool {
    if matches!(node.tag.as_str(), "footer" | "small") {
        return true;
    }
    if fast && !free_text_tag(&node.tag) {
        return false;
    }
    let id = node.attr("id");
    let class = node.attr("class");
    let first = |names: &[&str]| {
        node.attrs
            .iter()
            .find(|(name, _)| names.contains(&name.as_str()))
            .map_or("", |(_, value)| value.as_str())
    };
    let id_class = first(&["id", "class"]);
    let date = first(&["id", "class", "itemprop"]).replace('D', "d");
    date.contains("date")
        || date.contains("datum")
        || id_class.replace('M', "m").contains("meta")
        || ["time", "publish", "footer"]
            .iter()
            .any(|part| id_class.contains(part))
        || [
            "info",
            "post_detail",
            "block-content",
            "byline",
            "subline",
            "posted",
            "submitted",
            "created-post",
            "publication",
            "author",
            "autor",
            "field-content",
            "fa-clock-o",
            "fa-calendar",
            "fecha",
            "parution",
        ]
        .iter()
        .any(|part| class.contains(part))
        || id.contains("footer-info-lastmod")
}

fn timestamp(html: &str, options: &Options) -> Option<Candidate> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let parts = PATTERN.get_or_init(|| Regex::new(r"((?:199[0-9]|20[0-3][0-9])-(?:[01]?[0-9])-(?:[0-3]?[0-9])).[0-9]{2}:[0-9]{2}:[0-9]{2}").unwrap()).captures(html)?;
    fast_parse(&parts[1], options).map(|date| (parts[0].into(), date))
}

fn idiosyncrasies(html: &str, options: &Options) -> Option<Candidate> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| [
        r#"(?i)(?:date[^0-9"]{0,20}|updated|last-modified|published|posted|on)[ :]*([0-9]{1,4})[./]([0-9]{1,2})[./]([0-9]{2,4})"#,
        r"(?i)(?:Datum|Stand|Ver[\x{00f6}\x{00d6}]ffentlicht am):? ?([0-9]{1,2})\.([0-9]{1,2})\.([0-9]{2,4})",
        r"(?i)(?:g[\x{00fc}\x{00dc}]ncellen?me|yay[\x{0131}I](?:m|n)lan?ma) *(?:tarihi)? *:? *([0-9]{1,2})[./]([0-9]{1,2})[./]([0-9]{2,4})",
        r"(?i)([0-9]{1,2})[./]([0-9]{1,2})[./]([0-9]{2,4}) *(?:['\x{2019}](?:de|da|te|ta)|tarihinde) *(?:g[\x{00fc}\x{00dc}]ncellendi|yay[\x{0131}I][mn]land[\x{0131}I])",
    ].iter().map(|pattern| Regex::new(pattern).unwrap()).collect());
    let (_, parts) = patterns
        .iter()
        .enumerate()
        .filter_map(|(index, pattern)| pattern.captures(html).map(|parts| (index, parts)))
        .min_by_key(|(index, parts)| {
            (
                parts.get(0).unwrap().start(),
                std::cmp::Reverse(parts.get(0).unwrap().len()),
                *index,
            )
        })?;
    let date = if parts[1].len() == 4 {
        date_from_parts(
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
            parts[3].parse().ok()?,
            options,
        )
    } else if matches!(parts[3].len(), 2 | 4) {
        let mut day = parts[1].parse().ok()?;
        let mut month = parts[2].parse().ok()?;
        swap_parts(&mut day, &mut month);
        date_from_parts(correct_year(parts[3].parse().ok()?), month, day, options)
    } else {
        None
    }?;
    Some((
        limit(&html[parts.get(0).unwrap().start()..], 100).into(),
        date,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_references_use_local_calendar_dates() {
        for (name, midnight, unix_date, minimum_date, maximum_date) in [
            ("UTC", 1577836800, "2020-01-02", "2020-01-02", ""),
            (
                "America/New_York",
                1577854800,
                "2020-01-01",
                "",
                "2020-01-01",
            ),
            ("Asia/Kolkata", 1577817000, "2020-01-02", "2020-01-02", ""),
        ] {
            let zone: Timezone = name.parse().unwrap();
            crate::dates::in_environment(crate::dates::Environment::new(zone.clone()), || {
                let options = Options {
                    min_date: Some(zone.with_ymd_and_hms(1995, 1, 1, 0, 0, 0).unwrap()),
                    max_date: Some(zone.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap()),
                    skip_extensive_search: true,
                    ..Options::default()
                };
                let expected = Timezone::Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
                let mut reference = Reference::default();
                reference.text("2020-01-01", &options);
                assert_eq!(reference.value, midnight, "{name}");
                assert_eq!(reference.finish(&options).unwrap().1, expected, "{name}");
                let mut reference = Reference::default();
                reference.compare(1577925000, "1577925000", &options);
                assert_eq!(
                    reference
                        .finish(&options)
                        .unwrap()
                        .1
                        .format("%F")
                        .to_string(),
                    unix_date,
                    "{name}"
                );
                let raw = r#"<abbr data-utime="1577925000"></abbr>"#;
                let text = r#"<abbr class="published">January 02, 2020</abbr>"#;
                for (case, body, original, minimum_at_noon, maximum_at_date, expected) in [
                    ("unix", raw.into(), false, false, false, unix_date),
                    (
                        "mixed-original",
                        format!("{raw}{text}"),
                        true,
                        false,
                        false,
                        unix_date,
                    ),
                    (
                        "mixed-modified",
                        format!("{raw}{text}"),
                        false,
                        false,
                        false,
                        "2020-01-02",
                    ),
                    (
                        "mixed-reversed-original",
                        format!("{text}{raw}"),
                        true,
                        false,
                        false,
                        unix_date,
                    ),
                    (
                        "mixed-reversed-modified",
                        format!("{text}{raw}"),
                        false,
                        false,
                        false,
                        "2020-01-02",
                    ),
                    (
                        "minimum-after-midnight",
                        raw.into(),
                        false,
                        true,
                        false,
                        minimum_date,
                    ),
                    (
                        "maximum-at-midnight",
                        raw.into(),
                        false,
                        false,
                        true,
                        maximum_date,
                    ),
                    (
                        "time-roundtrip",
                        r#"<time datetime="2020-01-02"></time>"#.into(),
                        false,
                        false,
                        false,
                        "2020-01-02",
                    ),
                ] {
                    let mut options = options.clone();
                    options.use_original_date = original;
                    if minimum_at_noon {
                        options.min_date =
                            Some(zone.with_ymd_and_hms(2020, 1, 1, 12, 0, 0).unwrap());
                    }
                    if maximum_at_date {
                        options.max_date =
                            Some(zone.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());
                    }
                    let result =
                        crate::from_html(&format!("<html><body>{body}</body></html>"), &options);
                    let actual = if result.is_zero() {
                        String::new()
                    } else {
                        result.date_time.format("%F").to_string()
                    };
                    assert_eq!(actual, expected, "{name}/{case}");
                }
            });
        }
    }

    #[test]
    fn python_date_selector_preserves_attribute_order() {
        for (html, expected) in [
            (
                "<time itemprop='dateModified' class='hidden'>2018-08-29</time>",
                true,
            ),
            (
                "<time class='hidden' itemprop='dateModified'>2018-08-29</time>",
                false,
            ),
            (
                "<time class='' itemprop='dateModified'>2018-08-29</time>",
                false,
            ),
        ] {
            let document = Document::parse(html);
            assert_eq!(
                date_element(&document.nodes[document.tagged("time")[0]], true),
                expected,
                "{html}"
            );
        }
    }

    #[test]
    fn python_json_uses_first_match_in_script_order() {
        let options = Options {
            use_original_date: true,
            ..Options::default()
        };
        for (scripts, expected) in [
            (
                r#"<script type="application/ld+json">{"dateCreated":"2020-01-01","datePublished":"2020-01-02"}</script>"#,
                "2020-01-02",
            ),
            (
                r#"<script type="application/ld+json">{"datePublished":"2020-02-02","other":{"datePublished":"2020-01-01"}}</script>"#,
                "2020-02-02",
            ),
            (
                r#"<script type="application/settings+json">{"datePublished":"2020-02-02"}</script><script type="application/ld+json">{"datePublished":"2020-01-01"}</script>"#,
                "2020-02-02",
            ),
            (
                r#"<script type="application/ld+json">{"datePublished":"2020-02-30","other":{"datePublished":"2020-01-01"}}</script><script type="application/ld+json">{"datePublished":"2020-03-03"}</script>"#,
                "2020-03-03",
            ),
        ] {
            let document = Document::parse(&format!("<html><head>{scripts}</head></html>"));
            assert_eq!(
                json(&document, &options)
                    .map(|(_, date)| date.format("%F").to_string())
                    .as_deref(),
                Some(expected),
                "{scripts}"
            );
        }
    }
}
