//! Command bus shared by every PrzmTerm front end.
//!
//! Menus, shortcuts, the button bar, scripts, the CLI and the MCP server all
//! invoke the same named commands through a [`CommandBus`], so nothing is
//! reachable only from the GUI.

pub mod forward;
pub mod keywords;
pub mod serial;
pub mod triggers;

use std::collections::BTreeMap;
use std::fmt;

/// Application version, taken from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Which protocol a session uses.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Ssh,
    Telnet,
    Serial,
    Local,
}

impl Protocol {
    /// Default TCP port, or `None` for protocols that do not use one.
    pub fn default_port(self) -> Option<u16> {
        match self {
            Protocol::Ssh => Some(22),
            Protocol::Telnet => Some(23),
            Protocol::Serial | Protocol::Local => None,
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Protocol::Ssh => "ssh",
            Protocol::Telnet => "telnet",
            Protocol::Serial => "serial",
            Protocol::Local => "local",
        })
    }
}

/// Stable identifier of a command, e.g. `session.open` or `tab.close`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(String);

impl CommandId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Positional string arguments passed to a command.
pub type Args = Vec<String>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommandError {
    #[error("unknown command `{0}`")]
    Unknown(CommandId),
    #[error("command `{0}` is already registered")]
    Duplicate(CommandId),
    #[error("invalid arguments for `{id}`: {reason}")]
    InvalidArgs { id: CommandId, reason: String },
    #[error("command `{id}` failed: {reason}")]
    Failed { id: CommandId, reason: String },
}

type Handler<C> = Box<dyn Fn(&mut C, &Args) -> Result<String, CommandError> + Send + Sync>;

struct Entry<C> {
    summary: String,
    handler: Handler<C>,
}

/// Registry of named commands that operate on an application context `C`.
pub struct CommandBus<C> {
    commands: BTreeMap<CommandId, Entry<C>>,
}

impl<C> Default for CommandBus<C> {
    fn default() -> Self {
        Self {
            commands: BTreeMap::new(),
        }
    }
}

impl<C> CommandBus<C> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a command. Fails if the id is already taken.
    pub fn register<F>(
        &mut self,
        id: impl Into<String>,
        summary: impl Into<String>,
        handler: F,
    ) -> Result<(), CommandError>
    where
        F: Fn(&mut C, &Args) -> Result<String, CommandError> + Send + Sync + 'static,
    {
        let id = CommandId::new(id);
        if self.commands.contains_key(&id) {
            return Err(CommandError::Duplicate(id));
        }
        self.commands.insert(
            id,
            Entry {
                summary: summary.into(),
                handler: Box::new(handler),
            },
        );
        Ok(())
    }

    /// Runs a command by id and returns its textual result.
    pub fn invoke(&self, ctx: &mut C, id: &str, args: &Args) -> Result<String, CommandError> {
        let key = CommandId::new(id);
        let entry = self
            .commands
            .get(&key)
            .ok_or_else(|| CommandError::Unknown(key.clone()))?;
        (entry.handler)(ctx, args)
    }

    /// Lists registered commands with their one-line summaries, sorted by id.
    pub fn list(&self) -> impl Iterator<Item = (&CommandId, &str)> {
        self.commands.iter().map(|(id, e)| (id, e.summary.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Ctx {
        sent: Vec<String>,
    }

    fn bus() -> CommandBus<Ctx> {
        let mut bus = CommandBus::new();
        bus.register(
            "session.send",
            "Send text to the active session",
            |ctx: &mut Ctx, args| {
                let text = args.first().ok_or_else(|| CommandError::InvalidArgs {
                    id: CommandId::new("session.send"),
                    reason: "missing text".into(),
                })?;
                ctx.sent.push(text.clone());
                Ok(format!("sent {} bytes", text.len()))
            },
        )
        .unwrap();
        bus
    }

    #[test]
    fn invokes_registered_command() {
        let bus = bus();
        let mut ctx = Ctx::default();
        let out = bus
            .invoke(&mut ctx, "session.send", &vec!["show run".into()])
            .unwrap();
        assert_eq!(out, "sent 8 bytes");
        assert_eq!(ctx.sent, vec!["show run"]);
    }

    #[test]
    fn rejects_unknown_and_duplicate() {
        let mut bus = bus();
        let mut ctx = Ctx::default();
        assert!(matches!(
            bus.invoke(&mut ctx, "nope", &vec![]),
            Err(CommandError::Unknown(_))
        ));
        assert!(matches!(
            bus.register("session.send", "dup", |_, _| Ok(String::new())),
            Err(CommandError::Duplicate(_))
        ));
    }

    #[test]
    fn reports_missing_args() {
        let bus = bus();
        let mut ctx = Ctx::default();
        assert!(matches!(
            bus.invoke(&mut ctx, "session.send", &vec![]),
            Err(CommandError::InvalidArgs { .. })
        ));
    }
}
