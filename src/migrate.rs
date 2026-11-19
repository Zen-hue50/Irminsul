//! One-time migration for installs made before the project was renamed.
use crate::config::Paths;
use anyhow::{Context, Result, ensure};
use std::{fs, io, path::Path};

const PREVIOUS_ID: &str = "stilltime";

pub fn run(paths: &Paths) -> Result<()> {
    let old_config = paths.config_dir.with_file_name(PREVIOUS_ID);
    let old_data = paths.data_dir.with_file_name(PREVIOUS_ID);
    let config = check_directory(&old_config, &paths.config_dir)?;
    let data = check_directory(&old_data, &paths.data_dir)?;
    if !config && !data {
        return Ok(());
    }
    // Do not move SQLite or its WAL while the earlier daemon is writing. The
    // lock stays held through the rename and is released when this command exits.
    let _lock = data.then(|| crate::lock_data_dir(&old_data)).transpose().with_context(|| {
        format!("cannot migrate while the previous daemon is running. Find it with `pgrep -a -x {PREVIOUS_ID}`, stop it with `kill -TERM PID`, and rerun the installer")
    })?;
    if config {
        archive_templates(&old_config)?;
        move_dir(&old_config, &paths.config_dir)?;
    }
    if data {
        if let Err(e) = move_dir(&old_data, &paths.data_dir) {
            if config {
                if let Err(rollback) = move_dir(&paths.config_dir, &old_config) {
                    eprintln!(
                        "irminsul: could not restore the earlier configuration path: {rollback:#}"
                    );
                }
            }
            return Err(e);
        }
        println!(
            "Migrated usage and saved preferences to {}",
            paths.data_dir.display()
        );
    }
    if config {
        println!(
            "Migrated configuration and theme assets to {}",
            paths.config_dir.display()
        );
        if paths.config_dir.join("previous-dashboard").is_dir() {
            println!(
                "Earlier HTML/JavaScript templates were preserved in {} so the new palette picker can load.",
                paths.config_dir.join("previous-dashboard").display()
            );
        }
    }
    Ok(())
}

fn present(path: &Path) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("checking {}", path.display())),
    }
}

fn check_directory(old: &Path, new: &Path) -> Result<bool> {
    if !present(old)? {
        return Ok(false);
    }
    ensure!(
        old.is_dir(),
        "previous path is not a directory: {}",
        old.display()
    );
    ensure!(
        !present(new)?,
        "both {} and {} exist. Neither was changed. Back up and move the new directory if you want to import the earlier one, then rerun the installer",
        old.display(),
        new.display()
    );
    Ok(true)
}

fn move_dir(old: &Path, new: &Path) -> Result<()> {
    fs::rename(old, new).with_context(|| format!("moving {} to {}", old.display(), new.display()))
}

fn archive_templates(config: &Path) -> Result<()> {
    let web = config.join("web");
    if !present(&web)? {
        return Ok(());
    }
    let backup = config.join("previous-dashboard");
    if web.symlink_metadata()?.file_type().is_symlink() {
        // Don't write through a theme directory symlink into the user's dotfiles.
        ensure!(
            !present(&backup)?,
            "theme backup already exists: {}",
            backup.display()
        );
        return move_dir(&web, &backup);
    }
    ensure!(
        web.is_dir(),
        "theme path is not a directory: {}",
        web.display()
    );
    for name in ["index.html", "app.js"] {
        let active = web.join(name);
        if present(&active)? {
            fs::create_dir_all(&backup)?;
            ensure!(
                !present(&backup.join(name))?,
                "theme backup already exists: {}",
                backup.join(name).display()
            );
            move_dir(&active, &backup.join(name))?;
        }
    }
    let old_icon = web.join(format!("{PREVIOUS_ID}.svg"));
    let new_icon = web.join("irminsul.svg");
    if present(&old_icon)? {
        if present(&new_icon)? {
            fs::create_dir_all(&backup)?;
            let saved = backup.join("previous-icon.svg");
            ensure!(
                !present(&saved)?,
                "theme backup already exists: {}",
                saved.display()
            );
            move_dir(&old_icon, &saved)?;
        } else {
            move_dir(&old_icon, &new_icon)?;
        }
    }
    Ok(())
}
