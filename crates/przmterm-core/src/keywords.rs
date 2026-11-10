//! Keyword highlighting sets, shared by the configuration and the renderer.
//!
//! A set is a TOML file in `<config>/keywords/<name>.toml`:
//!
//! ```toml
//! match_case = false
//!
//! [[rule]]
//! text = "down"            # a whole word (or phrase)
//! color = "#ff0000"
//!
//! [[rule]]
//! regex = '\b\d{1,3}(\.\d{1,3}){3}\b'
//! color = "#ff55ff"
//! background = "#202020"   # optional
//! bold = true              # optional
//! ```
//!
//! Where matches overlap, the longest wins (`admin-down` over `down`), and
//! on equal length the earlier rule.

use serde::{Deserialize, Serialize};

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeywordSet {
    /// `Down` and `down` differ only when true.
    #[serde(skip_serializing_if = "is_false")]
    pub match_case: bool,
    #[serde(rename = "rule")]
    pub rules: Vec<Keyword>,
    /// Keys in the file this version does not know (left out).
    #[serde(skip)]
    pub unknown: Vec<String>,
}

/// One rule: a word (`text`) or a regular expression (`regex`), and how
/// matching text is drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keyword {
    /// Matched as a whole word: `up` colours "up" and "link-up", not "backup".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// A regular expression (lookaround allowed), used instead of `text`.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub regex: String,
    /// `#rrggbb`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub bold: bool,
}

/// Parses `#rrggbb` (the `#` is optional).
pub fn parse_color(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

pub fn format_color([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_round_trip() {
        assert_eq!(parse_color("#ff8000"), Some([255, 128, 0]));
        assert_eq!(parse_color("00ff00"), Some([0, 255, 0]));
        assert_eq!(parse_color("#fff"), None);
        assert_eq!(format_color([1, 2, 255]), "#0102ff");
    }

    #[test]
    fn reads_a_set() {
        let set: KeywordSet = toml::from_str(
            "[[rule]]\ntext = 'down'\ncolor = '#ff0000'\n\
             [[rule]]\nregex = '\\d+'\nbold = true",
        )
        .unwrap();
        assert_eq!(set.rules.len(), 2);
        assert!(!set.match_case);
        assert!(set.rules[1].bold);
        assert_eq!(
            toml::from_str::<KeywordSet>(&toml::to_string(&set).unwrap()).unwrap(),
            set
        );
    }
}
