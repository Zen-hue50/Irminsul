use crate::{
    config::{Config, validate_goal},
    store::{Bucket, Store, Totals},
};
use anyhow::{Result, ensure};
use chrono::{Days, Local, NaiveDate, TimeZone, Timelike, Utc};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[derive(Clone, Copy)]
struct Clock {
    wall_ms: i64,
    mono: Instant,
    boot_ms: Option<i64>,
}

impl Clock {
    fn now() -> Self {
        let mono = Instant::now();
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // CLOCK_BOOTTIME includes suspend; std::time::Instant on Linux does not.
        let ok = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) == 0 };
        #[allow(clippy::unnecessary_cast)] // timespec fields vary in width on Linux architectures
        let boot_ms = ok.then(|| (ts.tv_sec as i64) * 1_000 + (ts.tv_nsec as i64) / 1_000_000);
        Self {
            wall_ms: Utc::now().timestamp_millis(),
            mono,
            boot_ms,
        }
    }

    fn continuous_since(self, old: Self) -> bool {
        let mono = self.mono.saturating_duration_since(old.mono).as_millis() as i64;
        let wall = self.wall_ms - old.wall_ms;
        if wall < 0 || (wall - mono).abs() > 1_500 {
            return false;
        }
        match (old.boot_ms, self.boot_ms) {
            // Drop an ambiguous slice across suspend; never attribute suspend time to an app.
            (Some(a), Some(b)) => b - a - mono < 950,
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Signals {
    pub initialized: bool,
    pub idle: Option<bool>,
    pub input_only: bool,
    pub locked: Option<bool>,
    pub displayed: Option<bool>,
}

#[derive(Serialize)]
pub struct Session {
    pub status: &'static str,
    pub current_app: Option<String>,
    pub idle_detection: &'static str,
    pub lock_source: &'static str,
    pub display_source: &'static str,
    pub storage_ok: bool,
}

#[derive(Clone, Copy)]
pub struct Range {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub days: u32,
}

impl Range {
    pub fn new(days: u32, end: Option<NaiveDate>, today: NaiveDate) -> Result<Self> {
        ensure!(matches!(days, 1 | 7 | 30), "days must be 1, 7, or 30");
        let end = end.unwrap_or(today);
        ensure!(
            end <= today && end >= NaiveDate::from_ymd_opt(1900, 1, 1).unwrap(),
            "end must be a date between 1900-01-01 and today"
        );
        Ok(Self {
            end,
            start: end - Days::new(u64::from(days - 1)),
            days,
        })
    }
}

#[derive(Serialize)]
pub struct Daily {
    pub date: String,
    pub ms: u64,
    pub switches: u64,
}

#[derive(Serialize)]
pub struct Hourly {
    pub hour: u32,
    pub ms: u64,
    pub switches: u64,
}

#[derive(Serialize)]
pub struct AppTotal {
    pub id: String,
    pub name: String,
    pub category: String,
    pub icon: String,
    pub category_source: &'static str,
    pub ms: u64,
    pub switches: u64,
    pub daily: Vec<u64>,
    pub daily_switches: Vec<u64>,
    pub hourly: Vec<u64>,
}

#[derive(Serialize)]
pub struct Group {
    pub name: String,
    pub ms: u64,
}

#[derive(Serialize)]
pub struct Report {
    pub demo: bool,
    pub today: String,
    pub start: String,
    pub end: String,
    pub days: u32,
    pub total_ms: u64,
    pub unattributed_ms: u64,
    pub unattributed_daily_ms: Vec<u64>,
    pub previous_total_ms: u64,
    pub switches: u64,
    pub daily: Vec<Daily>,
    pub hourly: Vec<Hourly>,
    pub apps: Vec<AppTotal>,
    pub categories: Vec<Group>,
    pub session: Session,
    pub goal_minutes: u16,
    pub retention_days: u32,
    pub idle_seconds: u32,
    pub paused: bool,
}

pub struct Core {
    pub config: Config,
    db: Store,
    pub demo: bool,
    last: Clock,
    pending: Totals,
    prune_day: Option<NaiveDate>,
    deadline: Option<Instant>,
    storage_ok: bool,
    connected: bool,
    focused: Option<String>,
    signals: Signals,
    probed_lock: Option<bool>,
    probed_display: Option<bool>,
    last_app: Option<String>,
    paused: bool,
    goal: u16,
    saver: Arc<Notify>,
}

pub type Shared = Arc<Mutex<Core>>;
pub fn lock(shared: &Shared) -> MutexGuard<'_, Core> {
    shared.lock().expect("tracker lock was poisoned")
}

impl Core {
    pub fn new(config: Config, db: Store, demo: bool) -> Result<Self> {
        let paused = db.preference("paused")?.unwrap_or(0) != 0;
        let goal = db
            .preference("goal_minutes")?
            .unwrap_or(config.daily_goal_minutes);
        let mut core = Self {
            config,
            db,
            demo,
            last: Clock::now(),
            pending: Totals::new(),
            prune_day: None,
            deadline: None,
            storage_ok: true,
            connected: false,
            focused: None,
            signals: Signals::default(),
            probed_lock: None,
            probed_display: None,
            last_app: None,
            paused,
            goal,
            saver: Arc::new(Notify::new()),
        };
        if !demo {
            core.flush()?;
        }
        Ok(core)
    }

    fn reason(&self) -> &'static str {
        if self.paused {
            return "paused";
        }
        if self.demo {
            return "demo";
        }
        if !self.connected || (self.config.idle_seconds != 0 && self.signals.idle.is_none()) {
            return "waiting";
        }
        if self.signals.locked.or(self.probed_lock) == Some(true) {
            return "locked";
        }
        if self.signals.displayed.or(self.probed_display) == Some(false) {
            return "display-off";
        }
        if self.signals.idle == Some(true) {
            return "idle";
        }
        "tracking"
    }

    fn active(&self) -> bool {
        self.reason() == "tracking"
    }

    pub fn session(&self) -> Session {
        let status = self.reason();
        let current_app = if status == "tracking" {
            self.focused
                .as_ref()
                .map(|id| self.config.app(id).info.name)
        } else {
            None
        };
        Session {
            status,
            current_app,
            idle_detection: if self.config.idle_seconds == 0 {
                "disabled"
            } else if self.signals.idle.is_none() {
                "unavailable"
            } else if self.signals.input_only {
                "input-only"
            } else {
                "inhibitor-aware"
            },
            lock_source: signal_source(self.signals.locked, self.probed_lock),
            display_source: signal_source(self.signals.displayed, self.probed_display),
            storage_ok: self.storage_ok,
        }
    }

    fn advance(&mut self, at: Clock) {
        if self.active() {
            if at.continuous_since(self.last) {
                accumulate(
                    &mut self.pending,
                    &Local,
                    self.last.wall_ms,
                    at.wall_ms,
                    self.focused.as_deref().unwrap_or(""),
                );
            } else {
                self.last_app = None;
            }
        }
        self.last = at;
    }

    fn update(&mut self, at: Clock, apply: impl FnOnce(&mut Self)) {
        self.advance(at);
        apply(self);
        if self.active() {
            if let (Some(previous), Some(next)) = (self.last_app.as_ref(), self.focused.as_ref()) {
                if previous != next {
                    let (date, hour) = date_hour(&Local, at.wall_ms);
                    self.pending
                        .entry(Bucket {
                            date,
                            hour,
                            app: next.clone(),
                        })
                        .or_default()
                        .switches += 1;
                }
            }
            self.last_app.clone_from(&self.focused);
        } else {
            self.last_app = None;
        }
        if self.deadline.is_none() && (self.active() || !self.pending.is_empty()) {
            self.deadline = Some(at.mono + Duration::from_secs(self.config.checkpoint_seconds));
            self.saver.notify_one();
        }
    }

    pub fn set_connected(&mut self, connected: bool, focus: Option<String>) {
        self.update(Clock::now(), |c| {
            c.connected = connected;
            c.focused = focus;
            if !connected {
                c.probed_lock = None;
                c.probed_display = None;
            }
        });
    }

    pub fn set_focus(&mut self, focus: Option<String>) {
        if self.focused != focus {
            self.update(Clock::now(), |c| c.focused = focus);
        }
    }

    pub fn set_signals(&mut self, signals: Signals) {
        if self.signals != signals {
            self.update(Clock::now(), |c| {
                if c.signals.initialized && !signals.initialized {
                    c.probed_lock = None;
                    c.probed_display = None;
                }
                c.signals = signals;
            });
        }
    }

    pub fn set_probe(&mut self, lock: Option<bool>, display: Option<bool>) {
        if (lock.is_some() && lock != self.probed_lock)
            || (display.is_some() && display != self.probed_display)
        {
            self.update(Clock::now(), |c| {
                if lock.is_some() && c.signals.locked.is_none() {
                    c.probed_lock = lock;
                }
                if display.is_some() && c.signals.displayed.is_none() {
                    c.probed_display = display;
                }
            });
        }
    }

    /// Probe missing compositor signals only. Never poll a paused or idle session.
    pub fn probe_targets(&self) -> (bool, bool) {
        if !self.connected
            || !self.signals.initialized
            || self.paused
            || self.signals.idle == Some(true)
            || (self.config.idle_seconds != 0 && self.signals.idle.is_none())
        {
            (false, false)
        } else {
            (
                self.signals.locked.is_none(),
                self.signals.displayed.is_none(),
            )
        }
    }

    pub fn set_preferences(&mut self, paused: Option<bool>, goal: Option<u16>) -> Result<()> {
        if let Some(minutes) = goal {
            validate_goal(minutes)?;
            self.db.set_preference("goal_minutes", minutes)?;
            self.goal = minutes;
        }
        if let Some(paused) = paused {
            if self.paused != paused {
                self.db.set_preference("paused", u16::from(paused))?;
                self.update(Clock::now(), |c| c.paused = paused);
            }
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let now = Clock::now();
        self.advance(now);
        let today = Local::now().date_naive();
        let limit = if self.config.retention_days != 0 && self.prune_day != Some(today) {
            today.checked_sub_days(Days::new(u64::from(self.config.retention_days - 1)))
        } else {
            None
        };
        let result = self.db.save(&self.pending, limit);
        self.storage_ok = result.is_ok();
        if self.storage_ok {
            self.pending.clear();
            self.prune_day = Some(today);
        }
        self.deadline = if self.active() || !self.pending.is_empty() {
            Some(now.mono + Duration::from_secs(self.config.checkpoint_seconds))
        } else {
            None
        };
        result
    }

    pub fn report(&mut self, days: u32, end: Option<NaiveDate>) -> Result<Report> {
        self.advance(Clock::now());
        let today = Local::now().date_naive();
        let range = Range::new(days, end, today)?;
        let seq = range.start - Days::new(u64::from(days));
        let mut totals = self.db.range(seq, range.end)?;
        for (key, val) in &self.pending {
            if (seq..=range.end).contains(&key.date) {
                totals.entry(key.clone()).or_default().merge(*val);
            }
        }
        let mut report = Report {
            demo: self.demo,
            today: today.to_string(),
            start: range.start.to_string(),
            end: range.end.to_string(),
            days: range.days,
            total_ms: 0,
            unattributed_ms: 0,
            unattributed_daily_ms: vec![0; days as usize],
            previous_total_ms: 0,
            switches: 0,
            daily: (0..days)
                .map(|offset| Daily {
                    date: (range.start + Days::new(u64::from(offset))).to_string(),
                    ms: 0,
                    switches: 0,
                })
                .collect(),
            hourly: (0..24)
                .map(|hour| Hourly {
                    hour,
                    ms: 0,
                    switches: 0,
                })
                .collect(),
            apps: Vec::new(),
            categories: Vec::new(),
            session: self.session(),
            goal_minutes: self.goal,
            retention_days: self.config.retention_days,
            idle_seconds: self.config.idle_seconds,
            paused: self.paused,
        };
        let mut apps = BTreeMap::new();
        let mut groups: BTreeMap<String, u64> = BTreeMap::new();
        for (key, val) in totals {
            if key.date < range.start {
                report.previous_total_ms += val.ms;
                continue;
            }
            let day = (key.date - range.start).num_days() as usize;
            let hour = key.hour as usize;
            report.total_ms += val.ms;
            report.switches += val.switches;
            report.daily[day].ms += val.ms;
            report.daily[day].switches += val.switches;
            report.hourly[hour].ms += val.ms;
            report.hourly[hour].switches += val.switches;
            if key.app.is_empty() {
                report.unattributed_ms += val.ms;
                report.unattributed_daily_ms[day] += val.ms;
                *groups.entry("Desktop & excluded".into()).or_default() += val.ms;
                continue;
            }
            let app = apps.entry(key.app.clone()).or_insert_with(|| {
                let resolved = self.config.app(&key.app);
                let info = resolved.info;
                AppTotal {
                    id: key.app,
                    name: info.name,
                    category: info.category,
                    icon: info.icon,
                    category_source: resolved.category_source,
                    ms: 0,
                    switches: 0,
                    daily: vec![0; days as usize],
                    daily_switches: vec![0; days as usize],
                    hourly: vec![0; 24],
                }
            });
            *groups.entry(app.category.clone()).or_default() += val.ms;
            app.ms += val.ms;
            app.switches += val.switches;
            app.daily[day] += val.ms;
            app.daily_switches[day] += val.switches;
            app.hourly[hour] += val.ms;
        }
        report.apps = apps.into_values().collect();
        report
            .apps
            .sort_by(|a, b| b.ms.cmp(&a.ms).then(a.name.cmp(&b.name)));
        report.categories = groups
            .into_iter()
            .filter(|(_, ms)| *ms != 0)
            .map(|(name, ms)| Group { name, ms })
            .collect();
        report
            .categories
            .sort_by(|a, b| b.ms.cmp(&a.ms).then(a.name.cmp(&b.name)));
        Ok(report)
    }
}

fn signal_source(native: Option<bool>, probed: Option<bool>) -> &'static str {
    if native.is_some() {
        "wayland"
    } else if probed.is_some() {
        "snapshot"
    } else {
        "unavailable"
    }
}

fn date_hour(tz: &impl TimeZone, ms: i64) -> (NaiveDate, u32) {
    let t = tz
        .timestamp_millis_opt(ms)
        .single()
        .expect("clock within chrono's range");
    (t.date_naive(), t.hour())
}

fn accumulate(acc: &mut Totals, tz: &impl TimeZone, mut start: i64, end: i64, app: &str) {
    while start < end {
        let at = tz
            .timestamp_millis_opt(start)
            .single()
            .expect("clock within chrono's range");
        let label = (at.date_naive(), at.hour());
        let until_hour = 3_600_000
            - i64::from(at.minute()) * 60_000
            - i64::from(at.second()) * 1_000
            - i64::from(at.timestamp_subsec_millis());
        let mut next = end.min(start + until_hour);
        if date_hour(tz, next - 1) != label {
            // A clock-offset transition can change the hour before the next nominal boundary.
            let (mut lo, mut hi) = (start, next - 1);
            while hi - lo > 1 {
                let mid = lo + (hi - lo) / 2;
                if date_hour(tz, mid) == label {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            next = hi;
        }
        acc.entry(Bucket {
            date: label.0,
            hour: label.1,
            app: app.to_owned(),
        })
        .or_default()
        .ms += (next - start) as u64;
        start = next;
    }
}

pub async fn checkpoint_loop(shared: Shared) {
    let saver = lock(&shared).saver.clone();
    loop {
        let deadline = lock(&shared).deadline;
        if let Some(deadline) = deadline {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            if let Err(e) = lock(&shared).flush() {
                eprintln!("irminsul: could not save usage: {e:#}");
            }
        } else {
            saver.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Count;

    fn fake_clock(base: Clock, shift: u64) -> Clock {
        Clock {
            wall_ms: base.wall_ms + shift as i64,
            mono: base.mono + Duration::from_millis(shift),
            boot_ms: base.boot_ms.map(|b| b + shift as i64),
        }
    }

    #[test]
    fn splits_local_midnight_with_non_hour_offset() {
        let tz = chrono::FixedOffset::east_opt(5 * 3600 + 30 * 60).unwrap();
        let start = tz
            .with_ymd_and_hms(2026, 11, 17, 23, 59, 59)
            .unwrap()
            .timestamp_millis();
        let mut t = Totals::new();
        accumulate(&mut t, &tz, start, start + 3000, "code");
        let key = |day, hour| Bucket {
            date: NaiveDate::from_ymd_opt(2026, 11, day).unwrap(),
            hour,
            app: "code".into(),
        };
        assert_eq!(t[&key(17, 23)].ms, 1000);
        assert_eq!(t[&key(18, 0)].ms, 2000);
    }

    #[test]
    fn pauses_when_inactive_and_switches_only_between_apps() {
        let mut c = Core::new(Config::default(), Store::memory().unwrap(), false).unwrap();
        let t = c.last;
        c.update(t, |c| {
            c.connected = true;
            c.signals.idle = Some(false);
            c.focused = Some("code".into());
        });
        c.update(fake_clock(t, 2000), |c| c.focused = Some("firefox".into()));
        c.update(fake_clock(t, 4500), |c| c.signals.locked = Some(true));
        c.update(fake_clock(t, 7500), |c| c.focused = Some("kitty".into()));
        c.update(fake_clock(t, 9500), |c| c.signals.locked = Some(false));
        c.update(fake_clock(t, 10_500), |c| c.focused = None);
        c.update(fake_clock(t, 11_000), |c| c.signals.displayed = Some(false));
        c.advance(fake_clock(t, 16_000));
        let sum = |id: &str| {
            c.pending.iter().filter(|(k, _)| k.app == id).fold(
                Count::default(),
                |mut total, (_, v)| {
                    total.merge(*v);
                    total
                },
            )
        };
        assert_eq!(
            sum("code"),
            Count {
                ms: 2000,
                switches: 0
            }
        );
        assert_eq!(
            sum("firefox"),
            Count {
                ms: 2500,
                switches: 1
            }
        );
        assert_eq!(
            sum("kitty"),
            Count {
                ms: 1000,
                switches: 0
            }
        );
        assert_eq!(
            sum(""),
            Count {
                ms: 500,
                switches: 0
            }
        );
    }

    #[test]
    fn rejects_suspend_and_wall_clock_jumps() {
        let base = Clock {
            boot_ms: Some(100),
            ..Clock::now()
        };
        let t = fake_clock(base, 1100);
        assert!(t.continuous_since(base));
        assert!(
            !Clock {
                boot_ms: Some(20_000),
                ..t
            }
            .continuous_since(base)
        );
        assert!(
            !Clock {
                wall_ms: t.wall_ms + 7_000_000,
                ..t
            }
            .continuous_since(base)
        );
    }

    #[test]
    fn range_is_inclusive_and_future_is_rejected() {
        let date: NaiveDate = "2026-11-18".parse().unwrap();
        assert_eq!(
            Range::new(7, None, date).unwrap().start.to_string(),
            "2026-11-12"
        );
        assert!(Range::new(14, None, date).is_err());
        assert!(Range::new(1, Some(date + Days::new(1)), date).is_err());
    }
}
