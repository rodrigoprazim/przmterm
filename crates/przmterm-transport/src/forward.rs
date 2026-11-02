//! SSH tunnels: local (`-L`), remote (`-R`) and dynamic SOCKS (`-D`) forwards.
//!
//! Tunnels start once a session has logged in and stop when it ends. Each
//! accepted connection gets its own SSH channel.

use std::sync::Arc;

use przmterm_core::forward::{ForwardKind, HostPort, Tunnel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::pipe::Output;
use crate::ssh::{Handle, RemoteTargets};

/// Starts every tunnel; returns the tasks to abort when the session ends.
/// Problems (a port in use, a refused remote forward) are reported in the
/// terminal and do not stop the session.
pub(crate) async fn start(
    handle: &Arc<Handle>,
    tunnels: &[Tunnel],
    remote: &RemoteTargets,
    out: &Output,
) -> Vec<JoinHandle<()>> {
    let mut tasks = Vec::new();
    for tunnel in tunnels {
        match tunnel.kind {
            ForwardKind::Local | ForwardKind::Dynamic => {
                let listener = match TcpListener::bind((
                    tunnel.listen.host.as_str(),
                    tunnel.listen.port,
                ))
                .await
                {
                    Ok(l) => l,
                    Err(e) => {
                        out.error(&format!("Tunnel {tunnel}: cannot listen: {e}"));
                        continue;
                    }
                };
                out.notice(&format!("Tunnel {tunnel}"));
                let (handle, out, tunnel) = (handle.clone(), out.clone(), tunnel.clone());
                tasks.push(tokio::spawn(accept_loop(listener, handle, tunnel, out)));
            }
            ForwardKind::Remote => {
                let Some(target) = tunnel.target.clone() else {
                    continue;
                };
                match handle
                    .tcpip_forward(tunnel.listen.host.as_str(), u32::from(tunnel.listen.port))
                    .await
                {
                    Ok(bound) => {
                        // Port 0 lets the server choose; it tells us which.
                        let port = if tunnel.listen.port == 0 {
                            bound
                        } else {
                            u32::from(tunnel.listen.port)
                        };
                        if let Ok(mut r) = remote.lock() {
                            r.push((port, target));
                        }
                        out.notice(&format!("Tunnel {tunnel}"));
                    }
                    Err(e) => out.error(&format!(
                        "Tunnel {tunnel}: the server refused to listen ({e})"
                    )),
                }
            }
        }
    }
    tasks
}

async fn accept_loop(listener: TcpListener, handle: Arc<Handle>, tunnel: Tunnel, out: Output) {
    loop {
        let Ok((socket, peer)) = listener.accept().await else {
            continue;
        };
        let _ = socket.set_nodelay(true);
        let (handle, tunnel, out) = (handle.clone(), tunnel.clone(), out.clone());
        tokio::spawn(async move {
            let result = match &tunnel.target {
                Some(target) => {
                    local(socket, &handle, target, &peer.ip().to_string(), peer.port()).await
                }
                None => dynamic(socket, &handle, &peer.ip().to_string(), peer.port()).await,
            };
            if let Err(e) = result {
                out.debug(&format!("Tunnel {}: {e}", tunnel.listen));
            }
        });
    }
}

async fn open(
    handle: &Handle,
    target: &HostPort,
    from_host: &str,
    from_port: u16,
) -> Result<russh::ChannelStream<russh::client::Msg>, String> {
    handle
        .channel_open_direct_tcpip(
            target.host.as_str(),
            u32::from(target.port),
            from_host,
            u32::from(from_port),
        )
        .await
        .map(|c| c.into_stream())
        .map_err(|e| format!("{target}: {e}"))
}

async fn local(
    mut socket: TcpStream,
    handle: &Handle,
    target: &HostPort,
    from_host: &str,
    from_port: u16,
) -> Result<(), String> {
    let mut channel = open(handle, target, from_host, from_port).await?;
    let _ = tokio::io::copy_bidirectional(&mut socket, &mut channel).await;
    Ok(())
}

/// A SOCKS 4, 4a or 5 client asks where to go; the server connects there.
async fn dynamic(
    mut socket: TcpStream,
    handle: &Handle,
    from_host: &str,
    from_port: u16,
) -> Result<(), String> {
    let mut version = [0u8; 1];
    socket
        .read_exact(&mut version)
        .await
        .map_err(|e| e.to_string())?;
    let (target, v5) = match version[0] {
        5 => (socks5_request(&mut socket).await?, true),
        4 => (socks4_request(&mut socket).await?, false),
        v => return Err(format!("not a SOCKS client (version byte {v})")),
    };
    let reply = |ok: bool| -> Vec<u8> {
        if v5 {
            vec![5, if ok { 0 } else { 5 }, 0, 1, 0, 0, 0, 0, 0, 0]
        } else {
            vec![0, if ok { 0x5a } else { 0x5b }, 0, 0, 0, 0, 0, 0]
        }
    };
    match open(handle, &target, from_host, from_port).await {
        Ok(mut channel) => {
            socket
                .write_all(&reply(true))
                .await
                .map_err(|e| e.to_string())?;
            let _ = tokio::io::copy_bidirectional(&mut socket, &mut channel).await;
            Ok(())
        }
        Err(e) => {
            let _ = socket.write_all(&reply(false)).await;
            Err(e)
        }
    }
}

/// SOCKS 5 (RFC 1928), no authentication, CONNECT only. The version byte
/// has been read.
async fn socks5_request(s: &mut TcpStream) -> Result<HostPort, String> {
    let io = |e: std::io::Error| e.to_string();
    let n = s.read_u8().await.map_err(io)?;
    let mut methods = vec![0u8; usize::from(n)];
    s.read_exact(&mut methods).await.map_err(io)?;
    if !methods.contains(&0) {
        let _ = s.write_all(&[5, 0xff]).await;
        return Err("the SOCKS client wants authentication".into());
    }
    s.write_all(&[5, 0]).await.map_err(io)?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await.map_err(io)?;
    if head[1] != 1 {
        let _ = s.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await;
        return Err("only SOCKS CONNECT is supported".into());
    }
    let host = match head[3] {
        1 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a).await.map_err(io)?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        3 => {
            let len = s.read_u8().await.map_err(io)?;
            let mut name = vec![0u8; usize::from(len)];
            s.read_exact(&mut name).await.map_err(io)?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a).await.map_err(io)?;
            std::net::Ipv6Addr::from(a).to_string()
        }
        t => return Err(format!("unknown SOCKS address type {t}")),
    };
    let port = s.read_u16().await.map_err(io)?;
    Ok(HostPort { host, port })
}

/// SOCKS 4 and 4a, CONNECT only. The version byte has been read.
async fn socks4_request(s: &mut TcpStream) -> Result<HostPort, String> {
    let io = |e: std::io::Error| e.to_string();
    let command = s.read_u8().await.map_err(io)?;
    let port = s.read_u16().await.map_err(io)?;
    let mut ip = [0u8; 4];
    s.read_exact(&mut ip).await.map_err(io)?;
    read_cstring(s).await?; // user id, ignored
    if command != 1 {
        let _ = s.write_all(&[0, 0x5b, 0, 0, 0, 0, 0, 0]).await;
        return Err("only SOCKS CONNECT is supported".into());
    }
    // 0.0.0.x (x ≠ 0) means SOCKS 4a: the host name follows.
    let host = if ip[..3] == [0, 0, 0] && ip[3] != 0 {
        read_cstring(s).await?
    } else {
        std::net::Ipv4Addr::from(ip).to_string()
    };
    Ok(HostPort { host, port })
}

async fn read_cstring(s: &mut TcpStream) -> Result<String, String> {
    let mut bytes = Vec::new();
    loop {
        let b = s.read_u8().await.map_err(|e| e.to_string())?;
        if b == 0 {
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        if bytes.len() >= 255 {
            return Err("SOCKS field too long".into());
        }
        bytes.push(b);
    }
}

/// A connection to one of our remote forwards: connect to its target here.
pub(crate) fn serve_remote(
    channel: russh::Channel<russh::client::Msg>,
    target: HostPort,
    out: Output,
) {
    tokio::spawn(async move {
        match TcpStream::connect((target.host.as_str(), target.port)).await {
            Ok(mut socket) => {
                let _ = socket.set_nodelay(true);
                let mut channel = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut socket, &mut channel).await;
            }
            Err(e) => out.debug(&format!("Remote tunnel: cannot reach {target}: {e}")),
        }
    });
}
