//! Subscribes to Wayland state changes on the daemon's existing async event loop.
//! No surfaces, input devices, frame callbacks, or display-control requests are used.
use crate::tracker::{Shared, Signals, lock};
use anyhow::{Context, Result, anyhow};
use std::{collections::BTreeMap, io, sync::Arc, time::Duration};
use tokio::{io::unix::AsyncFd, sync::Notify};
use wayland_backend::client::WaylandError;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop,
    protocol::{wl_callback, wl_output, wl_registry, wl_seat},
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1 as idle_event, ext_idle_notifier_v1 as idle_manager,
};
use wayland_protocols_wlr::output_power_management::v1::client::{
    zwlr_output_power_manager_v1 as power_manager, zwlr_output_power_v1 as power_event,
};

mod lock_protocol {
    #[allow(clippy::single_component_path_imports)] // scanner imports super::wayland_client
    use wayland_client;
    #[allow(clippy::all)]
    mod interfaces {
        wayland_scanner::generate_interfaces!("protocols/hyprland-lock-notify-v1.xml");
    }
    use self::interfaces::*;
    wayland_scanner::generate_client_code!("protocols/hyprland-lock-notify-v1.xml");
}
use lock_protocol::{
    hyprland_lock_notification_v1 as lock_event, hyprland_lock_notifier_v1 as lock_manager,
};

struct Output {
    output: wl_output::WlOutput,
    monitor: Option<power_event::ZwlrOutputPowerV1>,
    on: Option<bool>,
    failed: bool,
}
struct State {
    timeout: u32,
    respect_inhibitors: bool,
    seat: Option<(u32, wl_seat::WlSeat)>,
    idle_manager: Option<(u32, idle_manager::ExtIdleNotifierV1)>,
    idle_event: Option<idle_event::ExtIdleNotificationV1>,
    lock_manager: Option<(u32, lock_manager::HyprlandLockNotifierV1)>,
    lock_event: Option<lock_event::HyprlandLockNotificationV1>,
    power_manager: Option<(u32, power_manager::ZwlrOutputPowerManagerV1)>,
    outputs: BTreeMap<u32, Output>,
    signals: Signals,
    invalid: bool,
    bound: bool,
}

impl State {
    fn ensure_bindings(&mut self, qh: &QueueHandle<Self>) {
        if self.idle_event.is_none() && self.timeout != 0 {
            if let (Some((_, m)), Some((_, seat))) = (&self.idle_manager, &self.seat) {
                self.signals.input_only = m.version() >= 2 && !self.respect_inhibitors;
                let notification = if self.signals.input_only {
                    m.get_input_idle_notification(self.timeout, seat, qh, ())
                } else {
                    m.get_idle_notification(self.timeout, seat, qh, ())
                };
                // The protocol starts out non-idle until it sends the first idled event.
                self.signals.idle = Some(false);
                self.idle_event = Some(notification);
            }
        }
        if self.lock_event.is_none() {
            if let Some((_, m)) = &self.lock_manager {
                self.lock_event = Some(m.get_lock_notification(qh, ()));
                self.signals.locked = Some(false);
            }
        }
        if let Some((_, manager)) = &self.power_manager {
            for (id, o) in &mut self.outputs {
                if o.monitor.is_none() && !o.failed {
                    o.monitor = Some(manager.get_output_power(&o.output, qh, *id));
                }
            }
        }
    }

    fn observe(&mut self) -> Signals {
        self.signals.displayed = if self.power_manager.is_none() {
            None
        } else if self.outputs.values().any(|o| o.on == Some(true)) {
            Some(true)
        } else if self.outputs.values().all(|o| o.on == Some(false)) {
            Some(false)
        } else {
            None
        };
        if self.signals.initialized {
            self.signals
        } else {
            Signals::default()
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        s: &mut Self,
        registry: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        match e {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                match interface.as_str() {
                    "wl_seat" if s.seat.is_none() && s.timeout != 0 => {
                        s.seat = Some((name, registry.bind(name, 1, q, ())))
                    }
                    "ext_idle_notifier_v1" if s.idle_manager.is_none() && s.timeout != 0 => {
                        s.idle_manager = Some((name, registry.bind(name, version.min(2), q, ())))
                    }
                    "hyprland_lock_notifier_v1" if s.lock_manager.is_none() => {
                        s.lock_manager = Some((name, registry.bind(name, 1, q, ())))
                    }
                    "zwlr_output_power_manager_v1" if s.power_manager.is_none() => {
                        s.power_manager = Some((name, registry.bind(name, 1, q, ())))
                    }
                    "wl_output" => {
                        s.outputs.insert(
                            name,
                            Output {
                                output: registry.bind(name, version.min(3), q, ()),
                                monitor: None,
                                on: None,
                                failed: false,
                            },
                        );
                    }
                    _ => return,
                }
                if s.bound {
                    s.ensure_bindings(q);
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                if [
                    s.seat.as_ref().map(|(id, _)| *id),
                    s.idle_manager.as_ref().map(|(id, _)| *id),
                    s.lock_manager.as_ref().map(|(id, _)| *id),
                    s.power_manager.as_ref().map(|(id, _)| *id),
                ]
                .contains(&Some(name))
                {
                    s.invalid = true;
                }
                if let Some(o) = s.outputs.remove(&name) {
                    if let Some(m) = o.monitor {
                        m.destroy();
                    }
                    if o.output.version() >= 3 {
                        o.output.release();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, bool> for State {
    fn event(
        s: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        complete: &bool,
        conn: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if *complete {
            s.signals.initialized = true;
        } else {
            s.bound = true;
            s.ensure_bindings(q);
            // Wait for initial lock and display values before allowing capture.
            conn.display().sync(q, true);
        }
    }
}

impl Dispatch<idle_event::ExtIdleNotificationV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &idle_event::ExtIdleNotificationV1,
        e: idle_event::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            idle_event::Event::Idled => s.signals.idle = Some(true),
            idle_event::Event::Resumed => s.signals.idle = Some(false),
            _ => {}
        }
    }
}

impl Dispatch<lock_event::HyprlandLockNotificationV1, ()> for State {
    fn event(
        s: &mut Self,
        _: &lock_event::HyprlandLockNotificationV1,
        e: lock_event::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            lock_event::Event::Locked => s.signals.locked = Some(true),
            lock_event::Event::Unlocked => s.signals.locked = Some(false),
        }
    }
}

impl Dispatch<power_event::ZwlrOutputPowerV1, u32> for State {
    fn event(
        s: &mut Self,
        power: &power_event::ZwlrOutputPowerV1,
        e: power_event::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(out) = s.outputs.get_mut(id) {
            match e {
                power_event::Event::Mode {
                    mode: WEnum::Value(v),
                } => out.on = Some(v == power_event::Mode::On),
                power_event::Event::Failed => {
                    out.monitor = None;
                    out.on = None;
                    out.failed = true;
                    power.destroy();
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore wl_output::WlOutput);
delegate_noop!(State: ignore idle_manager::ExtIdleNotifierV1);
delegate_noop!(State: ignore lock_manager::HyprlandLockNotifierV1);
delegate_noop!(State: ignore power_manager::ZwlrOutputPowerManagerV1);

fn would_block(e: &WaylandError) -> bool {
    matches!(e, WaylandError::Io(e) if e.kind() == io::ErrorKind::WouldBlock)
}

async fn connection(shared: &Shared, probe: &Notify) -> Result<()> {
    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
    let poll_fd = conn.backend().poll_fd().try_clone_to_owned()?;
    // OwnedFd guarantees the descriptor is valid and unchanged until deregistration.
    let fd = unsafe { AsyncFd::register(poll_fd) }?;
    let mut queue = conn.new_event_queue::<State>();
    let q = queue.handle();
    let (timeout, respect_inhibitors) = {
        let c = &lock(shared).config;
        (c.idle_seconds * 1000, c.respect_idle_inhibitors)
    };
    let mut state = State {
        timeout,
        respect_inhibitors,
        seat: None,
        idle_manager: None,
        idle_event: None,
        lock_manager: None,
        lock_event: None,
        power_manager: None,
        outputs: BTreeMap::new(),
        signals: Signals::default(),
        invalid: false,
        bound: false,
    };
    conn.display().get_registry(&q, ());
    conn.display().sync(&q, false);
    let mut sent = Signals::default();
    loop {
        queue.dispatch_pending(&mut state)?;
        if state.invalid {
            return Err(anyhow!("Wayland session globals changed"));
        }
        let signals = state.observe();
        if signals != sent {
            if !sent.initialized && signals.initialized && timeout != 0 && signals.idle.is_none() {
                eprintln!("irminsul: compositor has no usable idle notifier; capture is waiting");
            }
            lock(shared).set_signals(signals);
            probe.notify_one();
            sent = signals;
        }
        match queue.flush() {
            Ok(()) => {}
            Err(ref e) if would_block(e) => {
                fd.writable().await?.clear_ready();
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        if let Some(read) = queue.prepare_read() {
            let mut ready = fd.readable().await?;
            match read.read() {
                Ok(_) => {}
                Err(ref e) if would_block(e) => ready.clear_ready(),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

pub async fn signal_loop(shared: Shared, probe: Arc<Notify>) {
    let mut delay = Duration::from_secs(1);
    loop {
        let started = std::time::Instant::now();
        if let Err(e) = connection(&shared, &probe).await {
            if delay == Duration::from_secs(1) || started.elapsed() > Duration::from_secs(15) {
                eprintln!("irminsul: {e:#}; reconnecting to Wayland");
            }
        }
        if started.elapsed() > Duration::from_secs(15) {
            delay = Duration::from_secs(1);
        }
        lock(&shared).set_signals(Signals::default());
        probe.notify_one();
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(15));
    }
}
