use regex::Regex;
use serde::Deserialize;
use std::{collections::HashMap, sync::OnceLock};

#[derive(Deserialize)]
pub(crate) struct Rules {
    pub patterns: HashMap<String, String>,
    pub months: HashMap<String, u32>,
    pub timezones: HashMap<String, i32>,
    pub date_attributes: Vec<String>,
    pub modified_properties: Vec<String>,
    pub modified_names: Vec<String>,
}

pub(crate) fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        serde_json::from_str(include_str!("../data/rules.json"))
            .expect("pinned Go extraction rules")
    })
}

pub(crate) fn regex(name: &str) -> &'static Regex {
    static PATTERNS: OnceLock<HashMap<String, Regex>> = OnceLock::new();
    &PATTERNS.get_or_init(|| {
        rules()
            .patterns
            .iter()
            .map(|(name, pattern)| {
                (
                    name.clone(),
                    Regex::new(pattern)
                        .unwrap_or_else(|error| panic!("invalid {name} pattern: {error}")),
                )
            })
            .collect()
    })[name]
}

pub(crate) fn normalize(text: &str) -> String {
    text.split(rust_dateutil::lexer::is_space)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn limit(text: &str, length: usize) -> &str {
    text.char_indices()
        .nth(length)
        .map_or(text, |(end, _)| &text[..end])
}
