use crate::procfs::Process;
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::{Deserialize, Deserializer};
use std::{collections::HashMap, fs, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Pattern(Regex);
impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let pattern = String::deserialize(d)?;
        Regex::new(&pattern)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Matcher {
    pub exe_contains: Option<String>,
    pub command_contains: Option<String>,
    pub cwd_prefix: Option<String>,
    pub cgroup_contains: Option<String>,
    pub uid: Option<u32>,
    pub exe_regex: Option<Pattern>,
    pub command_regex: Option<Pattern>,
    pub cgroup_regex: Option<Pattern>,
    pub name_regex: Option<Pattern>,
}
impl Matcher {
    pub fn matches(&self, p: &Process) -> bool {
        self.exe_contains.as_ref().is_none_or(|v| p.exe.contains(v))
            && self
                .command_contains
                .as_ref()
                .is_none_or(|v| p.command.contains(v))
            && self
                .cwd_prefix
                .as_ref()
                .is_none_or(|v| std::path::Path::new(&p.cwd).starts_with(v))
            && self
                .cgroup_contains
                .as_ref()
                .is_none_or(|v| p.cgroup.contains(v))
            && self.uid.is_none_or(|v| p.uid == v)
            && self.exe_regex.as_ref().is_none_or(|v| v.0.is_match(&p.exe))
            && self
                .command_regex
                .as_ref()
                .is_none_or(|v| v.0.is_match(&p.command))
            && self
                .cgroup_regex
                .as_ref()
                .is_none_or(|v| v.0.is_match(&p.cgroup))
            && self
                .name_regex
                .as_ref()
                .is_none_or(|v| v.0.is_match(&p.name))
    }
    fn is_empty(&self) -> bool {
        self.exe_contains.is_none()
            && self.command_contains.is_none()
            && self.cwd_prefix.is_none()
            && self.cgroup_contains.is_none()
            && self.uid.is_none()
            && self.exe_regex.is_none()
            && self.command_regex.is_none()
            && self.cgroup_regex.is_none()
            && self.name_regex.is_none()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub refresh_ms: u64,
    pub memory_refresh_ms: u64,
    pub history_seconds: u64,
    pub mouse: bool,
    pub show_system: bool,
    pub workload_tree: bool,
    pub rules: Vec<Rule>,
    pub applications: Vec<Application>,
    #[serde(skip)]
    pub desktop_names: HashMap<String, String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            refresh_ms: 1000,
            memory_refresh_ms: 5000,
            history_seconds: 300,
            mouse: true,
            show_system: false,
            workload_tree: true,
            rules: vec![],
            applications: vec![],
            desktop_names: HashMap::new(),
        }
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub group: String,
    pub system: Option<bool>,
    pub service: Option<bool>,
    #[serde(flatten)]
    pub matcher: Matcher,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Application {
    pub name: String,
    pub system: Option<bool>,
    pub service: Option<bool>,
    #[serde(flatten)]
    pub matcher: Matcher,
}

pub fn default_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("xtop/config.toml")
}
impl Config {
    pub fn load(path: &PathBuf, explicit: bool) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => {
                let mut c = Self::default();
                c.read_desktop_names();
                return Ok(c);
            }
            Err(e) => return Err(e).with_context(|| format!("Reading {}", path.display())),
        };
        let mut c = Self::parse(&text).with_context(|| format!("Parsing {}", path.display()))?;
        c.read_desktop_names();
        Ok(c)
    }
    fn read_desktop_names(&mut self) {
        let mut dirs: Vec<PathBuf> = std::env::var_os("XDG_DATA_DIRS")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]);
        dirs.insert(
            0,
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
                }),
        );
        for dir in dirs.into_iter().rev() {
            let Ok(entries) = fs::read_dir(dir.join("applications")) else {
                continue;
            };
            for entry in entries.flatten() {
                let filename = entry.file_name().to_string_lossy().into_owned();
                let Some(id) = filename.strip_suffix(".desktop") else {
                    continue;
                };
                if entry.metadata().map_or(true, |m| m.len() > 64 * 1024) {
                    continue;
                }
                if let Ok(text) = fs::read_to_string(entry.path())
                    && let Some(name) = desktop_name(&text)
                {
                    self.desktop_names.insert(id.to_string(), name);
                }
            }
        }
    }
    pub fn parse(text: &str) -> Result<Self> {
        let c: Self = toml::from_str(text)?;
        if !(250..=60_000).contains(&c.refresh_ms)
            || !(1000..=60_000).contains(&c.memory_refresh_ms)
            || !(10..=3600).contains(&c.history_seconds)
        {
            bail!(
                "refresh_ms must be 250–60000; memory_refresh_ms 1000–60000; history_seconds 10–3600"
            );
        }
        for (name, matcher) in c
            .rules
            .iter()
            .map(|r| (&r.group, &r.matcher))
            .chain(c.applications.iter().map(|a| (&a.name, &a.matcher)))
        {
            if name.trim().is_empty() || name.chars().any(char::is_control) || matcher.is_empty() {
                bail!(
                    "Each rule/application needs a nonempty printable name and at least one matcher"
                );
            }
        }
        Ok(c)
    }
}

fn desktop_name(text: &str) -> Option<String> {
    let mut entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            entry = line == "[Desktop Entry]";
        } else if entry && let Some(name) = line.strip_prefix("Name=") {
            let name = crate::procfs::clean(name).trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_names_ignore_action_sections() {
        assert_eq!(
            desktop_name("[Desktop Action New]\nName=Wrong\n[Desktop Entry]\nName=gather.town"),
            Some("gather.town".into())
        );
    }
    #[test]
    fn config_validates_regexes_and_matchers() {
        assert!(Config::parse("[[applications]]\nname='Browser'\nexe_regex='['").is_err());
        assert!(Config::parse("[[applications]]\nname='Browser'").is_err());
        assert!(Config::parse("[[rules]]\ngroup='x'\nunknown_field='y'").is_err());
        assert!(Config::parse("[[rules]]\ngroup='x'\nexe_contains='codex'").is_ok());
        assert!(Config::parse("[[applications]]\nname='Browser'\nexe_regex='/chrome$'").is_ok());
    }
}
