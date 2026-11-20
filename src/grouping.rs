//! Presentation classification is independent of Unix ownership and cgroup placement.
use crate::{
    config::Config,
    procfs::{Process, Snapshot},
};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct Assignment {
    pub group: String,
    pub service: bool,
    pub session: Option<String>,
    pub system: bool,
    pub reason: String,
}
impl Assignment {
    pub fn bucket(&self) -> String {
        if let Some(session) = &self.session {
            format!("{}\x1f{session}", self.group)
        } else if self.service {
            format!("{}\x1fServices", self.group)
        } else {
            self.group.clone()
        }
    }
}

pub fn classify(p: &Process, s: &Snapshot, c: &Config) -> Assignment {
    let system = system_reason(p);
    for (i, r) in c.rules.iter().enumerate() {
        if r.matcher.matches(p) {
            return Assignment {
                session: None,
                group: r.group.clone(),
                system: r.system.unwrap_or(system.is_some()),
                service: r.service.unwrap_or(false),
                reason: format!("Configuration rule {}", i + 1),
            };
        }
    }
    if let Some(a) = configured_app(p, c) {
        return a;
    }
    if let Some(a) = known_app(p) {
        return a;
    }
    if let Some(reason) = system {
        return Assignment {
            session: None,
            group: if is_kernel(p) {
                "Kernel threads"
            } else {
                "System services"
            }
            .into(),
            system: true,
            service: false,
            reason: reason.into(),
        };
    }
    // Multiplexer servers own their descendants even after daemonizing away
    // from the terminal emulator. Clients remain in their launching terminal.
    let mut current = p;
    let mut visited = HashSet::new();
    let mut terminal = None;
    while visited.insert(current.id.pid) {
        if current.tty_nr != 0 {
            terminal = Some(terminal_section(current));
        }
        let exe = current.exe.rsplit('/').next().unwrap_or("");
        let owner = if current.name == "tmux: server" {
            Some("TMUX")
        } else if current.name.eq_ignore_ascii_case("screen") || exe == "screen" {
            Some("Screen")
        } else {
            None
        };
        if let Some(owner) = owner {
            let mut a = app(owner.into(), false, "Multiplexer process ancestry");
            a.session = terminal;
            return a;
        }
        let Some(parent) = s.processes.get(&current.ppid) else {
            break;
        };
        if system_reason(parent).is_some() {
            break;
        }
        current = parent;
    }
    // Pane metadata also covers descendants whose server is outside /proc visibility.
    let mut current = p;
    let mut visited = HashSet::new();
    let mut terminal = None;
    while visited.insert(current.id.pid) {
        if current.tty_nr != 0 && terminal.is_none() {
            terminal = Some(terminal_section(current));
        }
        if !current.session_title.is_empty() {
            let mut a = app("TMUX".into(), false, "Tmux pane terminal");
            a.session = Some(terminal_section(current));
            return a;
        }
        let Some(parent) = s.processes.get(&current.ppid) else {
            break;
        };
        if system_reason(parent).is_some() {
            break;
        }
        current = parent;
    }
    let mut current = p;
    let mut visited = HashSet::new();
    while visited.insert(current.id.pid) {
        if current.id != p.id
            && let Some(mut a) = configured_app(current, c).or_else(|| known_app(current))
        {
            a.reason = format!(
                "Application ancestry via PID {} ({})",
                current.id.pid, a.reason
            );
            a.service |= is_service(p);
            a.session = terminal;
            return a;
        }
        if let Some(scope) = current
            .cgroup
            .split('/')
            .find(|v| v.starts_with("app-") && (v.ends_with(".scope") || v.ends_with(".service")))
        {
            let mut a = app(
                friendly_scope(scope, &c.desktop_names),
                is_service(p),
                "Systemd application scope",
            );
            a.session = terminal;
            return a;
        }
        let Some(parent) = s.processes.get(&current.ppid) else {
            break;
        };
        if system_reason(parent).is_some() {
            break;
        }
        current = parent;
    }
    if let Some(terminal) = terminal {
        return Assignment {
            group: terminal,
            session: None,
            system: false,
            service: false,
            reason: "Terminal without an identifiable owning application".into(),
        };
    }
    if let Some(scope) = p
        .cgroup
        .split('/')
        .find(|v| v.starts_with("snap.") && v.ends_with(".scope"))
    {
        let name = scope.split('.').nth(1).unwrap_or("Snap application");
        return app(title(name), false, "Snap application scope");
    }
    Assignment {
        session: None,
        group: if is_service(p) {
            "Other services"
        } else {
            "Other processes"
        }
        .into(),
        system: false,
        service: false,
        reason: "No known application or system classification (Unix UID is not used)".into(),
    }
}

fn terminal_section(p: &Process) -> String {
    if p.tty.is_empty() {
        format!("Shell · tty:{}", p.tty_nr)
    } else {
        format!("Shell · {}", p.tty)
    }
}

fn configured_app(p: &Process, c: &Config) -> Option<Assignment> {
    c.applications
        .iter()
        .enumerate()
        .find(|(_, a)| a.matcher.matches(p))
        .map(|(i, a)| Assignment {
            session: None,
            group: format!("App · {}", a.name),
            system: a.system.unwrap_or(false),
            service: a.service.unwrap_or_else(|| is_service(p)),
            reason: format!("Application override {}", i + 1),
        })
}

fn app(name: String, service: bool, reason: &str) -> Assignment {
    Assignment {
        session: None,
        group: format!(
            "{} · {name}",
            if matches!(name.as_str(), "TMUX" | "Screen") {
                "Shell"
            } else {
                "App"
            }
        ),
        system: false,
        service,
        reason: reason.into(),
    }
}
fn known_app(p: &Process) -> Option<Assignment> {
    let exe = p.exe.to_lowercase();
    let cg = p.cgroup.to_lowercase();
    let name = p.name.to_lowercase();
    let is_docker = cg.split('/').any(|v| {
        v == "docker-desktop.service"
            || v == "docker.service"
            || v == "containerd.service"
            || v.starts_with("app-docker-desktop-")
    }) || exe.starts_with("/opt/docker-desktop/")
        || matches!(name.as_str(), "dockerd" | "containerd" | "docker desktop")
        || name.starts_with("com.docker.");
    if is_docker {
        return Some(app(
            "Docker".into(),
            is_service(p)
                || matches!(name.as_str(), "dockerd" | "containerd")
                || name.starts_with("com.docker."),
            "Docker executable/service identity",
        ));
    }
    // Embedded Chromium helpers belong to their host app, not to Chrome.
    if exe.starts_with("/usr/lib/slack/")
        || exe.starts_with("/opt/slack/")
        || cg
            .split('/')
            .any(|v| v.starts_with("app-slack-") || v.starts_with("app-slack@"))
    {
        return Some(app(
            "Slack".into(),
            is_service(p),
            "Slack executable/application scope",
        ));
    }
    if exe.starts_with("/snap/chromium/")
        || cg.split('/').any(|v| v.starts_with("snap.chromium."))
        || matches!(name.as_str(), "chromium" | "chromium-browse")
    {
        return Some(app(
            "Chromium".into(),
            is_service(p),
            "Chromium executable/Snap scope",
        ));
    }
    if exe.starts_with("/opt/google/chrome/")
        || matches!(name.as_str(), "google-chrome" | "google-chrome-st")
        || (name == "chrome"
            && exe.ends_with("/chrome")
            && !exe.contains("playwright")
            && !exe.contains("puppeteer"))
    {
        return Some(app(
            "Chrome".into(),
            is_service(p),
            "Chrome executable identity",
        ));
    }
    if exe.contains("/firefox/")
        || name == "firefox"
        || cg.split('/').any(|v| v.starts_with("snap.firefox."))
    {
        return Some(app(
            "Firefox".into(),
            is_service(p),
            "Firefox executable/application scope",
        ));
    }
    None
}

fn is_service(p: &Process) -> bool {
    // The user@ manager is an ancestor of *all* desktop apps, not their service.
    p.cgroup
        .rsplit('/')
        .find(|v| v.ends_with(".scope") || v.ends_with(".service"))
        .is_some_and(|v| {
            v.ends_with(".service") && !v.starts_with("user@") && !v.starts_with("app-")
        })
}
fn is_kernel(p: &Process) -> bool {
    p.id.pid == 2 || (p.ppid == 2 && p.exe.is_empty())
}

fn system_reason(p: &Process) -> Option<&'static str> {
    if is_kernel(p) {
        return Some("Kernel thread");
    }
    if p.id.pid == 1 || p.cgroup.ends_with("/init.scope") {
        return Some("Init/process-root scope");
    }
    let name = p.name.as_str();
    if name.starts_with("systemd")
        || name.starts_with("startplasma")
        || name.starts_with("kwin_wayland")
        || name.starts_with("baloo_file")
        || matches!(
            name,
            "sddm"
                | "sddm-helper"
                | "ksecretd"
                | "kwalletd5"
                | "kwalletd6"
                | "plasmashell"
                | "kwin_wayland"
                | "kwin_x11"
                | "Xwayland"
                | "Xorg"
                | "kded5"
                | "kded6"
                | "ksmserver"
                | "kglobalacceld"
                | "kactivitymanage"
                | "kaccess"
                | "kscreen_backend"
                | "kdeconnectd"
                | "polkitd"
                | "polkit-kde-auth"
                | "dbus-daemon"
                | "dbus-broker"
                | "dbus-broker-lau"
                | "pipewire"
                | "pipewire-pulse"
                | "wireplumber"
                | "NetworkManager"
                | "wpa_supplicant"
                | "bluetoothd"
                | "cupsd"
                | "cups-browsed"
                | "avahi-daemon"
                | "udisksd"
                | "upowerd"
                | "rtkit-daemon"
                | "accounts-daemon"
                | "ModemManager"
                | "cron"
                | "rsyslogd"
                | "thermald"
                | "power-profiles-"
                | "earlyoom"
                | "snapd"
                | "snapfuse"
                | "atop"
                | "atopacctd"
                | "at-spi-bus-laun"
                | "at-spi2-registr"
                | "dconf-service"
                | "obexd"
                | "mpris-proxy"
                | "kunifiedpush-di"
                | "DiscoverNotifie"
                | "ssh-agent"
                | "boltd"
                | "chronyd"
                | "chronyd-starter"
                | "fwupd"
                | "networkd-dispat"
                | "smartd"
                | "switcheroo-cont"
                | "unattended-upgr"
        )
    {
        return Some("Known Linux/KDE infrastructure process");
    }
    if p.cgroup.split('/').any(|v| {
        v.starts_with("plasma-") && v.ends_with(".service")
            || matches!(
                v,
                "sddm.service"
                    | "dbus.service"
                    | "polkit.service"
                    | "NetworkManager.service"
                    | "pipewire.service"
                    | "pipewire-pulse.service"
                    | "wireplumber.service"
                    | "xdg-desktop-portal.service"
                    | "xdg-desktop-portal-kde.service"
                    | "xdg-desktop-portal-gtk.service"
                    | "xdg-document-portal.service"
                    | "xdg-permission-store.service"
                    | "geoclue.service"
                    | "upower.service"
                    | "udisks2.service"
                    | "bluetooth.service"
                    | "accounts-daemon.service"
                    | "power-profiles-daemon.service"
                    | "at-spi-dbus-bus.service"
                    | "dconf.service"
                    | "obex.service"
                    | "mpris-proxy.service"
                    | "kunifiedpush-distributor.service"
                    | "ssh-agent.service"
                    | "chrony.service"
                    | "networkd-dispatcher.service"
                    | "switcheroo-control.service"
                    | "fwupd.service"
                    | "snap.cups.cupsd.service"
                    | "snap.cups.cups-browsed.service"
                    | "cups.service"
                    | "cups-browsed.service"
            )
    }) {
        return Some("Known Linux/KDE infrastructure unit");
    }
    if p.exe.starts_with("/usr/libexec/xdg-")
        || p.exe.starts_with("/usr/lib/xdg-")
        || p.exe.contains("/polkit-kde-authentication-agent")
        || p.exe.contains("/geoclue-2.0/")
    {
        return Some("Desktop portal/authentication/location infrastructure");
    }
    None
}

fn friendly_scope(scope: &str, names: &std::collections::HashMap<String, String>) -> String {
    let mut decoded = Vec::new();
    let b = scope.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if i + 3 < b.len()
            && b[i] == b'\\'
            && b[i + 1] == b'x'
            && let (Some(hi), Some(lo)) = (
                (b[i + 2] as char).to_digit(16),
                (b[i + 3] as char).to_digit(16),
            )
        {
            decoded.push((hi * 16 + lo) as u8);
            i += 4;
            continue;
        }
        decoded.push(b[i]);
        i += 1;
    }
    let decoded = String::from_utf8_lossy(&decoded);
    let raw = decoded.strip_prefix("app-").unwrap_or(&decoded);
    let raw = raw
        .strip_suffix(".scope")
        .or_else(|| raw.strip_suffix(".service"))
        .unwrap_or(raw);
    let raw = raw.split('@').next().unwrap_or(raw);
    let raw = raw
        .rsplit_once('-')
        .filter(|(_, suffix)| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
        .map(|(name, _)| name)
        .unwrap_or(raw);
    if let Some(name) = names.get(raw) {
        return name.clone();
    }
    let raw = raw
        .strip_prefix("org.kde.")
        .or_else(|| raw.strip_prefix("org.gnome."))
        .unwrap_or(raw);
    match raw {
        "docker-desktop" => "Docker".into(),
        "slack" => "Slack".into(),
        _ => title(raw),
    }
}
fn title(raw: &str) -> String {
    raw.replace(['-', '_', '.'], " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            format!(
                "{}{}",
                chars
                    .next()
                    .map(|c| c.to_uppercase().to_string())
                    .unwrap_or_default(),
                chars.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_names_are_friendly_and_decode_escapes() {
        let names = std::collections::HashMap::from([(
            "chrome-example-Default".into(),
            "gather.town".into(),
        )]);
        assert_eq!(
            friendly_scope(
                "app-chrome\\x2dexample\\x2dDefault@instance.service",
                &names
            ),
            "gather.town"
        );
        assert!(!friendly_scope("app-test\\xé.scope", &Default::default()).is_empty());
        assert_eq!(
            friendly_scope("app-org.kde.konsole-4756.scope", &Default::default()),
            "Konsole"
        );
        assert_eq!(
            friendly_scope("app-my\\x2deditor@abc.service", &Default::default()),
            "My Editor"
        );
        assert_eq!(
            friendly_scope("app-org.kde.dolphin@f57d.service", &Default::default()),
            "Dolphin"
        );
    }
}
