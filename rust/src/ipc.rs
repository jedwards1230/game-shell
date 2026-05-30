//! Unix-socket IPC server: accepts connections, parses one command per line,
//! and dispatches to the input runtime over the control channel. The
//! `subscribe` command upgrades the connection into an event stream fed by the
//! broadcast bus.
//!
//! Cross-platform (only `tokio`/`tokio-util`), so it compiles and can be tested
//! on non-Linux hosts even though the input runtime is Linux-only.

use crate::protocol::{self, Command, Event};
use crate::state::Control;
use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::os::unix::fs::PermissionsExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::codec::{Framed, LinesCodec};

/// Bind the socket (removing any stale file), chmod 0o600, and serve until the
/// process exits.
pub async fn serve(
    sock_path: String,
    control_tx: mpsc::Sender<Control>,
    events_tx: broadcast::Sender<Event>,
) -> Result<()> {
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding unix socket at {sock_path}"))?;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0o600 on {sock_path}"))?;
    tracing::info!("Listening on {sock_path}");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let control_tx = control_tx.clone();
                let events_tx = events_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, control_tx, events_tx).await {
                        tracing::debug!("client connection ended: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("accept error: {e}");
            }
        }
    }
}

/// Send a control request and await the runtime's response line.
async fn request<F>(control_tx: &mpsc::Sender<Control>, make: F) -> Option<String>
where
    F: FnOnce(oneshot::Sender<String>) -> Control,
{
    let (reply_tx, reply_rx) = oneshot::channel();
    control_tx.send(make(reply_tx)).await.ok()?;
    reply_rx.await.ok()
}

async fn handle_client(
    stream: UnixStream,
    control_tx: mpsc::Sender<Control>,
    events_tx: broadcast::Sender<Event>,
) -> Result<()> {
    let mut framed = Framed::new(stream, LinesCodec::new());

    while let Some(line) = framed.next().await {
        let line = line.context("reading command line")?;
        match Command::parse(&line) {
            Command::Subscribe => {
                framed.send(protocol::resp_subscribed()).await?;
                return stream_events(framed, events_tx).await;
            }
            cmd => {
                let response = dispatch(&control_tx, cmd).await;
                framed.send(response).await?;
            }
        }
    }
    Ok(())
}

/// Resolve a non-subscribe command to its response line.
async fn dispatch(control_tx: &mpsc::Sender<Control>, cmd: Command) -> String {
    let fallback = protocol::resp_unknown();
    match cmd {
        Command::Grab => request(control_tx, Control::Grab).await,
        Command::Release => request(control_tx, Control::Release).await,
        Command::Status => request(control_tx, Control::Status).await,
        Command::GetBindings => request(control_tx, Control::GetBindings).await,
        Command::SetBinding { action, button } => {
            request(control_tx, move |reply| Control::SetBinding {
                action,
                button,
                reply,
            })
            .await
        }
        Command::CaptureNext => request(control_tx, Control::CaptureNext).await,
        Command::CaptureCancel => request(control_tx, Control::CaptureCancel).await,
        Command::KbdLog(on) => request(control_tx, move |reply| Control::KbdLog(on, reply)).await,
        // Handled without a round-trip to the runtime:
        Command::SetBindingUsage => return protocol::resp_set_binding_usage(),
        Command::Unknown => return protocol::resp_unknown(),
        // Subscribe is handled by the caller before dispatch.
        Command::Subscribe => return protocol::resp_unknown(),
    }
    .unwrap_or(fallback)
}

/// Stream broadcast events to a subscribed client until it disconnects.
async fn stream_events(
    framed: Framed<UnixStream, LinesCodec>,
    events_tx: broadcast::Sender<Event>,
) -> Result<()> {
    let mut rx = events_tx.subscribe();
    let (mut sink, mut input) = framed.split();
    loop {
        tokio::select! {
            // Detect client disconnect (EOF / further input). Python reads
            // until EOF then drops the subscriber.
            next = input.next() => {
                match next {
                    None => return Ok(()),          // EOF
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(_)) => { /* ignore extra input from a subscriber */ }
                }
            }
            evt = rx.recv() => {
                match evt {
                    Ok(event) => sink.send(event.to_string()).await?,
                    // Slow subscriber fell behind: skip, don't die.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("subscriber lagged, dropped {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Control;

    /// A tiny stand-in runtime that answers control messages, so the IPC layer
    /// can be exercised end-to-end on any host (no evdev needed).
    async fn fake_runtime(mut rx: mpsc::Receiver<Control>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                Control::Grab(r) | Control::Release(r) | Control::CaptureCancel(r) => {
                    let _ = r.send(protocol::resp_ok());
                }
                Control::KbdLog(_, r) => {
                    let _ = r.send(protocol::resp_ok());
                }
                Control::Status(r) => {
                    let _ = r.send(protocol::resp_status(true, true));
                }
                Control::GetBindings(r) => {
                    let _ = r.send(protocol::resp_bindings(&[(
                        "select".into(),
                        "BTN_SOUTH".into(),
                    )]));
                }
                Control::SetBinding { reply, .. } => {
                    let _ = reply.send(protocol::resp_ok());
                }
                Control::CaptureNext(r) => {
                    let _ = r.send(protocol::resp_captured("BTN_SOUTH"));
                }
                Control::Shutdown => break,
            }
        }
    }

    async fn send_line(stream: &mut UnixStream, line: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        let mut buf = vec![0u8; 256];
        let n = stream.read(&mut buf).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).trim_end().to_string()
    }

    #[tokio::test]
    async fn end_to_end_commands_and_subscribe() {
        let dir = std::env::temp_dir();
        let sock = dir
            .join(format!("gs-ipc-test-{}.sock", std::process::id()))
            .to_string_lossy()
            .to_string();
        let (control_tx, control_rx) = mpsc::channel(16);
        let (events_tx, _) = broadcast::channel(16);

        tokio::spawn(fake_runtime(control_rx));
        let server = tokio::spawn(serve(sock.clone(), control_tx, events_tx.clone()));

        // Wait for the socket to appear.
        for _ in 0..100 {
            if std::path::Path::new(&sock).exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Command round-trips.
        let mut s = UnixStream::connect(&sock).await.unwrap();
        assert_eq!(send_line(&mut s, "grab").await, "ok");
        assert_eq!(send_line(&mut s, "status").await, "connected:grabbed");
        assert_eq!(
            send_line(&mut s, "get-bindings").await,
            r#"{"select":"BTN_SOUTH"}"#
        );
        assert_eq!(
            send_line(&mut s, "set-binding select").await,
            "error:usage: set-binding <action> <button_name>"
        );
        assert_eq!(send_line(&mut s, "frobnicate").await, "unknown");
        drop(s);

        // Subscribe receives broadcast events.
        let mut sub = UnixStream::connect(&sock).await.unwrap();
        assert_eq!(send_line(&mut sub, "subscribe").await, "subscribed");
        events_tx.send(Event::HomePress).unwrap();
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 64];
        let n = sub.read(&mut buf).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&buf[..n]).trim_end(), "home-press");

        server.abort();
    }
}
