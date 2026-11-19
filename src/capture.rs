use crate::tracker::{Shared, lock};
use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, de::DeserializeOwned};
use std::{
    env,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::Notify,
};

#[derive(Clone)]
pub struct Ipc {
    pub events: PathBuf,
    pub request: PathBuf,
}

impl Ipc {
    pub fn from_env() -> Result<Self> {
        let signature = env::var("HYPRLAND_INSTANCE_SIGNATURE").context(
            "run Irminsul from inside Hyprland (HYPRLAND_INSTANCE_SIGNATURE is missing)",
        )?;
        ensure!(
            !signature.is_empty()
                && signature
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
            "invalid HYPRLAND_INSTANCE_SIGNATURE"
        );
        let runtime = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .context("XDG_RUNTIME_DIR is not set to an absolute path")?;
        let base = runtime.join("hypr").join(&signature);
        Ok(Self {
            events: base.join(".socket2.sock"),
            request: base.join(".socket.sock"),
        })
    }

    async fn snapshot<T: DeserializeOwned>(&self, command: &'static [u8]) -> Result<T> {
        // Hyprland handles this socket synchronously. Write immediately, bound the
        // response and drop the stream promptly even if the compositor hangs.
        tokio::time::timeout(Duration::from_millis(350), async {
            let mut stream = UnixStream::connect(&self.request).await?;
            stream.write_all(command).await?;
            let mut buf = Vec::with_capacity(1024);
            stream.take(256 * 1024).read_to_end(&mut buf).await?;
            Ok(serde_json::from_slice(&buf)?)
        })
        .await
        .context("Hyprland IPC request timed out")?
    }
}

#[derive(Deserialize)]
struct Window {
    #[serde(default)]
    class: String,
    #[serde(default, rename = "initialClass")]
    initial_class: String,
}
#[derive(Deserialize)]
struct Lock {
    locked: bool,
}
#[derive(Deserialize)]
struct Monitor {
    #[serde(rename = "dpmsStatus")]
    on: bool,
}

pub async fn focus_loop(ipc: Ipc, shared: Shared, probe: Arc<Notify>) -> Result<()> {
    let mut lost_since = Instant::now();
    let mut ever_connected = false;
    let mut delay = Duration::from_millis(300);
    loop {
        match UnixStream::connect(&ipc.events).await {
            Ok(stream) => {
                if !ever_connected {
                    eprintln!("irminsul: listening for Hyprland focus changes");
                }
                ever_connected = true;
                delay = Duration::from_millis(300);
                let start_focus = ipc
                    .snapshot::<Window>(b"j/activewindow")
                    .await
                    .ok()
                    .and_then(|w| {
                        let class = if w.class.is_empty() {
                            &w.initial_class
                        } else {
                            &w.class
                        };
                        lock(&shared).config.canonical_class(class)
                    });
                lock(&shared).set_connected(true, start_focus);
                probe.notify_one();
                let _ = read_events(stream, &shared, &probe).await;
                lock(&shared).set_connected(false, None);
                lost_since = Instant::now();
            }
            Err(e) if lost_since.elapsed() > Duration::from_secs(14) => {
                if ever_connected {
                    eprintln!("irminsul: Hyprland session ended");
                    return Ok(());
                }
                return Err(e)
                    .with_context(|| format!("cannot connect to {}", ipc.events.display()));
            }
            Err(_) => {}
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(3));
    }
}

fn focus_class(line: &[u8]) -> Option<&str> {
    let body = line.strip_prefix(b"activewindow>>")?;
    let end = body.iter().position(|b| *b == b',').unwrap_or(body.len());
    std::str::from_utf8(&body[..end]).ok()
}

async fn read_events(stream: UnixStream, shared: &Shared, probe: &Notify) -> Result<()> {
    const MAX: usize = 32 * 1024;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::with_capacity(1024);
    let mut oversize = false;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(());
        }
        let newline = chunk.iter().position(|b| *b == b'\n');
        let n = newline.map_or(chunk.len(), |i| i + 1);
        if !oversize && line.len() + n <= MAX {
            line.extend_from_slice(&chunk[..n]);
        } else {
            oversize = true;
        }
        reader.consume(n);
        if newline.is_some() {
            if !oversize {
                let data = &line[..line.len() - 1];
                if let Some(class) = focus_class(data) {
                    let mut core = lock(shared);
                    let focus = core.config.canonical_class(class);
                    core.set_focus(focus);
                    let (lock_needed, _) = core.probe_targets();
                    if class.is_empty() && lock_needed {
                        probe.notify_one();
                    }
                } else if data.starts_with(b"monitoradded>>")
                    || data.starts_with(b"monitorremoved>>")
                {
                    probe.notify_one();
                }
            }
            line.clear();
            oversize = false;
        }
    }
}

pub async fn probe_loop(ipc: Ipc, shared: Shared, wake: Arc<Notify>) {
    let interval = lock(&shared).config.fallback_probe_seconds;
    let mut last_probe: Option<Instant> = None;
    loop {
        let targets = lock(&shared).probe_targets();
        if interval != 0 && targets != (false, false) {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(interval)) => {},
                _ = wake.notified() => {},
            }
        } else {
            wake.notified().await;
        }
        if let Some(previous) = last_probe {
            // A burst of hotplug / focus / idle events should not spam the command socket.
            let earliest = previous + Duration::from_secs(2);
            if Instant::now() < earliest {
                tokio::time::sleep_until(tokio::time::Instant::from_std(earliest)).await;
            }
        }
        let (needs_lock, needs_display) = lock(&shared).probe_targets();
        if !needs_lock && !needs_display {
            continue;
        }
        last_probe = Some(Instant::now());
        let locked = if needs_lock {
            ipc.snapshot::<Lock>(b"j/locked")
                .await
                .ok()
                .map(|r| r.locked)
        } else {
            None
        };
        let displayed = if needs_display {
            ipc.snapshot::<Vec<Monitor>>(b"j/monitors")
                .await
                .ok()
                .map(|r| r.iter().any(|m| m.on))
        } else {
            None
        };
        // A wake/idle or source change may have happened during the request.
        let mut core = lock(&shared);
        let current = core.probe_targets();
        if current != (false, false) {
            core.set_probe(locked, displayed);
        }
    }
}

pub fn doctor(config: &crate::config::Config) -> Result<()> {
    let ipc = Ipc::from_env()?;
    use std::os::unix::fs::FileTypeExt;
    let check = |p: &std::path::Path| -> bool {
        let ok = p.metadata().is_ok_and(|m| m.file_type().is_socket());
        println!(
            "{}  {}",
            if ok { "ok     " } else { "missing" },
            p.display()
        );
        ok
    };
    let events = check(&ipc.events);
    let command = check(&ipc.request);
    let wayland = match env::var_os("WAYLAND_DISPLAY") {
        Some(display) => {
            let p = PathBuf::from(display);
            let p = if p.is_absolute() {
                p
            } else {
                PathBuf::from(env::var_os("XDG_RUNTIME_DIR").unwrap_or_default()).join(p)
            };
            check(&p)
        }
        None => {
            env::var_os("WAYLAND_SOCKET").is_some()
                || check(
                    &PathBuf::from(env::var_os("XDG_RUNTIME_DIR").unwrap_or_default())
                        .join("wayland-0"),
                )
        }
    };
    println!("        http://127.0.0.1:{}/ (daemon URL)", config.port);
    if !events || !command || (!wayland && config.idle_seconds != 0) {
        return Err(anyhow!(
            "session sockets are missing; run the command from a Hyprland terminal"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_only_class_and_tolerates_arbitrary_title_bytes() {
        assert_eq!(
            focus_class(b"activewindow>>firefox,a,title,with,commas"),
            Some("firefox")
        );
        assert_eq!(focus_class(b"activewindow>>,"), Some(""));
        assert_eq!(focus_class(b"activewindow>>obsi,\xff"), Some("obsi"));
        assert_eq!(focus_class(b"windowtitlev2>>firefox,title"), None);
    }
}
