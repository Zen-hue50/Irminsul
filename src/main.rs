mod capture;
mod config;
mod demo;
mod desktop;
mod migrate;
mod store;
mod tracker;
mod wayland;
mod web;

use anyhow::{Context, Result, bail, ensure};
use config::{Config, Paths};
use std::{
    fs,
    net::{Ipv4Addr, SocketAddrV4},
    os::fd::AsRawFd,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

const HELP: &str = "Irminsul — digital wellbeing for Hyprland

Usage: irminsul <command>
  init                  Write the default config (never overwrites)
  migrate               Import config and usage from an earlier installation
  run                   Start tracking and serve the dashboard on loopback
  demo [--port PORT]    Show sample history without using the real database
  theme init            Copy editable dashboard files to your config directory
  paths                 Print config, theme, and database paths
  doctor                Check Hyprland / Wayland socket paths and configuration
  app-info <class>      Show the detected category for a window class
  --version             Print the version
";

fn main() {
    // There is no worker thread yet. Make new config, database and sidecar files private.
    unsafe {
        libc::umask(0o077);
    }
    if let Err(e) = command() {
        eprintln!("irminsul: {e:#}");
        std::process::exit(1);
    }
}

fn command() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["help" | "-h" | "--help"] => print!("{HELP}"),
        ["--version" | "-V"] => println!("irminsul {}", env!("CARGO_PKG_VERSION")),
        ["init"] => {
            let p = Paths::discover()?;
            fs::create_dir_all(&p.config_dir)?;
            let created = config::write_new(&p.config(), config::SAMPLE.as_bytes())?;
            let c = Config::load(&p.config())?;
            println!(
                "{} {}",
                if created { "Created" } else { "Kept" },
                p.config().display()
            );
            println!("Dashboard: http://127.0.0.1:{}/", c.port);
        }
        ["migrate"] => migrate::run(&Paths::discover()?)?,
        ["theme", "init"] => web::theme_init(&Paths::discover()?)?,
        ["paths"] => {
            let p = Paths::discover()?;
            println!(
                "Config    {}\nTheme     {}\nDatabase  {}",
                p.config().display(),
                p.web().display(),
                p.db().display()
            );
        }
        ["doctor"] => {
            let p = Paths::discover()?;
            let c = Config::load(&p.config())?;
            println!(
                "Config    {}\nDatabase  {}",
                p.config().display(),
                p.db().display()
            );
            capture::doctor(&c)?;
        }
        ["app-info", class] => {
            let paths = Paths::discover()?;
            let mut c = Config::load(&paths.config())?;
            let id = c
                .canonical_class(class)
                .context("this class is empty or excluded")?;
            c.desktop_index = desktop::DesktopIndex::discover();
            let app = c.app(&id);
            println!(
                "Class     {id}\nName      {}\nCategory  {}\nSource    {}",
                app.info.name, app.info.category, app.category_source
            );
        }
        ["run"] => {
            let paths = Paths::discover()?;
            let ipc = capture::Ipc::from_env()?;
            let c = Config::load(&paths.config())?;
            runtime()?.block_on(serve(c, Some((paths, ipc)), false))?;
        }
        ["demo", rest @ ..] => {
            let (port, preview) = demo_flags(rest)?;
            let c = Config {
                port,
                ..Config::default()
            };
            runtime()?.block_on(serve(c, None, preview))?;
        }
        _ => bail!("invalid command; try `irminsul --help`"),
    }
    Ok(())
}

fn demo_flags(mut flags: &[&str]) -> Result<(u16, bool)> {
    let mut port = None;
    let mut preview = false;
    while !flags.is_empty() {
        match flags {
            ["--port", value, rest @ ..] if port.is_none() => {
                let value = value.parse().context("invalid demo port")?;
                config::validate_port(value)?;
                port = Some(value);
                flags = rest;
            }
            ["--preview", rest @ ..] if !preview => {
                preview = true;
                flags = rest;
            }
            _ => bail!("usage: irminsul demo [--port PORT] [--preview]"),
        }
    }
    Ok((port.unwrap_or(8765), preview))
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    // One reactor thread handles sockets, signals, and low-cadence checkpoints.
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

fn lock_data_dir(data_dir: &Path) -> Result<fs::File> {
    fs::create_dir_all(data_dir)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data_dir.join("daemon.lock"))?;
    // Kernel releases the advisory lock on close, including after an unclean exit.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    ensure!(
        rc == 0,
        "another Irminsul daemon holds the data-directory lock ({})",
        std::io::Error::last_os_error()
    );
    Ok(file)
}

async fn serve(
    mut config: Config,
    live: Option<(Paths, capture::Ipc)>,
    preview: bool,
) -> Result<()> {
    let _instance = live
        .as_ref()
        .map(|(paths, _)| lock_data_dir(&paths.data_dir))
        .transpose()?;
    let port = config.port;
    let addr = SocketAddrV4::new(
        if preview {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        },
        port,
    );
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| {
        format!("cannot listen on {addr}; check whether the port is already in use")
    })?;
    let (db, overrides) = if let Some((paths, _)) = &live {
        (
            store::Store::open(&paths.db())
                .with_context(|| format!("opening {}", paths.db().display()))?,
            Some(paths.web()),
        )
    } else {
        let mut db = store::Store::memory()?;
        demo::seed(&mut db)?;
        (db, None)
    };
    if live.is_some() {
        config.desktop_index = desktop::DesktopIndex::discover();
    }
    let shared = Arc::new(Mutex::new(tracker::Core::new(config, db, live.is_none())?));
    let probe = Arc::new(Notify::new());
    let app = web::router(shared.clone(), overrides, preview, port, probe.clone());
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let stop = async {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {}, }
    };
    if preview {
        eprintln!("irminsul: preview on {addr}; synthetic data only");
    } else {
        eprintln!(
            "irminsul: dashboard at http://{addr}/{}",
            if live.is_none() {
                " (demo data only)"
            } else {
                ""
            }
        );
    }
    let server = axum::serve(listener, app);
    if let Some((_, ipc)) = live {
        tokio::spawn(wayland::signal_loop(shared.clone(), probe.clone()));
        tokio::spawn(capture::probe_loop(
            ipc.clone(),
            shared.clone(),
            probe.clone(),
        ));
        tokio::spawn(tracker::checkpoint_loop(shared.clone()));
        let result: Result<()> = tokio::select! {
            _ = stop => Ok(()),
            r = capture::focus_loop(ipc, shared.clone(), probe) => r,
            r = server => r.context("dashboard server"),
        };
        tracker::lock(&shared).flush()?;
        result
    } else {
        tokio::select! { _ = stop => Ok(()), r = server => r.context("demo server"), }
    }
}
