//! Triggers: text that, when it shows up in a session's output, gets an
//! answer typed automatically.
//!
//! ```toml
//! [[trigger]]
//! match = "--More--"          # plain text…
//! send = ' '
//!
//! [[trigger]]
//! match = '\[confirm\]$'      # …or a regular expression
//! regex = true
//! send = '\r'
//!
//! [[trigger]]
//! match = "Password:"
//! send = 'secret\r'
//! once = true                 # only the first time per connection
//! ```
//!
//! `send` takes the same escapes as buttons: `\r` Enter, `\n`, `\t`, `\e`,
//! `\p` (one-second pause) and `\\`.

use serde::{Deserialize, Serialize};

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trigger {
    /// What to look for in the output.
    #[serde(rename = "match")]
    pub pattern: String,
    /// `pattern` is a regular expression rather than plain text.
    #[serde(default, skip_serializing_if = "is_false")]
    pub regex: bool,
    /// What to type when it appears.
    #[serde(default)]
    pub send: String,
    /// Fire only the first time per connection.
    #[serde(default, skip_serializing_if = "is_false")]
    pub once: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_triggers() {
        #[derive(Deserialize)]
        struct File {
            trigger: Vec<Trigger>,
        }
        let f: File = toml::from_str(
            "[[trigger]]\nmatch = '--More--'\nsend = ' '\n\
             [[trigger]]\nmatch = 'x$'\nregex = true\nsend = '\\r'\nonce = true",
        )
        .unwrap();
        assert_eq!(f.trigger[0].pattern, "--More--");
        assert!(!f.trigger[0].regex);
        assert!(f.trigger[1].regex && f.trigger[1].once);
        assert_eq!(f.trigger[1].send, "\\r");
    }
}
