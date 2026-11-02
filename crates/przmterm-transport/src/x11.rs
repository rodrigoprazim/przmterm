//! X11 forwarding: remote graphical programs drawn by the local X server
//! (XQuartz on macOS, the desktop's X server or XWayland on Linux, VcXsrv
//! and the like on Windows).
//!
//! As OpenSSH does, the server only ever sees a random *fake* cookie. Each
//! X connection that comes back must present it; PrzmTerm then puts the real
//! cookie of the local display in its place (or none, if the display has no
//! cookie) before passing the connection on.

use std::path::PathBuf;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::pipe::Output;

pub const MIT_MAGIC_COOKIE: &str = "MIT-MAGIC-COOKIE-1";

/// Where the local X server listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum X11Target {
    Tcp(String, u16),
    /// A Unix socket: `/tmp/.X11-unix/X0`, or XQuartz's launchd socket.
    Unix(PathBuf),
}

/// The local display to forward to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X11Display {
    /// As written in `$DISPLAY`, for messages.
    pub name: String,
    pub target: X11Target,
    pub screen: u32,
    /// The display's real MIT-MAGIC-COOKIE-1, from `xauth`, if it has one.
    pub cookie: Option<Vec<u8>>,
}

impl X11Display {
    /// Parses `$DISPLAY`: `:0`, `:0.1`, `unix:0`, `localhost:10.0`,
    /// `[::1]:0`, or a socket path such as XQuartz's
    /// `/private/tmp/com.apple.launchd.xyz/org.xquartz:0`.
    pub fn parse(display: &str) -> Option<X11Display> {
        let display = display.trim();
        let (host, rest) = display.rsplit_once(':')?;
        let (number, screen) = match rest.split_once('.') {
            Some((n, s)) => (n, s.parse().ok()?),
            None => (rest, 0),
        };
        let number: u16 = number.parse().ok()?;
        let target = if host.starts_with('/') {
            // The socket file is named `<path>:<number>`.
            X11Target::Unix(PathBuf::from(format!("{host}:{number}")))
        } else if host.is_empty() || host == "unix" {
            if cfg!(unix) {
                X11Target::Unix(PathBuf::from(format!("/tmp/.X11-unix/X{number}")))
            } else {
                X11Target::Tcp("127.0.0.1".into(), 6000 + number)
            }
        } else {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            X11Target::Tcp(host.to_string(), 6000u16.checked_add(number)?)
        };
        Some(X11Display {
            name: display.to_string(),
            target,
            screen,
            cookie: None,
        })
    }

    /// `$DISPLAY`, or the usual default: `:0` (Windows X servers listen on
    /// TCP port 6000).
    pub fn from_env() -> X11Display {
        std::env::var("DISPLAY")
            .ok()
            .filter(|d| !d.trim().is_empty())
            .and_then(|d| X11Display::parse(&d))
            .or_else(|| X11Display::parse(if cfg!(windows) { "localhost:0" } else { ":0" }))
            .expect("the default display parses")
    }
}

/// Parses `xauth list` output for the display's MIT cookie.
pub fn cookie_from_xauth_list(output: &str) -> Option<Vec<u8>> {
    output.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (_display, proto, hex) = (parts.next()?, parts.next()?, parts.next()?);
        (proto == MIT_MAGIC_COOKIE).then(|| from_hex(hex)).flatten()
    })
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Forwarding state of one SSH connection.
#[derive(Debug)]
pub(crate) struct X11Forward {
    pub(crate) display: X11Display,
    /// What the server was given instead of the real cookie.
    pub(crate) fake: [u8; 16],
}

impl X11Forward {
    pub(crate) fn new(display: X11Display) -> Self {
        Self {
            display,
            fake: rand::random(),
        }
    }
}

const PAD: fn(usize) -> usize = |n| (4 - n % 4) % 4;

/// Reads the client's connection setup from `remote`, checks the fake
/// cookie and returns the setup to send to the local server.
async fn rewrite_setup<R: AsyncRead + Unpin>(
    remote: &mut R,
    forward: &X11Forward,
) -> Result<Vec<u8>, String> {
    let io = |e: std::io::Error| e.to_string();
    let mut head = [0u8; 12];
    remote.read_exact(&mut head).await.map_err(io)?;
    let u16_at = |i: usize| match head[0] {
        b'B' => u16::from_be_bytes([head[i], head[i + 1]]),
        _ => u16::from_le_bytes([head[i], head[i + 1]]),
    };
    let (name_len, data_len) = (usize::from(u16_at(6)), usize::from(u16_at(8)));
    let mut rest = vec![0u8; name_len + PAD(name_len) + data_len + PAD(data_len)];
    remote.read_exact(&mut rest).await.map_err(io)?;
    let name = &rest[..name_len];
    let data_at = name_len + PAD(name_len);
    let data = &rest[data_at..data_at + data_len];
    if name != MIT_MAGIC_COOKIE.as_bytes() || data != forward.fake {
        return Err("an X11 connection without the right cookie was refused".into());
    }
    let (name, cookie): (&[u8], &[u8]) = match &forward.display.cookie {
        Some(real) => (MIT_MAGIC_COOKIE.as_bytes(), real),
        None => (b"", b""),
    };
    let put_u16 = |v: usize| -> [u8; 2] {
        let v = v as u16;
        if head[0] == b'B' {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let mut setup = head[..6].to_vec();
    setup.extend(put_u16(name.len()));
    setup.extend(put_u16(cookie.len()));
    setup.extend(&head[10..12]);
    setup.extend(name);
    setup.extend(std::iter::repeat_n(0, PAD(name.len())));
    setup.extend(cookie);
    setup.extend(std::iter::repeat_n(0, PAD(cookie.len())));
    Ok(setup)
}

/// Relays one X11 connection from the server to the local display.
pub(crate) fn serve<S>(mut remote: S, forward: std::sync::Arc<X11Forward>, out: Output)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let setup = match rewrite_setup(&mut remote, &forward).await {
            Ok(setup) => setup,
            Err(e) => {
                out.debug(&format!("X11: {e}"));
                return;
            }
        };
        let display = &forward.display;
        let unreachable = |e: std::io::Error| {
            out.notice(&format!(
                "X11: cannot reach the X server at {} ({e}). Is it running (XQuartz on macOS)?",
                display.name
            ));
        };
        match &display.target {
            X11Target::Tcp(host, port) => {
                match tokio::net::TcpStream::connect((host.as_str(), *port)).await {
                    Ok(mut local) => {
                        let _ = local.set_nodelay(true);
                        if local.write_all(&setup).await.is_ok() {
                            let _ = tokio::io::copy_bidirectional(&mut remote, &mut local).await;
                        }
                    }
                    Err(e) => unreachable(e),
                }
            }
            #[cfg(unix)]
            X11Target::Unix(path) => match tokio::net::UnixStream::connect(path).await {
                Ok(mut local) => {
                    if local.write_all(&setup).await.is_ok() {
                        let _ = tokio::io::copy_bidirectional(&mut remote, &mut local).await;
                    }
                }
                Err(e) => unreachable(e),
            },
            #[cfg(not(unix))]
            X11Target::Unix(_) => unreachable(std::io::Error::other("no Unix sockets here")),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_displays() {
        let d = X11Display::parse("localhost:10.0").unwrap();
        assert_eq!(d.target, X11Target::Tcp("localhost".into(), 6010));
        let d = X11Display::parse("[::1]:2.1").unwrap();
        assert_eq!(
            (d.target, d.screen),
            (X11Target::Tcp("::1".into(), 6002), 1)
        );
        let d = X11Display::parse("/private/tmp/com.apple.launchd.ab/org.xquartz:0").unwrap();
        assert_eq!(
            d.target,
            X11Target::Unix("/private/tmp/com.apple.launchd.ab/org.xquartz:0".into())
        );
        #[cfg(unix)]
        assert_eq!(
            X11Display::parse(":1").unwrap().target,
            X11Target::Unix("/tmp/.X11-unix/X1".into())
        );
        assert!(X11Display::parse("nonsense").is_none());
    }

    #[test]
    fn reads_xauth_cookies() {
        let out = "vm/unix:0  MIT-MAGIC-COOKIE-1  00112233445566778899aabbccddeeff\n";
        assert_eq!(
            cookie_from_xauth_list(out).unwrap(),
            (0u8..16).map(|i| i * 0x11).collect::<Vec<_>>()
        );
        assert_eq!(
            cookie_from_xauth_list("vm/unix:0  XDM-AUTHORIZATION-1  00"),
            None
        );
    }

    fn setup(order: u8, name: &[u8], data: &[u8]) -> Vec<u8> {
        let n = |v: usize| {
            if order == b'B' {
                (v as u16).to_be_bytes()
            } else {
                (v as u16).to_le_bytes()
            }
        };
        let mut s = vec![order, 0, 0, 11, 0, 0];
        s.extend(n(name.len()));
        s.extend(n(data.len()));
        s.extend([0, 0]);
        s.extend(name);
        s.extend(std::iter::repeat_n(0, PAD(name.len())));
        s.extend(data);
        s.extend(std::iter::repeat_n(0, PAD(data.len())));
        s
    }

    #[tokio::test]
    async fn swaps_the_fake_cookie_for_the_real_one() {
        let mut display = X11Display::parse("localhost:0").unwrap();
        display.cookie = Some(vec![7; 16]);
        let forward = X11Forward::new(display);
        for order in *b"lB" {
            let sent = setup(order, MIT_MAGIC_COOKIE.as_bytes(), &forward.fake);
            let got = rewrite_setup(&mut &sent[..], &forward).await.unwrap();
            assert_eq!(got, setup(order, MIT_MAGIC_COOKIE.as_bytes(), &[7; 16]));
        }
        let wrong = setup(b'l', MIT_MAGIC_COOKIE.as_bytes(), &[0; 16]);
        assert!(rewrite_setup(&mut &wrong[..], &forward).await.is_err());

        // A display without a cookie gets no authentication at all.
        let forward = X11Forward::new(X11Display::parse("localhost:0").unwrap());
        let sent = setup(b'l', MIT_MAGIC_COOKIE.as_bytes(), &forward.fake);
        let got = rewrite_setup(&mut &sent[..], &forward).await.unwrap();
        assert_eq!(got, setup(b'l', b"", b""));
    }
}
