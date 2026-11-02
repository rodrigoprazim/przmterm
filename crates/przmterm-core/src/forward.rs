//! SSH tunnels (port forwarding), shared by session files and the transport.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    /// `ssh -L`: a port here reaches `target` from the server's side.
    #[default]
    Local,
    /// `ssh -R`: a port on the server reaches `target` from this side.
    Remote,
    /// `ssh -D`: a SOCKS proxy here; each connection picks its destination.
    Dynamic,
}

impl ForwardKind {
    pub const ALL: [ForwardKind; 3] = [
        ForwardKind::Local,
        ForwardKind::Remote,
        ForwardKind::Dynamic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ForwardKind::Local => "local",
            ForwardKind::Remote => "remote",
            ForwardKind::Dynamic => "dynamic (SOCKS)",
        }
    }
}

/// One tunnel as written in a session file:
///
/// ```toml
/// [[forward]]
/// type = "local"
/// listen = "8080"            # or "127.0.0.1:8080", "0.0.0.0:8080"
/// target = "10.0.0.5:80"     # not used by dynamic forwards
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forward {
    #[serde(rename = "type", default)]
    pub kind: ForwardKind,
    pub listen: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
}

/// A `host:port` pair. IPv6 hosts are written in brackets: `[::1]:22`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

impl HostPort {
    /// Parses `host:port`, `[v6]:port`, or a bare port (host = `default_host`).
    pub fn parse(text: &str, default_host: &str) -> Result<HostPort, String> {
        let text = text.trim();
        let port_of = |p: &str| {
            p.parse::<u16>()
                .map_err(|_| format!("`{p}` is not a valid port"))
        };
        if let Ok(port) = text.parse::<u16>() {
            return Ok(HostPort {
                host: default_host.to_string(),
                port,
            });
        }
        let (host, port) = if let Some(rest) = text.strip_prefix('[') {
            let (host, after) = rest
                .split_once(']')
                .ok_or_else(|| format!("missing `]` in `{text}`"))?;
            let port = after
                .strip_prefix(':')
                .ok_or_else(|| format!("`{text}` needs a port"))?;
            (host, port)
        } else {
            text.rsplit_once(':')
                .ok_or_else(|| format!("`{text}` needs host:port"))?
        };
        if host.is_empty() {
            return Err(format!("`{text}` needs a host"));
        }
        Ok(HostPort {
            host: host.to_string(),
            port: port_of(port)?,
        })
    }
}

/// A forward with its addresses checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunnel {
    pub kind: ForwardKind,
    /// Where to listen: here for local and dynamic, on the server for remote.
    pub listen: HostPort,
    /// Where connections go; `None` for dynamic forwards.
    pub target: Option<HostPort>,
}

impl Forward {
    /// Checks the addresses. A bare listen port binds to localhost.
    pub fn tunnel(&self) -> Result<Tunnel, String> {
        let default_listen = match self.kind {
            ForwardKind::Remote => "localhost",
            _ => "127.0.0.1",
        };
        let listen =
            HostPort::parse(&self.listen, default_listen).map_err(|e| format!("listen: {e}"))?;
        let target = match self.kind {
            ForwardKind::Dynamic => None,
            _ if self.target.trim().is_empty() => {
                return Err("target: a local or remote forward needs host:port".into());
            }
            _ => Some(
                HostPort::parse(&self.target, "localhost").map_err(|e| format!("target: {e}"))?,
            ),
        };
        Ok(Tunnel {
            kind: self.kind,
            listen,
            target,
        })
    }
}

impl fmt::Display for Tunnel {
    /// `L 127.0.0.1:8080 → 10.0.0.5:80`, `D 127.0.0.1:1080`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let letter = match self.kind {
            ForwardKind::Local => 'L',
            ForwardKind::Remote => 'R',
            ForwardKind::Dynamic => 'D',
        };
        write!(f, "{letter} {}", self.listen)?;
        if let Some(target) = &self.target {
            write!(f, " → {target}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Forward {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.tunnel() {
            Ok(t) => t.fmt(f),
            Err(_) => write!(f, "{} {} {}", self.kind.label(), self.listen, self.target),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fwd(kind: ForwardKind, listen: &str, target: &str) -> Forward {
        Forward {
            kind,
            listen: listen.into(),
            target: target.into(),
        }
    }

    #[test]
    fn parses_tunnels() {
        let t = fwd(ForwardKind::Local, "8080", "10.0.0.5:80")
            .tunnel()
            .unwrap();
        assert_eq!(t.to_string(), "L 127.0.0.1:8080 → 10.0.0.5:80");
        let t = fwd(ForwardKind::Remote, "0.0.0.0:2222", "22")
            .tunnel()
            .unwrap();
        assert_eq!(t.to_string(), "R 0.0.0.0:2222 → localhost:22");
        let t = fwd(ForwardKind::Dynamic, "1080", "").tunnel().unwrap();
        assert_eq!(t.to_string(), "D 127.0.0.1:1080");
        let t = fwd(ForwardKind::Local, "[::1]:8080", "[fe80::1]:80")
            .tunnel()
            .unwrap();
        assert_eq!(t.to_string(), "L [::1]:8080 → [fe80::1]:80");
    }

    #[test]
    fn rejects_bad_addresses() {
        assert!(fwd(ForwardKind::Local, "8080", "").tunnel().is_err());
        assert!(fwd(ForwardKind::Local, "99999", "a:1").tunnel().is_err());
        assert!(fwd(ForwardKind::Local, "8080", "host").tunnel().is_err());
        assert!(fwd(ForwardKind::Local, ":8080", "a:1").tunnel().is_err());
    }

    #[test]
    fn reads_session_toml() {
        #[derive(Deserialize)]
        struct File {
            forward: Vec<Forward>,
        }
        let f: File = toml::from_str(
            "[[forward]]\ntype = 'dynamic'\nlisten = '1080'\n\
             [[forward]]\nlisten = '8443'\ntarget = 'fw:443'",
        )
        .unwrap();
        assert_eq!(f.forward[0].kind, ForwardKind::Dynamic);
        assert_eq!(f.forward[1].kind, ForwardKind::Local);
    }
}
