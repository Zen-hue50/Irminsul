//! Synthetic demo data: never reads or writes the user's history or starts capture.
use crate::store::{Bucket, Count, Store, Totals};
use anyhow::Result;
use chrono::{Datelike, Days, Local, Timelike};

pub fn seed(db: &mut Store) -> Result<()> {
    let now = Local::now();
    let today = now.date_naive();
    let apps = [
        "helium", "code", "slack", "obsidian", "spotify", "kitty", "nautilus", "discord", "",
    ];
    let weights = [31, 27, 10, 8, 8, 5, 3, 4, 4];
    let minutes = [238, 322, 286, 147, 212, 288, 221];
    let switches = [23, 31, 26, 13, 18, 28, 17];
    let mut changes = Totals::new();
    for back in 0..75 {
        let date = today - Days::new(back);
        let index = 6 - back as usize % 7;
        let target = if back < 7 {
            minutes[index]
        } else {
            minutes[index] * (104 + (date.ordinal() % 20)) / 100
        };
        let target_switches = switches[index] + u32::from(back >= 7);
        let last_hour = if back == 0 {
            now.hour().clamp(13, 20)
        } else {
            21
        };
        let hours: Vec<u32> = (8..=last_hour).collect();
        for (n, id) in apps.iter().enumerate() {
            let prefix: u32 = weights[..n].iter().sum();
            let app_ms =
                (target * (prefix + weights[n]) / 100 - target * prefix / 100) as u64 * 60_000;
            let switch_weights: u32 = if n == 8 { 0 } else { weights[n] };
            let app_switches = if n == 8 {
                0
            } else {
                target_switches * (prefix + switch_weights) / 96 - target_switches * prefix / 96
            };
            let hour_weights: Vec<_> = hours
                .iter()
                .map(|h| {
                    let peak = match h {
                        9 | 10 | 14 | 15 => 10,
                        12 | 13 | 18 => 2,
                        11 | 16 | 20 => 7,
                        _ => 4,
                    };
                    (peak + (h + n as u32 + index as u32) % 4) as u64
                })
                .collect();
            let sum: u64 = hour_weights.iter().sum();
            let mut cumulative = 0;
            for (h, w) in hours.iter().zip(hour_weights) {
                let until = cumulative + w;
                changes.insert(
                    Bucket {
                        date,
                        hour: *h,
                        app: (*id).into(),
                    },
                    Count {
                        ms: app_ms * until / sum - app_ms * cumulative / sum,
                        switches: u64::from(app_switches) * until / sum
                            - u64::from(app_switches) * cumulative / sum,
                    },
                );
                cumulative = until;
            }
        }
    }
    db.save(&changes, None)
}
