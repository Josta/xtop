//! UI preferences are separate from user-authored grouping configuration.
use crate::model::{App, Sort, View};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    view: View,
    sort: Sort,
    show_system: bool,
    workload_tree: bool,
    depth: Option<usize>,
    details: bool,
    mouse: bool,
    show_pid: bool,
    show_age: bool,
    show_cpu: bool,
    show_pss: bool,
    show_swap: bool,
    show_total: bool,
    measurement: crate::history::Measurement,
}
impl Default for Preferences {
    fn default() -> Self {
        Self::from(&App::new(Default::default(), Default::default()))
    }
}
impl From<&App> for Preferences {
    fn from(a: &App) -> Self {
        Self {
            view: a.view,
            sort: a.sort,
            show_system: a.show_system,
            workload_tree: a.workload_tree,
            depth: a.depth_limit,
            details: a.details,
            mouse: a.config.mouse,
            show_pid: a.show_pid,
            show_age: a.show_age,
            show_cpu: a.show_cpu,
            show_pss: a.show_pss,
            show_swap: a.show_swap,
            show_total: a.show_total,
            measurement: a.measurement,
        }
    }
}
impl Preferences {
    pub fn apply(&self, a: &mut App) {
        a.view = self.view;
        a.sort = self.sort;
        a.show_system = self.show_system;
        a.workload_tree = self.workload_tree;
        a.depth_limit = self.depth;
        a.details = self.details;
        a.config.mouse = self.mouse;
        a.show_pid = self.show_pid;
        a.show_age = self.show_age;
        a.show_cpu = self.show_cpu;
        a.show_pss = self.show_pss;
        a.show_swap = self.show_swap;
        a.show_total = self.show_total;
        a.measurement = self.measurement;
    }
}
pub fn path(config: &Path) -> PathBuf {
    config.with_extension("state.toml")
}

pub fn load(path: &Path) -> Result<Option<Preferences>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut p: Preferences = toml::from_str(&text).context("Reading UI preferences")?;
    let legacy: toml::Value = toml::from_str(&text)?;
    if legacy.get("measurement").is_none()
        && let Some(columns) = legacy.get("history_columns").and_then(|v| v.as_array())
    {
        let on = |i: usize| columns.get(i).and_then(|v| v.as_bool()).unwrap_or(false);
        p.show_cpu |= on(0) || on(1);
        p.show_pss |= on(2) || on(3);
        p.show_swap |= on(4) || on(5);
        let average = on(0) || on(2) || on(4);
        let peak = on(1) || on(3) || on(5);
        let five = legacy.get("history_window").and_then(|v| v.as_integer()) == Some(300);
        use crate::history::Measurement::*;
        p.measurement = match (average, peak, five) {
            (true, _, true) => Average5m,
            (true, _, false) => Average1m,
            (false, true, true) => Peak5m,
            (false, true, false) => Peak1m,
            _ => Live,
        };
    }
    ensure!(
        p.depth.is_none_or(|n| (1..=9).contains(&n)),
        "Invalid expansion depth"
    );
    Ok(Some(p))
}

pub fn save(path: &Path, preferences: &Preferences) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(preferences)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temporary = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.context("Saving UI preferences")
}

pub fn restore(a: &mut App) {
    let path = path(&a.config_path);
    match load(&path) {
        Ok(Some(p)) => p.apply(a),
        Ok(None) => {}
        Err(e) => a.status = format!("Could not restore {}: {e:#}", path.display()),
    }
}

pub fn save_if_changed(a: &mut App, saved: &mut Preferences) {
    let current = Preferences::from(&*a);
    if current != *saved {
        if let Err(e) = save(&path(&a.config_path), &current) {
            a.status = format!("Could not save settings: {e:#}");
        }
        *saved = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_round_trip_without_restoring_transient_state() {
        let mut a = App::new(Default::default(), "config.toml".into());
        a.view = View::Cgroups;
        a.sort = Sort::Cpu;
        a.show_system = true;
        a.workload_tree = false;
        a.depth_limit = Some(3);
        a.show_pid = false;
        a.show_age = false;
        a.show_cpu = false;
        a.show_pss = false;
        a.show_swap = false;
        a.show_total = true;
        a.details = false;
        a.config.mouse = false;
        a.measurement = crate::history::Measurement::Peak5m;
        a.filter = "temporary".into();
        a.paused = true;
        let prefs = Preferences::from(&a);
        let dir = PathBuf::from("target").join(format!(
            "preferences-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = dir.join("config.state.toml");
        assert!(load(&file).unwrap().is_none());
        save(&file, &prefs).unwrap();
        let loaded = load(&file).unwrap().unwrap();
        assert_eq!(loaded, prefs);
        let mut restored = App::new(Default::default(), Default::default());
        loaded.apply(&mut restored);
        assert_eq!(Preferences::from(&restored), prefs);
        assert!(!restored.paused);
        assert!(restored.filter.is_empty());
        restored.show_pid = true;
        save(&file, &Preferences::from(&restored)).unwrap();
        assert!(load(&file).unwrap().unwrap().show_pid);
        fs::write(&file, "measurement = 'invalid'").unwrap();
        assert!(load(&file).is_err());
        fs::write(&file, "show_cpu=false\nhistory_columns=[false,true,false,false,false,false]\nhistory_window=300").unwrap();
        let migrated = load(&file).unwrap().unwrap();
        assert!(migrated.show_cpu);
        assert_eq!(migrated.measurement, crate::history::Measurement::Peak5m);

        fs::remove_dir_all(dir).unwrap();
    }
}
