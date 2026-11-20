use crate::{
    config::Config,
    procfs::{Identity, Process, Snapshot},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    time::Instant,
};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Key {
    Group(String),
    Process(Identity),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Groups,
    Tree,
    Cgroups,
}
impl View {
    pub fn next(self) -> Self {
        match self {
            Self::Groups => Self::Tree,
            Self::Tree => Self::Cgroups,
            Self::Cgroups => Self::Groups,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Groups => "Workloads",
            Self::Tree => "Process tree",
            Self::Cgroups => "Cgroups",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    Name,
    Memory,
    Cpu,
}
impl Sort {
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Memory,
            Self::Memory => Self::Cpu,
            Self::Cpu => Self::Name,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Memory => "PSS",
            Self::Cpu => "CPU",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub count: usize,
    pub covered: usize,
    pub pss: u64,
    pub swap: u64,
    pub rss: u64,
    pub cpu: f64,
    pub cpu_covered: usize,
}
impl Metrics {
    pub fn add(&mut self, p: &Process) {
        self.count += 1;
        self.rss += p.rss;
        if let Some(cpu) = p.cpu {
            self.cpu += cpu;
            self.cpu_covered += 1;
        }
        if let Some(m) = &p.memory {
            self.covered += 1;
            self.pss += m.pss;
            self.swap += m.swap_pss;
        }
    }
    fn mem_text(&self, value: u64) -> String {
        if self.covered == 0 {
            "—".into()
        } else {
            format!(
                "{:.1} MB{}",
                value as f64 / 1_000_000.0,
                if self.covered < self.count { "*" } else { "" }
            )
        }
    }
    pub fn pss_text(&self) -> String {
        self.mem_text(self.pss)
    }
    pub fn swap_text(&self) -> String {
        self.mem_text(self.swap)
    }
    pub fn total_text(&self) -> String {
        self.mem_text(self.pss + self.swap)
    }
    pub fn cpu_text(&self) -> String {
        if self.cpu_covered == 0 {
            return "—".into();
        }
        let cores = format!("{:.2}", self.cpu / 100.0);
        format!(
            "{}{}",
            if cores == "0.00" { "0   " } else { &cores },
            if self.cpu_covered < self.count {
                "*"
            } else {
                ""
            }
        )
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub key: Key,
    pub label: String,
    pub depth: usize,
    pub expandable: bool,
    pub open: bool,
    pub metrics: Metrics,
    pub members: Vec<Identity>,
    /// Folded descendants, ordered from outer wrapper to final child.
    pub folded: Vec<Identity>,
}

#[derive(Clone, Debug)]
pub struct Point {
    pub at: Instant,
    pub metrics: Metrics,
}

struct Graph {
    nodes: HashMap<Key, Row>,
    parents: HashMap<Key, Option<Key>>,
}

pub struct App {
    pub config: Config,
    pub config_path: PathBuf,
    pub snapshot: Option<Snapshot>,
    pub rows: Vec<Row>,
    pub selected: Option<Key>,
    pub offset: usize,
    pub viewport: usize,
    pub view: View,
    pub sort: Sort,
    pub show_system: bool,
    pub workload_tree: bool,
    pub depth_limit: Option<usize>,
    pub expanded: HashSet<Key>,
    pub filter: String,
    pub editing_filter: bool,
    pub paused: bool,
    pub details: bool,
    pub help: bool,
    pub settings: bool,
    pub settings_cursor: usize,
    pub show_pid: bool,
    pub show_age: bool,
    pub show_cpu: bool,
    pub show_pss: bool,
    pub show_swap: bool,
    pub show_total: bool,
    pub measurement: crate::history::Measurement,
    pub status: String,
    pub history: HashMap<Key, VecDeque<Point>>,
    pub confirmation: Option<(Vec<Identity>, i32, String)>,
    pub confirmation_offset: usize,
}

impl App {
    pub fn new(config: Config, config_path: PathBuf) -> Self {
        Self {
            show_system: config.show_system,
            workload_tree: config.workload_tree,
            depth_limit: None,
            config,
            config_path,
            snapshot: None,
            rows: vec![],
            selected: None,
            offset: 0,
            viewport: 10,
            view: View::Groups,
            sort: Sort::Name,
            expanded: HashSet::new(),
            filter: String::new(),
            editing_filter: false,
            paused: false,
            details: true,
            help: false,
            settings: false,
            settings_cursor: 0,
            show_pid: true,
            show_age: true,
            show_cpu: true,
            show_pss: true,
            show_swap: true,
            show_total: false,
            measurement: Default::default(),
            status: "Collecting processes…".into(),
            history: HashMap::new(),
            confirmation: None,
            confirmation_offset: 0,
        }
    }

    pub fn update(&mut self, snapshot: Snapshot) {
        if self.paused {
            return;
        }
        self.snapshot = Some(snapshot);
        let at = self.snapshot.as_ref().unwrap().at;
        // Record every process and aggregate, including collapsed/offscreen groups.
        let nodes = self.nodes();
        for row in nodes.values() {
            let h = self.history.entry(row.key.clone()).or_default();
            h.push_back(Point {
                at,
                metrics: row.metrics.clone(),
            });
        }
        let seconds = self.config.history_seconds;
        self.history.retain(|_, h| {
            // Retain one boundary sample to integrate a full window correctly.
            while h
                .get(1)
                .is_some_and(|p| at.duration_since(p.at).as_secs() >= seconds)
            {
                h.pop_front();
            }
            h.back()
                .is_some_and(|p| at.duration_since(p.at).as_secs() <= seconds)
        });
        self.rebuild();
        if self.status == "Collecting processes…" {
            self.status.clear();
        }
    }

    pub fn cycle_measurement(&mut self) {
        self.measurement = self.measurement.next();
        self.rebuild();
    }

    pub fn measured(&self, row: &Row, metric: crate::history::Metric) -> (Option<f64>, bool) {
        if self.measurement == crate::history::Measurement::Live {
            return metric
                .sample(&row.metrics)
                .map_or((None, false), |(value, partial)| (Some(value), partial));
        }
        let Some((history, snapshot)) = self.history.get(&row.key).zip(self.snapshot.as_ref())
        else {
            return (None, true);
        };
        let stats = crate::history::summarize(
            history,
            snapshot.at,
            self.measurement.window(),
            std::time::Duration::from_millis(self.config.refresh_ms.saturating_mul(3)),
            metric,
        );
        (
            if self.measurement.is_peak() {
                stats.peak
            } else {
                stats.average
            },
            stats.partial,
        )
    }

    fn nodes(&self) -> HashMap<Key, Row> {
        self.graph().nodes
    }

    fn graph(&self) -> Graph {
        let mut graph = Graph {
            nodes: HashMap::new(),
            parents: HashMap::new(),
        };
        let Some(s) = &self.snapshot else {
            return graph;
        };
        let assignments: HashMap<_, _> = s
            .processes
            .values()
            .map(|p| (p.id.pid, crate::grouping::classify(p, s, &self.config)))
            .collect();
        let eligible: HashSet<_> = s
            .processes
            .values()
            .filter(|p| self.show_system || !assignments[&p.id.pid].system)
            .map(|p| p.id.pid)
            .collect();
        // Count actual snapshot children, including hidden ones: filtering must
        // not make a multi-child shell look like a single-child wrapper.
        let mut child_counts: HashMap<u32, usize> = HashMap::new();
        for p in s.processes.values() {
            *child_counts.entry(p.ppid).or_default() += 1;
        }
        let mut candidates: Vec<_> = s
            .processes
            .values()
            .filter_map(|p| {
                if self.view != View::Groups || !eligible.contains(&p.id.pid) {
                    return None;
                }
                let parent = s.processes.get(&p.ppid)?;
                (eligible.contains(&parent.id.pid)
                    && parent.id != p.id
                    && matches!(
                        process_name(parent),
                        "zsh" | "dash" | "bash" | "fish" | "node" | "nodejs"
                    )
                    && child_counts.get(&parent.id.pid) == Some(&1)
                    && assignments[&p.id.pid].bucket() == assignments[&parent.id.pid].bucket())
                .then_some((p.id, parent.id))
            })
            .collect();
        // Resolve every foldable edge to the outermost wrapper so chains
        // occupy one row. Keep depth for the ordered display/signal identity.
        let depth = |id: Identity| {
            let mut depth = 0;
            let mut pid = id.pid;
            let mut seen = HashSet::new();
            while seen.insert(pid) {
                let Some(p) = s.processes.get(&pid) else {
                    break;
                };
                depth += 1;
                pid = p.ppid;
            }
            depth
        };
        let candidates: HashMap<_, _> = candidates.drain(..).collect();
        let mut folded = HashMap::new();
        for (&child, &parent) in &candidates {
            let mut root = parent;
            let mut seen = HashSet::from([child]);
            while seen.insert(root) {
                let Some(&next) = candidates.get(&root) else {
                    folded.insert(child, root);
                    break;
                };
                root = next;
            }
        }
        let displayed: HashSet<_> = eligible
            .iter()
            .copied()
            .filter(|pid| !folded.contains_key(&s.processes[pid].id))
            .collect();
        let new_row = |key: Key, label: String| Row {
            key,
            label,
            depth: 0,
            expandable: false,
            open: false,
            metrics: Metrics::default(),
            members: vec![],
            folded: vec![],
        };
        for p in s
            .processes
            .values()
            .filter(|p| displayed.contains(&p.id.pid))
        {
            let key = Key::Process(p.id);
            graph
                .nodes
                .insert(key.clone(), new_row(key.clone(), label(p)));
            let assignment = &assignments[&p.id.pid];
            let mut parent = None;
            if self.view == View::Groups {
                let root = Key::Group(assignment.group.clone());
                graph
                    .nodes
                    .entry(root.clone())
                    .or_insert_with(|| new_row(root.clone(), assignment.group.clone()));
                graph.parents.insert(root.clone(), None);
                parent = Some(root.clone());
                if let Some(session) = &assignment.session {
                    let section = Key::Group(assignment.bucket());
                    graph
                        .nodes
                        .entry(section.clone())
                        .or_insert_with(|| new_row(section.clone(), session.clone()));
                    graph.parents.insert(section.clone(), Some(root));
                    parent = Some(section);
                } else if assignment.service {
                    let section = Key::Group(assignment.bucket());
                    graph
                        .nodes
                        .entry(section.clone())
                        .or_insert_with(|| new_row(section.clone(), "Services".into()));
                    graph.parents.insert(section.clone(), Some(root));
                    parent = Some(section);
                }
            } else if self.view == View::Cgroups {
                for path in cgroup_paths(&p.cgroup) {
                    let group = Key::Group(path.clone());
                    graph
                        .nodes
                        .entry(group.clone())
                        .or_insert_with(|| new_row(group.clone(), path));
                    graph.parents.insert(group.clone(), parent);
                    parent = Some(group);
                }
            }
            if self.view == View::Tree || self.view == View::Groups && self.workload_tree {
                let mut pid = p.ppid;
                let mut seen = HashSet::from([p.id.pid]);
                while let Some(ancestor) = s.processes.get(&pid) {
                    if !seen.insert(pid) {
                        break;
                    }
                    if displayed.contains(&pid) {
                        if self.view == View::Tree
                            || assignments[&pid].bucket() == assignment.bucket()
                        {
                            parent = Some(Key::Process(ancestor.id));
                        }
                        break;
                    }
                    pid = ancestor.ppid;
                }
            }
            graph.parents.insert(key, parent);
        }
        // Infrastructure roots add no useful level inside their own sections.
        // Keep their parent links so their own usage still reaches the section.
        if self.view == View::Groups {
            let mut roots: Vec<_> = s
                .processes
                .values()
                .filter(|p| {
                    matches!(
                        (p.name.as_str(), assignments[&p.id.pid].bucket().as_str()),
                        ("systemd", "System services") | ("kthreadd", "Kernel threads")
                    )
                })
                .collect();
            roots.sort_by_key(|p| depth(p.id));
            for p in roots {
                let key = Key::Process(p.id);
                let Some(Some(parent @ Key::Group(_))) = graph.parents.get(&key).cloned() else {
                    continue;
                };
                graph.nodes.remove(&key);
                for (child, link) in &mut graph.parents {
                    if child != &key && link.as_ref() == Some(&key) {
                        *link = Some(parent.clone());
                    }
                }
            }
        }
        // Each eligible process contributes once to every *visual* ancestor.
        // Crossing a workload/service boundary terminates the process tree.
        for p in s
            .processes
            .values()
            .filter(|p| eligible.contains(&p.id.pid))
        {
            let mut current = Some(Key::Process(folded.get(&p.id).copied().unwrap_or(p.id)));
            let mut seen = HashSet::new();
            while let Some(key) = current {
                if !seen.insert(key.clone()) {
                    break;
                }
                if let Some(row) = graph.nodes.get_mut(&key) {
                    row.metrics.add(p);
                    row.members.push(p.id);
                }
                current = graph.parents.get(&key).cloned().flatten();
            }
        }
        for (child, wrapper) in folded {
            if let Some(row) = graph.nodes.get_mut(&Key::Process(wrapper)) {
                row.folded.push(child);
            }
        }
        for row in graph.nodes.values_mut() {
            if let Key::Group(group) = &row.key
                && let Some(heading) = shell_heading(group, s)
            {
                row.label = heading.label;
            }
            row.folded.sort_by_key(|id| (depth(*id), *id));
            if let Key::Process(wrapper) = row.key
                && let Some(child) = row.folded.last()
            {
                let chain = std::iter::once(&wrapper)
                    .chain(row.folded.iter())
                    .map(|id| process_name(&s.processes[&id.pid]))
                    .collect::<Vec<_>>()
                    .join(": ");
                row.label = label_named(&s.processes[&child.pid], &chain);
            }
        }
        graph.parents.retain(|key, _| graph.nodes.contains_key(key));
        for parent in graph.parents.values().flatten() {
            if let Some(row) = graph.nodes.get_mut(parent) {
                row.expandable = true;
            }
        }
        graph
    }

    pub fn set_depth(&mut self, level: usize) {
        self.depth_limit = Some(level.max(1));
        self.rebuild();
    }
    pub fn toggle_system(&mut self) {
        self.show_system = !self.show_system;
        self.history.clear();
        self.rebuild();
    }
    pub fn toggle_workload_tree(&mut self) {
        self.workload_tree = !self.workload_tree;
        self.history.clear();
        self.rebuild();
    }

    pub fn rebuild(&mut self) {
        let old_index = self.index().unwrap_or(0);
        let old_screen_row = old_index.saturating_sub(self.offset);
        let graph = self.graph();
        let nodes = &graph.nodes;
        let mut children: HashMap<Option<Key>, Vec<Key>> = HashMap::new();
        for (key, parent) in &graph.parents {
            children
                .entry(parent.clone())
                .or_default()
                .push(key.clone());
        }
        if let Some(level) = self.depth_limit {
            self.expanded.clear();
            let mut queue: Vec<_> = children
                .get(&None)
                .into_iter()
                .flatten()
                .map(|k| (k.clone(), 1))
                .collect();
            let mut seen = HashSet::new();
            while let Some((key, depth)) = queue.pop() {
                if !seen.insert(key.clone()) {
                    continue;
                }
                if depth < level {
                    self.expanded.insert(key.clone());
                    if let Some(kids) = children.get(&Some(key)) {
                        queue.extend(kids.iter().map(|k| (k.clone(), depth + 1)));
                    }
                }
            }
        }
        let sort_values: HashMap<_, _> = nodes
            .values()
            .filter_map(|row| {
                let metric = match self.sort {
                    Sort::Memory => crate::history::Metric::Ram,
                    Sort::Cpu => crate::history::Metric::Cpu,
                    Sort::Name => return None,
                };
                Some((row.key.clone(), self.measured(row, metric).0))
            })
            .collect();
        for siblings in children.values_mut() {
            siblings.sort_by(|a, b| {
                let (a, b) = (&nodes[a], &nodes[b]);
                let primary = match self.sort {
                    Sort::Name if self.view == View::Groups => {
                        let rank = |row: &Row| match &row.key {
                            Key::Group(name) if name == "Other services" => 1,
                            Key::Group(name) if name == "Kernel threads" => 2,
                            _ => 0,
                        };
                        let shell = |row: &Row| match &row.key {
                            Key::Group(group) => {
                                self.snapshot.as_ref().and_then(|s| shell_heading(group, s))
                            }
                            _ => None,
                        };
                        let (sa, sb) = (shell(a), shell(b));
                        let an = if sa.is_some() {
                            "shell".into()
                        } else {
                            a.label.to_lowercase()
                        };
                        let bn = if sb.is_some() {
                            "shell".into()
                        } else {
                            b.label.to_lowercase()
                        };
                        rank(a)
                            .cmp(&rank(b))
                            .then_with(|| an.cmp(&bn))
                            .then_with(|| match (sa, sb) {
                                (Some(a), Some(b)) => a.order.cmp(&b.order),
                                _ => std::cmp::Ordering::Equal,
                            })
                    }
                    Sort::Name => std::cmp::Ordering::Equal,
                    Sort::Memory | Sort::Cpu => sort_values[&b.key]
                        .partial_cmp(&sort_values[&a.key])
                        .unwrap_or(std::cmp::Ordering::Equal),
                };
                primary
                    .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
                    .then_with(|| format!("{:?}", a.key).cmp(&format!("{:?}", b.key)))
            });
        }
        let mut visible = HashSet::new();
        if !self.filter.is_empty() {
            let query = self.filter.to_lowercase();
            for row in nodes.values() {
                let matches = row.label.to_lowercase().contains(&query)
                    || match &row.key {
                        Key::Process(id) => self.snapshot.as_ref().is_some_and(|s| {
                            std::iter::once(id).chain(row.folded.iter()).any(|id| {
                                let p = &s.processes[&id.pid];
                                format!(
                                    "{} {} {} {} {} {}",
                                    p.command, p.cwd, p.cgroup, p.tty, id.pid, p.session_title
                                )
                                .to_lowercase()
                                .contains(&query)
                            })
                        }),
                        _ => false,
                    };
                if matches {
                    visible.insert(row.key.clone());
                }
            }
            // Include ancestors of matches. A matching group includes its members.
            for row in nodes.values().filter(|r| matches!(r.key, Key::Group(_))) {
                if row.label.to_lowercase().contains(&query) {
                    for id in &row.members {
                        visible.insert(Key::Process(*id));
                    }
                }
            }
            loop {
                let before = visible.len();
                for (parent, kids) in &children {
                    if kids.iter().any(|k| visible.contains(k))
                        && let Some(parent) = parent
                    {
                        visible.insert(parent.clone());
                    }
                }
                if visible.len() == before {
                    break;
                }
            }
        }
        self.rows.clear();
        let mut stack: Vec<_> = children
            .get(&None)
            .into_iter()
            .flatten()
            .rev()
            .map(|k| (k.clone(), 0))
            .collect();
        let mut seen = HashSet::new();
        while let Some((key, depth)) = stack.pop() {
            if !seen.insert(key.clone()) || !self.filter.is_empty() && !visible.contains(&key) {
                continue;
            }
            let mut row = nodes[&key].clone();
            row.depth = depth;
            row.expandable = children
                .get(&Some(key.clone()))
                .is_some_and(|v| !v.is_empty());
            row.open = row.expandable
                && (self.expanded.contains(&key)
                    || !self.filter.is_empty() && self.depth_limit.is_none());
            if row.open
                && let Some(kids) = children.get(&Some(key))
            {
                stack.extend(kids.iter().rev().map(|k| (k.clone(), depth + 1)));
            }
            self.rows.push(row);
        }
        if self.index().is_none()
            && let Some(Key::Process(id)) = self.selected
            && let Some(alias) = self.rows.iter().find(|r| r.folded.contains(&id))
        {
            self.selected = Some(alias.key.clone());
        }
        if self.index().is_none() {
            let mut ancestor = self.selected.clone();
            let mut seen = HashSet::new();
            while let Some(key) = ancestor {
                if !seen.insert(key.clone()) {
                    break;
                }
                if self.rows.iter().any(|r| r.key == key) {
                    self.selected = Some(key);
                    break;
                }
                ancestor = graph.parents.get(&key).cloned().flatten();
            }
        }
        if let Some(i) = self.index() {
            // Keep the selected identity at the same screen position across re-sorts.
            self.offset = i.saturating_sub(old_screen_row);
        } else {
            self.selected = self
                .rows
                .get(old_index.min(self.rows.len().saturating_sub(1)))
                .map(|r| r.key.clone());
        }
        self.ensure_visible();
    }

    pub fn index(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| Some(&r.key) == self.selected.as_ref())
    }
    pub fn current(&self) -> Option<&Row> {
        self.index().map(|i| &self.rows[i])
    }
    pub fn ensure_visible(&mut self) {
        let n = self.viewport.max(1);
        self.offset = self.offset.min(self.rows.len().saturating_sub(n));
        if let Some(i) = self.index() {
            if i < self.offset {
                self.offset = i;
            } else if i >= self.offset + n {
                self.offset = i + 1 - n;
            }
        }
    }
    pub fn navigate(&mut self, delta: isize) {
        let i = self
            .index()
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
        self.selected = self.rows.get(i).map(|r| r.key.clone());
        self.ensure_visible();
    }
    pub fn toggle(&mut self) {
        self.depth_limit = None;
        if let Some(r) = self.current().filter(|r| r.expandable) {
            let key = r.key.clone();
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
            self.rebuild();
        }
    }
    pub fn collapse(&mut self) {
        self.depth_limit = None;
        if let Some(i) = self.index() {
            if self.rows[i].open {
                self.expanded.remove(&self.rows[i].key);
                self.rebuild();
            } else if let Some(parent) = (0..i)
                .rev()
                .find(|j| self.rows[*j].depth < self.rows[i].depth)
            {
                self.selected = Some(self.rows[parent].key.clone());
                self.ensure_visible();
            }
        }
    }
    pub fn change_view(&mut self) {
        self.view = self.view.next();
        self.history.clear();
        self.rebuild();
    }
    pub fn prepare_signal(&mut self, signal: i32, all: bool) {
        if self.paused {
            self.status = "Resume before signaling a process".into();
            return;
        }
        if let Some(row) = self.current() {
            let members = if all {
                row.members.clone()
            } else if let Some(child) = row.folded.last() {
                vec![*child]
            } else if let Key::Process(id) = row.key {
                vec![id]
            } else {
                self.status = "Select a process, or Shift+K for the group".into();
                return;
            };
            let targets: Vec<_> = members
                .into_iter()
                .filter(|id| id.pid > 1 && id.pid != std::process::id())
                .collect();
            if targets.is_empty() {
                self.status = "No eligible processes".into();
                return;
            }
            self.confirmation = Some((targets, signal, row.label.clone()));
            self.confirmation_offset = 0;
        }
    }
}

fn cgroup_paths(path: &str) -> Vec<String> {
    if path.is_empty() || path == "/" {
        return vec!["(root)".into()];
    }
    let mut paths = vec![];
    let mut prefix = String::new();
    for component in path.split('/').filter(|s| !s.is_empty()) {
        prefix.push('/');
        prefix.push_str(component);
        paths.push(prefix.clone());
    }
    paths
}

pub fn classify(p: &Process, s: &Snapshot, config: &Config) -> (String, String) {
    let a = crate::grouping::classify(p, s, config);
    (
        if a.service {
            format!("{} / Services", a.group)
        } else {
            a.group
        },
        format!(
            "{} · {}",
            if a.system { "system" } else { "user workload" },
            a.reason
        ),
    )
}

pub fn process_name(p: &Process) -> &str {
    let exe = p.exe.strip_suffix(" (deleted)").unwrap_or(&p.exe);
    let executable = std::path::Path::new(exe)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&p.name);
    // Native self-updating binaries (e.g. Claude) often use the version as
    // their on-disk filename. comm preserves the actual program identity.
    let core = executable
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or(executable);
    let version = core.contains('.')
        && core
            .split('.')
            .all(|v| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()));
    if version && exe.contains("/claude/versions/") {
        "claude"
    } else if version && !p.name.is_empty() {
        &p.name
    } else if p.name == "tmux: server" {
        "tmux server"
    } else if p.name == "tmux: client" {
        "tmux client"
    } else {
        executable
    }
}

/// Display-only abbreviation, also usable for paths embedded in commands.
pub fn display_home(text: &str) -> String {
    std::env::var_os("HOME").map_or_else(
        || text.to_string(),
        |home| shorten_home(text, &home.to_string_lossy()),
    )
}
fn shorten_home(text: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return text.to_string();
    }
    if text.trim_end_matches('/') == home {
        return "~".into();
    }
    let mut result = String::new();
    let mut cursor = 0;
    for (i, _) in text.match_indices(home) {
        let end = i + home.len();
        let left = text[..i].chars().next_back();
        let right = text[end..].chars().next();
        let starts_path = left.is_none_or(|c| {
            c.is_whitespace() || matches!(c, '\'' | '"' | '=' | ':' | '(' | '[' | '{' | ';' | ',')
        });
        let ends_component = right.is_none_or(|c| {
            c == '/'
                || c.is_whitespace()
                || matches!(c, '\'' | '"' | ')' | ']' | '}' | ';' | ',' | ':')
        });
        if starts_path && ends_component {
            result.push_str(&text[cursor..i]);
            result.push('~');
            cursor = end;
        }
    }
    result.push_str(&text[cursor..]);
    result
}
fn format_label(name: &str, cwd: &str, home: Option<&std::path::Path>) -> String {
    if cwd.is_empty() {
        name.to_string()
    } else {
        format!(
            "{name}  {}",
            home.map_or_else(
                || cwd.to_string(),
                |h| shorten_home(cwd, &h.to_string_lossy())
            )
        )
    }
}

fn label_named(p: &Process, name: &str) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    format_label(name, &p.cwd, home.as_deref())
}
pub fn label(p: &Process) -> String {
    label_named(p, process_name(p))
}

struct ShellHeading {
    label: String,
    order: (u8, u32, u32, String),
}

fn shell_heading(group: &str, snapshot: &Snapshot) -> Option<ShellHeading> {
    if matches!(group, "Shell · TMUX" | "Shell · Screen") {
        return Some(ShellHeading {
            label: group.into(),
            order: (
                if group == "Shell · TMUX" { 0 } else { 2 },
                0,
                0,
                group.into(),
            ),
        });
    }
    let tty = group.rsplit('\x1f').next()?.strip_prefix("Shell · ")?;
    let session_title = snapshot
        .processes
        .values()
        .filter(|p| p.tty == tty && !p.session_title.is_empty())
        .min_by_key(|p| p.id)
        .map(|p| p.session_title.as_str());
    if let Some(title) = session_title
        && let Some((location, title)) = title
            .strip_prefix("tmux ")
            .and_then(|s| s.split_once(" · "))
        && let Some((session, indices)) = location.rsplit_once(':')
        && let Some((window, pane)) = indices.split_once('.')
        && let (Ok(window), Ok(pane)) = (window.parse::<u32>(), pane.parse::<u32>())
    {
        return Some(ShellHeading {
            label: format!("TMUX / Window {window} / Pane {pane} - {title}"),
            order: (0, window, pane, session.into()),
        });
    }
    let short = tty.strip_prefix("/dev/pts/").unwrap_or(tty);
    let interactive = snapshot
        .processes
        .values()
        .any(|p| p.tty == tty && p.interactive_shell);
    let kind = if interactive {
        "interactive"
    } else {
        "background"
    };
    Some(ShellHeading {
        label: format!("Shell #{short} ({kind})"),
        order: (
            if interactive { 1 } else { 2 },
            short.parse().unwrap_or(u32::MAX),
            0,
            tty.into(),
        ),
    })
}

pub fn bytes(n: u64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.1}G", n as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if n >= 1024 * 1024 {
        format!("{:.1}M", n as f64 / (1024.0 * 1024.0))
    } else {
        format!("{}K", n / 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::{Memory, System};
    fn process(pid: u32, ppid: u32, start: u64) -> Process {
        Process {
            id: Identity { pid, start },
            ppid,
            name: format!("p{pid}"),
            command: format!("p{pid}"),
            exe: format!("/bin/p{pid}"),
            cwd: String::new(),
            cgroup: "/user.slice/app.scope".into(),
            tty: String::new(),
            tty_nr: 0,
            age: None,
            session_title: String::new(),
            interactive_shell: false,
            uid: 1000,
            state: 'S',
            ticks: 0,
            rss: 100,
            cpu: Some(1.0),
            memory: Some(Memory {
                pss: 40,
                swap_pss: 10,
            }),
        }
    }
    fn snapshot(ps: Vec<Process>) -> Snapshot {
        Snapshot {
            processes: ps.into_iter().map(|p| (p.id.pid, p)).collect(),
            system: System::default(),
            cgroups: HashMap::new(),
            at: Instant::now(),
            memory_age: Default::default(),
        }
    }
    fn app() -> App {
        App::new(
            Config {
                show_system: true,
                ..Config::default()
            },
            PathBuf::new(),
        )
    }
    #[test]
    fn shell_headings_sort_numeric_terminals_then_tmux_windows_and_panes() {
        let mut a = app();
        a.sort = Sort::Name;
        let fixtures = [
            (11, ""),
            (2, ""),
            (12, ""),
            (1, ""),
            (3, "tmux work:11.0 · Later window"),
            (4, "tmux work:2.11 · Later pane"),
            (5, "tmux work:2.2 · Earlier pane"),
        ];
        let ps = fixtures
            .into_iter()
            .map(|(pts, title)| {
                let mut p = process(100 + pts, 0, 100 + pts as u64);
                p.tty = format!("/dev/pts/{pts}");
                p.tty_nr = 34816 + pts as i64;
                p.session_title = title.into();
                p.interactive_shell = pts == 2 || pts == 11;
                p
            })
            .collect();
        a.update(snapshot(ps));
        a.set_depth(2);
        assert_eq!(
            a.rows
                .iter()
                .filter(|r| matches!(&r.key, Key::Group(_)))
                .map(|r| r.label.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Shell · TMUX",
                "TMUX / Window 2 / Pane 2 - Earlier pane",
                "TMUX / Window 2 / Pane 11 - Later pane",
                "TMUX / Window 11 / Pane 0 - Later window",
                "Shell #2 (interactive)",
                "Shell #11 (interactive)",
                "Shell #1 (background)",
                "Shell #12 (background)",
            ]
        );
        assert!(
            a.rows
                .iter()
                .any(|r| r.key == Key::Group("Shell · /dev/pts/2".into()))
        );
        // A changing title updates the label without changing identity/order.
        let mut next = a.snapshot.clone().unwrap();
        next.processes.get_mut(&105).unwrap().session_title = "tmux work:2.2 · New title".into();
        a.update(next);
        assert_eq!(a.rows[1].label, "TMUX / Window 2 / Pane 2 - New title");
    }

    #[test]
    fn tree_totals_count_processes_once() {
        let mut a = app();
        a.view = View::Tree;
        a.update(snapshot(vec![
            process(1, 0, 1),
            process(2, 1, 2),
            process(3, 2, 3),
        ]));
        assert_eq!(a.rows[0].metrics.pss, 120);
        assert_eq!(a.rows[0].metrics.swap, 30);
        assert_eq!(a.rows[0].members.len(), 3);
        a.toggle();
        assert_eq!(a.rows[1].metrics.pss, 80);
    }
    #[test]
    fn selection_and_scroll_survive_refresh_and_insertion() {
        let mut a = app();
        a.viewport = 5;
        a.update(snapshot(
            (10..40).map(|i| process(i, 0, i as u64)).collect(),
        ));
        a.toggle();
        a.navigate(20);
        let id = a.selected.clone();
        let screen = a.index().unwrap() - a.offset;
        a.update(snapshot((9..40).map(|i| process(i, 0, i as u64)).collect()));
        assert_eq!(a.selected, id);
        assert_eq!(a.index().unwrap() - a.offset, screen);
    }
    #[test]
    fn resource_sort_follows_the_selected_measurement_without_resetting_history() {
        let mut a = app();
        a.sort = Sort::Memory;
        let mut first = process(10, 0, 10);
        let mut second = process(11, 0, 11);
        first.memory.as_mut().unwrap().pss = 100;
        second.memory.as_mut().unwrap().pss = 20;
        let mut sample = snapshot(vec![first, second]);
        a.update(sample.clone());
        a.set_depth(9);
        sample.at += std::time::Duration::from_secs(1);
        sample
            .processes
            .get_mut(&10)
            .unwrap()
            .memory
            .as_mut()
            .unwrap()
            .pss = 10;
        sample
            .processes
            .get_mut(&11)
            .unwrap()
            .memory
            .as_mut()
            .unwrap()
            .pss = 40;
        a.update(sample);
        assert_eq!(a.rows[1].key, Key::Process(Identity { pid: 11, start: 11 }));
        a.measurement = crate::history::Measurement::Peak1m;
        a.rebuild();
        assert_eq!(a.rows[1].key, Key::Process(Identity { pid: 10, start: 10 }));
        assert_eq!(
            a.measured(&a.rows[1], crate::history::Metric::Ram),
            (Some(100.0), true)
        );
        a.cycle_measurement();
        assert_eq!(a.history[&a.rows[1].key].len(), 2);
    }

    #[test]
    fn history_retains_a_boundary_sample_for_the_full_five_minutes() {
        let mut a = app();
        let mut s = snapshot(vec![process(10, 0, 10)]);
        let start = s.at;
        a.update(s.clone());
        s.at = start + std::time::Duration::from_secs(150);
        a.update(s.clone());
        s.at = start + std::time::Duration::from_secs(300);
        a.update(s.clone());
        let key = Key::Process(Identity { pid: 10, start: 10 });
        assert_eq!(a.history[&key].front().unwrap().at, start);
        s.at = start + std::time::Duration::from_secs(451);
        a.update(s);
        assert_eq!(
            a.history[&key].front().unwrap().at,
            start + std::time::Duration::from_secs(150)
        );
    }

    #[test]
    fn reused_pid_gets_new_identity_and_history() {
        let mut a = app();
        a.update(snapshot(vec![process(10, 0, 1)]));
        a.toggle();
        a.navigate(1);
        let old = a.selected.clone();
        a.update(snapshot(vec![process(10, 0, 2)]));
        assert_ne!(a.selected, old);
        assert_eq!(
            a.history[&Key::Process(Identity { pid: 10, start: 2 })].len(),
            1
        );
    }
    #[test]
    fn terminal_owners_keep_sessions_totals_and_signal_targets_separate() {
        let scope = "/user.slice/app-org.kde.konsole-50.scope/tab(60).scope";
        let mut konsole = process(50, 0, 50);
        konsole.name = "konsole".into();
        let mut shell = process(60, 50, 60);
        shell.tty_nr = 34817;
        shell.tty = "/dev/pts/1".into();
        shell.interactive_shell = true;
        let mut server = process(70, 0, 70); // daemonized, no Konsole ancestor
        server.name = "tmux: server".into();
        let mut pane = process(71, 70, 71);
        pane.tty_nr = 34818;
        pane.tty = "/dev/pts/2".into();
        pane.session_title = "tmux work:2.0 · Coding".into();
        let mut screen = process(80, 0, 80);
        screen.name = "screen".into();
        let mut dev_shell = process(81, 80, 81);
        dev_shell.tty_nr = 34820;
        dev_shell.tty = "/dev/pts/4".into();
        let mut worker = process(82, 81, 82);
        worker.tty_nr = 34822;
        worker.tty = "/dev/pts/6".into();
        let mut processes = vec![konsole, shell, server, pane, screen, dev_shell, worker];
        for p in &mut processes {
            p.cgroup = scope.into();
        }
        let mut a = app();
        a.update(snapshot(processes));
        a.set_depth(9);
        let graph = a.graph();
        for (owner, count) in [("Konsole", 2), ("TMUX", 2), ("Screen", 3)] {
            let row = &graph.nodes[&Key::Group(format!(
                "{} · {owner}",
                if owner == "Konsole" { "App" } else { "Shell" }
            ))];
            assert_eq!(row.metrics.count, count);
            assert_eq!(row.metrics.pss, count as u64 * 40);
        }
        let screen_session = Key::Group("Shell · Screen\x1fShell · /dev/pts/4".into());
        assert_eq!(graph.nodes[&screen_session].metrics.count, 2);
        assert!(
            !graph
                .nodes
                .contains_key(&Key::Group("Shell · Screen\x1fShell · /dev/pts/6".into()))
        );
        let pane_session = Key::Group("Shell · TMUX\x1fShell · /dev/pts/2".into());
        a.selected = Some(pane_session);
        a.prepare_signal(libc::SIGTERM, true);
        assert_eq!(
            a.confirmation.as_ref().unwrap().0,
            vec![Identity { pid: 71, start: 71 }]
        );
        // Flat mode retains session boundaries and once-only totals too.
        a.toggle_workload_tree();
        assert_eq!(a.nodes()[&screen_session].metrics.count, 2);
    }

    #[test]
    fn terminal_ancestry_and_rule_precedence() {
        let mut p = process(10, 0, 1);
        p.tty_nr = 123;
        p.tty = "/dev/pts/7".into();
        let child = process(11, 10, 2);
        let s = snapshot(vec![p, child.clone()]);
        assert_eq!(
            classify(&child, &s, &Config::default()).0,
            "Shell · /dev/pts/7"
        );
        let c: Config = toml::from_str(
            "[[rules]]\ngroup='Mine'\ncommand_contains='p11'\n[[rules]]\ngroup='Later'\nuid=1000",
        )
        .unwrap();
        assert_eq!(classify(&child, &s, &c).0, "Mine");
    }
    #[test]
    fn filter_reveals_matching_descendant_and_ancestors() {
        let mut a = app();
        a.view = View::Tree;
        a.filter = "p3".into();
        a.update(snapshot(vec![
            process(1, 0, 1),
            process(2, 1, 2),
            process(3, 2, 3),
            process(4, 1, 4),
        ]));
        assert_eq!(a.rows.len(), 3);
        assert_eq!(a.rows[2].depth, 2);
    }
    #[test]
    fn partial_totals_are_explicit() {
        let mut p = process(1, 0, 1);
        p.memory = None;
        let mut m = Metrics::default();
        m.add(&p);
        assert_eq!(m.pss_text(), "—");
        m.add(&process(2, 0, 2));
        assert!(m.pss_text().ends_with('*'));
    }
    #[test]
    fn cgroup_rollups_include_each_member_once() {
        let mut a = app();
        a.view = View::Cgroups;
        a.update(snapshot(vec![process(1, 0, 1), process(2, 1, 2)]));
        assert_eq!(a.rows[0].metrics.count, 2);
        a.toggle();
        assert_eq!(a.rows[1].metrics.pss, 80);
    }

    #[test]
    fn reparenting_updates_totals_without_losing_process_selection() {
        let mut a = app();
        a.view = View::Tree;
        a.update(snapshot(vec![
            process(1, 0, 1),
            process(2, 1, 2),
            process(3, 2, 3),
        ]));
        a.toggle();
        a.navigate(1);
        a.toggle();
        a.navigate(1);
        let selected = a.selected.clone();
        a.update(snapshot(vec![
            process(1, 0, 1),
            process(2, 1, 2),
            process(3, 1, 3),
        ]));
        assert_eq!(a.selected, selected);
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.key == Key::Process(Identity { pid: 2, start: 2 }))
                .unwrap()
                .metrics
                .count,
            1
        );
        assert_eq!(a.rows[0].metrics.count, 3);
    }
    #[test]
    fn infrastructure_roots_flatten_only_in_their_workload_sections() {
        for (name, pid, group) in [
            ("systemd", 1, "System services"),
            ("kthreadd", 2, "Kernel threads"),
        ] {
            let mut root = process(pid, 0, pid as u64);
            root.name = name.into();
            root.exe.clear();
            let mut child = process(20, pid, 20);
            child.name = "systemd-journald".into();
            child.exe.clear();
            for view in [View::Groups, View::Tree, View::Cgroups] {
                let mut a = app();
                a.view = view;
                a.update(snapshot(vec![root.clone(), child.clone()]));
                a.set_depth(9);
                let nodes = a.nodes();
                assert_eq!(
                    nodes.contains_key(&Key::Process(root.id)),
                    view != View::Groups
                );
                if view == View::Groups {
                    let section = &nodes[&Key::Group(group.into())];
                    assert_eq!(section.metrics.count, 2);
                    assert_eq!(section.metrics.pss, 80);
                    assert!(section.members.contains(&root.id));
                    assert_eq!(
                        a.graph().parents[&Key::Process(child.id)],
                        Some(Key::Group(group.into()))
                    );
                    assert_eq!(nodes[&Key::Process(child.id)].metrics.count, 1);
                    a.update(snapshot(vec![root.clone()]));
                    assert_eq!(a.nodes()[&Key::Group(group.into())].metrics.count, 1);
                }
            }
        }
    }

    #[test]
    fn workload_tree_aggregates_only_its_visual_children() {
        let mut a = app();
        a.update(snapshot(vec![
            process(10, 0, 10),
            process(11, 10, 11),
            process(12, 11, 12),
        ]));
        a.set_depth(4);
        assert_eq!(a.rows.len(), 4);
        assert_eq!(a.rows[0].metrics.count, 3);
        assert_eq!(a.rows[1].metrics.pss, 120);
        assert_eq!(a.rows[2].metrics.pss, 80);
        assert_eq!(a.rows[3].depth, 3);
        a.toggle_workload_tree();
        assert!(
            a.rows
                .iter()
                .skip(1)
                .all(|r| r.depth == 1 && r.metrics.count == 1)
        );
        assert_eq!(a.rows[0].metrics.count, 3);
    }

    #[test]
    fn depth_shortcuts_reach_hidden_nodes_and_persist_on_refresh_in_all_views() {
        for view in [View::Groups, View::Tree, View::Cgroups] {
            let ps = vec![process(10, 0, 10), process(11, 10, 11), process(12, 11, 12)];
            let mut a = app();
            a.view = view;
            a.update(snapshot(ps.clone()));
            a.set_depth(9);
            let full = a.rows.len();
            assert!(full >= 3);
            a.navigate(99);
            a.set_depth(1);
            assert!(a.rows.iter().all(|r| r.depth == 0 && !r.open));
            assert!(a.current().is_some());
            a.set_depth(2);
            assert!(a.rows.iter().all(|r| r.depth < 2));
            a.update(snapshot(ps));
            assert!(a.rows.iter().all(|r| r.depth < 2));
            a.set_depth(9);
            assert_eq!(a.rows.len(), full);
        }
    }

    #[test]
    fn hide_system_preserves_children_and_excludes_hidden_metrics_and_targets() {
        let mut manager = process(10, 0, 10);
        manager.name = "systemd".into();
        let mut child = process(11, 10, 11);
        child.cgroup = "/user.slice/app-editor.scope".into();
        for view in [View::Groups, View::Tree, View::Cgroups] {
            let mut a = app();
            a.view = view;
            a.update(snapshot(vec![manager.clone(), child.clone()]));
            a.toggle_system();
            a.set_depth(9);
            assert!(!a.rows.iter().any(|r| r.members.contains(&manager.id)));
            assert!(a.rows.iter().any(|r| r.key == Key::Process(child.id)));
            assert!(
                a.rows
                    .iter()
                    .all(|r| r.metrics.count == 1 && r.metrics.pss == 40)
            );
            a.selected = Some(Key::Process(child.id));
            a.prepare_signal(libc::SIGTERM, true);
            assert_eq!(a.confirmation.as_ref().unwrap().0, vec![child.id]);
            a.toggle_system();
            assert_eq!(
                a.nodes().contains_key(&Key::Process(manager.id)),
                view != View::Groups
            );
        }
    }

    #[test]
    fn linux_desktop_infrastructure_does_not_depend_on_uid() {
        for name in [
            "sddm-helper",
            "startplasma-way",
            "ksecretd",
            "systemd",
            "plasmashell",
            "pipewire",
        ] {
            let mut p = process(10, 0, 10);
            p.name = name.into();
            p.uid = 1000;
            assert!(
                crate::grouping::classify(&p, &snapshot(vec![p.clone()]), &Config::default())
                    .system,
                "{name}"
            );
        }
        let mut p = process(10, 0, 10);
        p.name = "my-server".into();
        p.uid = 0;
        p.cgroup = "/system.slice/my-server.service".into();
        assert!(
            !crate::grouping::classify(&p, &snapshot(vec![p.clone()]), &Config::default()).system
        );
    }

    #[test]
    fn chromium_snap_scopes_merge_but_slack_crashpad_stays_with_slack() {
        let mut browser = process(10, 0, 10);
        browser.name = "chrome".into();
        browser.exe = "/snap/chromium/3583/usr/lib/chromium-browser/chrome".into();
        browser.cgroup = "/user.slice/snap.chromium.chromium-abc.scope".into();
        let mut renderer = browser.clone();
        renderer.id = Identity { pid: 11, start: 11 };
        renderer.ppid = 10;
        renderer.cgroup = "/user.slice/snap.chromium.chromium-def.scope".into();
        let mut slack = process(12, 0, 12);
        slack.name = "chrome_crashpad".into();
        slack.exe = "/usr/lib/slack/chrome_crashpad_handler".into();
        let mut a = app();
        a.update(snapshot(vec![browser, renderer, slack]));
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.label == "App · Chromium")
                .unwrap()
                .metrics
                .count,
            2
        );
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.label == "App · Slack")
                .unwrap()
                .metrics
                .count,
            1
        );
        assert_eq!(a.rows.len(), 2);
    }

    #[test]
    fn docker_gui_and_service_are_one_application_with_a_services_section() {
        let mut backend = process(10, 0, 10);
        backend.name = "com.docker.back".into();
        backend.exe = String::new();
        backend.cgroup = "/user.slice/docker-desktop.service".into();
        let mut gui = process(11, 10, 11);
        gui.name = "Docker Desktop".into();
        gui.exe = "/opt/docker-desktop/Docker Desktop".into();
        gui.cgroup = "/user.slice/app-docker-desktop-11.scope".into();
        let mut a = app();
        a.update(snapshot(vec![backend.clone(), gui.clone()]));
        a.set_depth(9);
        assert_eq!(a.rows[0].label, "App · Docker");
        assert_eq!(a.rows[0].metrics.count, 2);
        let services = a.rows.iter().find(|r| r.label == "Services").unwrap();
        assert_eq!(services.members, vec![backend.id]);
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.key == Key::Process(gui.id))
                .unwrap()
                .metrics
                .count,
            1
        );
        a.toggle_system();
        assert_eq!(a.rows[0].metrics.count, 2);
    }

    #[test]
    fn regex_overrides_and_cross_group_boundaries_are_respected() {
        let mut c=Config::parse("[[applications]]\nname='My browser'\nexe_regex='/p1[01]$'\n[[rules]]\ngroup='Separate'\nname_regex='^p12$'").unwrap();
        c.workload_tree = true;
        let mut a = App::new(c, Default::default());
        a.update(snapshot(vec![
            process(10, 0, 10),
            process(11, 10, 11),
            process(12, 11, 12),
        ]));
        a.set_depth(9);
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.label == "App · My browser")
                .unwrap()
                .metrics
                .count,
            2
        );
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.key == Key::Process(Identity { pid: 10, start: 10 }))
                .unwrap()
                .metrics
                .count,
            2
        );
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.label == "Separate")
                .unwrap()
                .metrics
                .count,
            1
        );
    }

    #[test]
    fn explicit_system_override_keeps_a_named_service_visible() {
        let mut p = process(10, 0, 10);
        p.name = "ksecretd".into();
        let mut a=App::new(Config::parse("show_system=false\n[[rules]]\ngroup='My secrets'\nname_regex='^ksecretd$'\nsystem=false").unwrap(),Default::default());
        a.update(snapshot(vec![p]));
        assert_eq!(a.rows[0].label, "My secrets");
    }
    #[test]
    fn home_cwd_and_project_prefixes_use_tilde() {
        let home = std::path::Path::new("/home/demo");
        assert_eq!(format_label("zsh", "/home/demo/", Some(home)), "zsh  ~");
        assert_eq!(
            format_label("zsh", "/home/demo/project", Some(home)),
            "zsh  ~/project"
        );
        assert_eq!(
            format_label("zsh", "/home/demo2", Some(home)),
            "zsh  /home/demo2"
        );
    }

    #[test]
    fn versioned_claude_uses_process_name_and_matches_example_rule() {
        let mut p = process(10, 0, 10);
        p.name = "claude".into();
        p.exe = "/home/demo/.local/share/claude/versions/2.1.327".into();
        assert_eq!(process_name(&p), "claude");
        p.name = "2.1.327".into();
        assert_eq!(process_name(&p), "claude");
        p.name = "claude".into();
        let c = Config::parse(include_str!("../examples/config.toml")).unwrap();
        assert_eq!(
            classify(&p, &snapshot(vec![p.clone()]), &c).0,
            "Coding agents"
        );
        p.name = "chrome_crashpad".into();
        p.exe = "/usr/lib/slack/chrome_crashpad_handler".into();
        assert_eq!(process_name(&p), "chrome_crashpad_handler");
    }

    fn tmux_pair() -> (Process, Process) {
        let mut shell = process(100, 0, 100);
        shell.name = "zsh".into();
        shell.exe = "/usr/bin/zsh".into();
        shell.tty_nr = 34817;
        shell.tty = "/dev/pts/1".into();
        let mut client = process(101, 100, 101);
        client.name = "tmux: client".into();
        client.exe = "/usr/bin/tmux".into();
        client.tty_nr = 34817;
        client.tty = "/dev/pts/1".into();
        (shell, client)
    }

    #[test]
    fn tmux_client_folds_into_zsh_without_losing_metrics_or_signal_targets() {
        let (shell, client) = tmux_pair();
        for tree in [true, false] {
            let mut a = app();
            a.workload_tree = tree;
            a.update(snapshot(vec![shell.clone(), client.clone()]));
            a.set_depth(9);
            assert_eq!(a.rows.len(), 2);
            assert_eq!(a.rows[0].metrics.count, 2);
            let alias = &a.rows[1];
            assert_eq!(alias.label, "zsh: tmux client");
            assert_eq!(alias.metrics.pss, 80);
            assert_eq!(alias.metrics.swap, 20);
            assert_eq!(alias.metrics.cpu, 2.0);
            assert_eq!(alias.folded, vec![client.id]);
            a.selected = Some(Key::Process(shell.id));
            a.prepare_signal(libc::SIGTERM, false);
            assert_eq!(a.confirmation.as_ref().unwrap().0, vec![client.id]);
            a.prepare_signal(libc::SIGTERM, true);
            let targets = &a.confirmation.as_ref().unwrap().0;
            assert_eq!(targets.len(), 2);
            assert!(targets.contains(&shell.id));
            assert!(targets.contains(&client.id));
            a.filter = "101".into();
            a.rebuild();
            assert!(a.rows.iter().any(|r| r.label == "zsh: tmux client"));
        }
    }

    #[test]
    fn tmux_servers_pane_shells_and_raw_views_are_not_folded() {
        let (shell, mut server) = tmux_pair();
        server.name = "tmux: server".into();
        server.ppid = 1;
        server.tty_nr = 0;
        let mut pane = process(102, 101, 102);
        pane.name = "zsh".into();
        pane.exe = "/usr/bin/zsh".into();
        pane.tty_nr = 34818;
        pane.tty = "/dev/pts/2".into();
        let mut a = app();
        a.update(snapshot(vec![shell.clone(), server, pane]));
        assert!(a.nodes().values().all(|r| r.folded.is_empty()));
        let (_, client) = tmux_pair();
        for view in [View::Tree, View::Cgroups] {
            a.view = view;
            a.update(snapshot(vec![shell.clone(), client.clone()]));
            assert!(a.nodes().contains_key(&Key::Process(client.id)));
            assert!(a.nodes().values().all(|r| r.folded.is_empty()));
        }
    }

    #[test]
    fn tmux_folding_respects_group_boundaries_and_client_exit() {
        let (shell, client) = tmux_pair();
        let c =
            Config::parse("[[rules]]\ngroup='Separate tmux'\nname_regex='^tmux: client$'").unwrap();
        let mut a = App::new(c, Default::default());
        a.update(snapshot(vec![shell.clone(), client.clone()]));
        assert!(a.nodes().values().all(|r| r.folded.is_empty()));
        let mut a = app();
        a.update(snapshot(vec![shell.clone(), client]));
        a.set_depth(9);
        a.selected = Some(Key::Process(shell.id));
        a.update(snapshot(vec![shell.clone()]));
        assert_eq!(a.current().unwrap().label, "zsh");
        assert_eq!(a.current().unwrap().metrics.pss, 40);
        assert!(a.current().unwrap().folded.is_empty());
    }
    #[test]
    fn embedded_home_paths_respect_component_boundaries() {
        assert_eq!(
            shorten_home(
                "node /home/demo/a --cwd=/home/demo '/home/demo/b c'",
                "/home/demo"
            ),
            "node ~/a --cwd=~ '~/b c'"
        );
        assert_eq!(
            shorten_home("/home/demo2 /mnt/home/demo /home/demo-old", "/home/demo"),
            "/home/demo2 /mnt/home/demo /home/demo-old"
        );
        assert_eq!(
            shorten_home("/home/demo/.config/xtop/config.toml", "/home/demo"),
            "~/.config/xtop/config.toml"
        );
    }
    #[test]
    fn single_zsh_child_uses_child_identity_cwd_and_preserves_descendants() {
        let (shell, mut child) = tmux_pair();
        child.name = "claude".into();
        child.exe = "/opt/claude/2.1.327".into();
        child.cwd = "/workspace/project".into();
        child.tty_nr = 0;
        let mut grandchild = process(102, 101, 102);
        grandchild.tty_nr = 0;
        let mut a = app();
        a.update(snapshot(vec![
            shell.clone(),
            child.clone(),
            grandchild.clone(),
        ]));
        a.set_depth(9);
        let row = a
            .rows
            .iter()
            .find(|r| r.key == Key::Process(shell.id))
            .unwrap();
        assert_eq!(row.label, "zsh: claude  /workspace/project");
        assert_eq!(row.metrics.pss, 120);
        assert_eq!(row.metrics.count, 3);
        assert_eq!(row.folded, vec![child.id]);
        assert!(!a.rows.iter().any(|r| r.key == Key::Process(child.id)));
        assert!(
            a.rows
                .iter()
                .any(|r| r.key == Key::Process(grandchild.id) && r.depth == 2)
        );
        a.selected = Some(Key::Process(shell.id));
        a.prepare_signal(libc::SIGTERM, false);
        assert_eq!(a.confirmation.as_ref().unwrap().0, vec![child.id]);
    }

    #[test]
    fn a_second_child_prevents_folding_even_if_hidden() {
        let (shell, child) = tmux_pair();
        let mut second = process(102, 100, 102);
        second.name = "systemd".into();
        let mut a = app();
        a.show_system = false;
        a.update(snapshot(vec![shell.clone(), child.clone(), second]));
        a.set_depth(9);
        assert!(a.rows.iter().all(|r| r.folded.is_empty()));
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.key == Key::Process(shell.id))
                .unwrap()
                .label,
            "zsh"
        );
        a.selected = Some(Key::Process(child.id));
        a.update(snapshot(vec![shell.clone(), child]));
        assert_eq!(a.selected, Some(Key::Process(shell.id)));
        assert_eq!(
            a.rows
                .iter()
                .find(|r| r.key == Key::Process(shell.id))
                .unwrap()
                .label,
            "zsh: tmux client"
        );
    }

    #[test]
    fn nested_wrapper_chains_fold_without_losing_usage() {
        let (shell, mut middle) = tmux_pair();
        middle.name = "node".into();
        middle.exe = "/usr/bin/node".into();
        let mut inner = middle.clone();
        inner.id = Identity {
            pid: 102,
            start: 102,
        };
        inner.ppid = 101;
        let mut leaf = process(103, 102, 103);
        leaf.tty_nr = shell.tty_nr;
        leaf.tty = shell.tty.clone();
        let mut a = app();
        a.update(snapshot(vec![
            shell.clone(),
            middle.clone(),
            inner.clone(),
            leaf.clone(),
        ]));
        a.set_depth(9);
        assert_eq!(a.rows.len(), 2);
        assert_eq!(a.rows[0].metrics.count, 4);
        assert_eq!(a.rows[0].metrics.pss, 160);
        assert_eq!(a.rows[1].folded, vec![middle.id, inner.id, leaf.id]);
        assert_eq!(a.rows[1].metrics.pss, 160);
        assert_eq!(a.rows[1].label, "zsh: node: node: p103");
        a.selected = Some(Key::Process(shell.id));
        a.prepare_signal(libc::SIGTERM, false);
        assert_eq!(a.confirmation.as_ref().unwrap().0, vec![leaf.id]);
        a.prepare_signal(libc::SIGTERM, true);
        assert_eq!(a.confirmation.as_ref().unwrap().0.len(), 4);
    }
    #[test]
    fn wrapper_parents_fold_only_single_children() {
        for name in ["zsh", "dash", "bash", "fish", "node", "nodejs"] {
            let mut parent = process(100, 0, 100);
            parent.name = name.into();
            parent.exe = format!("/usr/bin/{name}");
            let mut child = process(101, 100, 101);
            child.name = "codex".into();
            child.exe = "/usr/bin/codex".into();
            let mut a = app();
            a.update(snapshot(vec![parent.clone(), child.clone()]));
            a.set_depth(9);
            assert_eq!(a.rows[1].label, format!("{name}: codex"));
            assert_eq!(a.rows[1].metrics.count, 2);
            assert_eq!(a.rows[1].metrics.pss, 80);
            a.update(snapshot(vec![parent, child, process(102, 100, 102)]));
            assert!(a.rows.iter().all(|r| r.folded.is_empty()));
            assert_eq!(a.rows[0].metrics.count, 3);
        }
    }
}
