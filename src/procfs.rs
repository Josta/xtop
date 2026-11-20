use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    fs,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Identity {
    pub pid: u32,
    pub start: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Memory {
    pub pss: u64,
    pub swap_pss: u64,
}

#[derive(Clone, Debug)]
pub struct Process {
    pub id: Identity,
    pub ppid: u32,
    pub name: String,
    pub command: String,
    pub exe: String,
    pub cwd: String,
    pub cgroup: String,
    pub tty: String,
    pub tty_nr: i64,
    pub age: Option<Duration>,
    pub session_title: String,
    pub interactive_shell: bool,
    pub uid: u32,
    pub state: char,
    pub ticks: u64,
    pub rss: u64,
    pub cpu: Option<f64>,
    pub memory: Option<Memory>,
}

#[derive(Clone, Debug, Default)]
pub struct System {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub cache: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub cpu: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Cgroup {
    pub memory: Option<u64>,
    pub swap: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub processes: HashMap<u32, Process>,
    pub system: System,
    pub cgroups: HashMap<String, Cgroup>,
    pub at: Instant,
    pub memory_age: Duration,
}

pub struct Collector {
    config: Config,
    previous: HashMap<Identity, (u64, Instant)>,
    memory: HashMap<Identity, Memory>,
    last_memory: Option<Instant>,
    last_cpu: Option<(u64, u64)>,
    titles: HashMap<String, String>,
    last_titles: Option<Instant>,
    hz: f64,
    page_size: u64,
}

impl Collector {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            previous: HashMap::new(),
            memory: HashMap::new(),
            last_memory: None,
            last_cpu: None,
            titles: HashMap::new(),
            last_titles: None,
            hz: unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64,
            page_size: unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64,
        }
    }

    pub fn sample(&mut self) -> Result<Snapshot> {
        let at = Instant::now();
        let uptime = fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok());
        if self
            .last_titles
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            self.titles = tmux_titles();
            self.last_titles = Some(at);
        }
        let refresh_memory = self
            .last_memory
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(self.config.memory_refresh_ms));
        let mut processes = HashMap::new();
        let mut next_previous = HashMap::new();
        let mut next_memory = HashMap::new();
        for entry in fs::read_dir("/proc").context("Reading /proc")?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(mut p) = read_process(pid, self.page_size) else {
                continue;
            };
            p.age = uptime
                .map(|up| Duration::from_secs_f64((up - p.id.start as f64 / self.hz).max(0.0)));
            p.session_title = self.titles.get(&p.tty).cloned().unwrap_or_default();
            if let Some((ticks, time)) = self.previous.get(&p.id) {
                let elapsed = at.duration_since(*time).as_secs_f64();
                if elapsed > 0.0 {
                    p.cpu = Some(p.ticks.saturating_sub(*ticks) as f64 / self.hz / elapsed * 100.0);
                }
            }
            p.memory = if refresh_memory {
                fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
                    .ok()
                    .and_then(|s| parse_memory(&s))
            } else {
                self.memory.get(&p.id).cloned()
            };
            // Reject a PID that exited/reused while its other files were read.
            if identity(pid).ok() != Some(p.id) {
                continue;
            }
            next_previous.insert(p.id, (p.ticks, at));
            if let Some(m) = &p.memory {
                next_memory.insert(p.id, m.clone());
            }
            processes.insert(pid, p);
        }
        self.previous = next_previous;
        self.memory = next_memory;
        if refresh_memory {
            self.last_memory = Some(Instant::now());
        }
        let mut system = parse_system(&fs::read_to_string("/proc/meminfo")?);
        let cpu_stat = fs::read_to_string("/proc/stat")?;
        let counters: Vec<u64> = cpu_stat
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .skip(1)
            .take(8)
            .filter_map(|v| v.parse().ok())
            .collect();
        let total = counters.iter().sum::<u64>();
        let idle = counters.get(3).copied().unwrap_or(0) + counters.get(4).copied().unwrap_or(0);
        if let Some((prev_total, prev_idle)) = self.last_cpu {
            let delta = total.saturating_sub(prev_total);
            if delta > 0 {
                system.cpu = 100.0 * delta.saturating_sub(idle.saturating_sub(prev_idle)) as f64
                    / delta as f64;
            }
        }
        self.last_cpu = Some((total, idle));
        let mut cgroups = HashMap::new();
        for p in processes.values() {
            let mut path = p.cgroup.as_str();
            while !path.is_empty() {
                cgroups
                    .entry(path.to_string())
                    .or_insert_with(|| read_cgroup(path));
                path = path
                    .rsplit_once('/')
                    .map(|(parent, _)| parent)
                    .unwrap_or("");
            }
        }
        Ok(Snapshot {
            processes,
            system,
            cgroups,
            at,
            memory_age: self.last_memory.map(|t| t.elapsed()).unwrap_or_default(),
        })
    }
}

fn fields(text: &str) -> HashMap<&str, u64> {
    text.lines()
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            Some((
                k,
                v.split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()?
                    .saturating_mul(1024),
            ))
        })
        .collect()
}

fn parse_memory(text: &str) -> Option<Memory> {
    let f = fields(text);
    Some(Memory {
        pss: *f.get("Pss")?,
        swap_pss: *f.get("SwapPss")?,
    })
}

fn parse_system(text: &str) -> System {
    let f = fields(text);
    let get = |k| f.get(k).copied().unwrap_or(0);
    System {
        total: get("MemTotal"),
        free: get("MemFree"),
        available: get("MemAvailable"),
        cache: (get("Cached") + get("Buffers") + get("SReclaimable")).saturating_sub(get("Shmem")),
        swap_total: get("SwapTotal"),
        swap_used: get("SwapTotal").saturating_sub(get("SwapFree")),
        cpu: 0.0,
    }
}

fn read_cgroup(path: &str) -> Cgroup {
    // Do not resolve namespace-relative '..' paths outside the cgroup mount.
    if path.split('/').any(|s| s == "..") {
        return Cgroup::default();
    }
    let read = |name| {
        fs::read_to_string(format!("/sys/fs/cgroup{path}/{name}"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    };
    Cgroup {
        memory: read("memory.current"),
        swap: read("memory.swap.current"),
    }
}

pub fn identity(pid: u32) -> Result<Identity> {
    Ok(parse_stat(pid, &fs::read_to_string(format!("/proc/{pid}/stat"))?, 4096)?.id)
}

fn read_process(pid: u32, page_size: u64) -> Result<Process> {
    let base = format!("/proc/{pid}");
    let mut p = parse_stat(pid, &fs::read_to_string(format!("{base}/stat"))?, page_size)?;
    let status = fs::read_to_string(format!("{base}/status"))?;
    p.uid = status
        .lines()
        .find_map(|l| {
            l.strip_prefix("Uid:")
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(u32::MAX);
    let read_link = |file| {
        fs::read_link(format!("{base}/{file}"))
            .map(|p| clean(&p.to_string_lossy()))
            .unwrap_or_default()
    };
    p.cwd = read_link("cwd");
    p.exe = read_link("exe");
    // stdin can be /dev/null, a pipe, or another terminal; tty_nr is the
    // controlling terminal and stays consistent for background shell jobs.
    p.tty = terminal_name(p.tty_nr);
    p.command = fs::read(format!("{base}/cmdline"))
        .map(|b| {
            let args: Vec<_> = b
                .split(|c| *c == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            p.interactive_shell = p.tty_nr != 0
                && interactive_shell(
                    &p.exe,
                    &args,
                    read_link("fd/0") == p.tty && read_link("fd/2") == p.tty,
                );

            clean(&String::from_utf8_lossy(&b).replace('\0', " "))
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    if p.command.is_empty() {
        p.command = p.name.clone();
    }
    p.cgroup = fs::read_to_string(format!("{base}/cgroup"))
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("0::").map(clean)))
        .unwrap_or_default();
    Ok(p)
}

// Heuristic: explicit interactive mode, or a terminal-attached shell without
// a command/script argument. A PTY alone also occurs in background job runners.
fn interactive_shell(exe: &str, args: &[String], terminal_io: bool) -> bool {
    let name = exe.rsplit('/').next().unwrap_or(exe);
    if !matches!(name, "zsh" | "bash" | "dash" | "fish" | "sh") || args.is_empty() {
        return false;
    }
    for arg in args.iter().skip(1) {
        if arg == "--interactive" {
            return true;
        }
        if arg == "--command" {
            return false;
        }
        if matches!(
            arg.as_str(),
            "--login" | "--norc" | "--noprofile" | "--no-config"
        ) {
            continue;
        }
        if let Some(flags) = arg
            .strip_prefix('-')
            .filter(|s| !s.starts_with('-') && !s.is_empty())
        {
            if flags.contains('i') {
                return true;
            }
            if flags.contains('c') {
                return false;
            }
        } else {
            return false;
        }
    }
    terminal_io
}

pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn terminal_name(tty_nr: i64) -> String {
    if tty_nr == 0 {
        return String::new();
    }
    let dev = tty_nr as u32;
    let major = (dev >> 8) & 0xfff;
    let minor = (dev & 0xff) | ((dev >> 12) & 0xfff00);
    match major {
        136..=143 => format!("/dev/pts/{}", (major - 136) * 256 + minor),
        4 => format!("/dev/tty{minor}"),
        _ => format!("tty {major}:{minor}"),
    }
}

fn parse_stat(pid: u32, text: &str, page_size: u64) -> Result<Process> {
    let open = text.find('(').context("stat missing name")?;
    let close = text.rfind(')').context("stat missing name end")?;
    let f: Vec<_> = text[close + 1..].split_whitespace().collect();
    if f.len() < 22 {
        bail!("stat too short");
    }
    Ok(Process {
        id: Identity {
            pid,
            start: f[19].parse()?,
        },
        ppid: f[1].parse()?,
        name: clean(&text[open + 1..close]),
        state: f[0].chars().next().unwrap_or('?'),
        tty_nr: f[4].parse()?,
        ticks: f[11].parse::<u64>()? + f[12].parse::<u64>()?,
        rss: f[21].parse::<i64>()?.max(0) as u64 * page_size,
        uid: u32::MAX,
        command: String::new(),
        exe: String::new(),
        cwd: String::new(),
        cgroup: String::new(),
        tty: String::new(),
        age: None,
        session_title: String::new(),
        interactive_shell: false,
        cpu: None,
        memory: None,
    })
}

/// pidfd binds the signal to a process instance, preventing PID reuse races.
pub fn signal(target: Identity, signal: i32) -> Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, target.pid, 0) } as i32;
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("Opening process handle");
    }
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    if identity(target.pid)? != target {
        bail!("Process identity changed; refusing signal");
    }
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("Sending signal");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controlling_terminal_does_not_depend_on_stdin() {
        assert_eq!(terminal_name(34819), "/dev/pts/3");
        assert_eq!(terminal_name(1026), "/dev/tty2");
        assert_eq!(terminal_name(0), "");
    }

    #[test]
    fn pidfd_signal_rejects_wrong_identity_and_terminates_only_test_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let id = identity(child.id()).unwrap();
        let wrong = Identity {
            start: id.start + 1,
            ..id
        };
        let rejected = signal(wrong, libc::SIGTERM).is_err();
        let still_alive = child.try_wait().unwrap().is_none();
        let sent = signal(id, libc::SIGTERM);
        if sent.is_err() {
            let _ = child.kill();
        }
        let _ = child.wait();
        assert!(rejected);
        assert!(still_alive);
        assert!(sent.is_ok(), "{sent:?}");
    }
    #[test]
    fn parses_stat_with_spaces_and_parentheses() {
        let p = parse_stat(
            7,
            "7 (a ) tricky name) S 1 2 3 34816 5 6 7 8 9 10 120 30 13 14 15 16 17 18 999 20 42",
            4096,
        )
        .unwrap();
        assert_eq!(p.name, "a ) tricky name");
        assert_eq!(p.id.start, 999);
        assert_eq!(p.ticks, 150);
        assert_eq!(p.rss, 42 * 4096);
        assert_eq!(p.tty_nr, 34816);
    }
    #[test]
    fn missing_memory_is_unknown_not_zero() {
        assert!(parse_memory("Rss: 42 kB").is_none());
        let m = parse_memory("Pss: 12 kB\nSwapPss: 3 kB\nRss: 40 kB").unwrap();
        assert_eq!(m.pss, 12288);
        assert_eq!(m.swap_pss, 3072);
    }
    #[test]
    fn memory_composition_excludes_shmem_from_reclaimable_cache() {
        let m = parse_system(
            "MemTotal: 100 kB\nMemFree: 10 kB\nCached: 30 kB\nShmem: 12 kB\nBuffers: 2 kB\nSReclaimable: 3 kB",
        );
        assert_eq!(m.cache, 23 * 1024);
    }
}

// A bounded read-only query on the current/default tmux server. Failure is
// normal when tmux is absent, inaccessible, or no server is running.
fn tmux_titles() -> HashMap<String, String> {
    let output = std::process::Command::new("timeout")
        .args([
            "0.3s",
            "tmux",
            "list-panes",
            "-a",
            "-F",
            "#{pane_tty}\t#{session_name}:#{window_index}.#{pane_index}\t#{pane_title}",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match output {
        Ok(out) if out.status.success() => parse_tmux_titles(&String::from_utf8_lossy(&out.stdout)),
        _ => HashMap::new(),
    }
}
fn parse_tmux_titles(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let tty = fields.next()?;
            let location = fields.next()?;
            let title = fields.next()?;
            tty.starts_with("/dev/").then(|| {
                (
                    tty.to_string(),
                    clean(&format!("tmux {location} · {title}")),
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod title_tests {
    use super::*;
    #[test]
    fn interactive_shells_are_not_just_processes_with_a_pty() {
        for (exe, argv, terminal, expected) in [
            ("/usr/bin/zsh", vec!["-zsh"], true, true),
            ("/bin/bash", vec!["bash", "-lc", "run-server"], true, false),
            ("/bin/bash", vec!["bash", "script.sh"], true, false),
            ("/bin/bash", vec!["bash", "-ic", "run-command"], true, true),
            ("/bin/zsh", vec!["zsh"], false, false),
            ("/bin/fish", vec!["fish", "--interactive"], false, true),
            ("/bin/node", vec!["node"], true, false),
            ("/bin/bash", vec!["bash", "-c", "-i"], true, false),
        ] {
            let argv = argv.into_iter().map(String::from).collect::<Vec<_>>();
            assert_eq!(
                interactive_shell(exe, &argv, terminal),
                expected,
                "{argv:?}"
            );
        }
    }

    #[test]
    fn pane_titles_preserve_locations_and_sanitize_controls() {
        let titles = parse_tmux_titles(
            "/dev/pts/2\twork:1.0\tFix parser\tbug\nmalformed\n/dev/pts/3\twork:2.0\tSecond task\n",
        );
        assert_eq!(titles.len(), 2);
        assert_eq!(titles["/dev/pts/2"], "tmux work:1.0 · Fix parser bug");
        assert_eq!(titles["/dev/pts/3"], "tmux work:2.0 · Second task");
    }
}
