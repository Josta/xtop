# xtop

A Linux process explorer focused on workloads, readable identities, and stable
navigation. The initial version is a Rust/Ratatui TUI. No service, telemetry,
automatic process management, or changes to your existing cgroups.

## Run

Build on Linux with Rust 1.88 or newer.

```sh
git clone https://github.com/Josta/xtop.git
cd xtop
cargo build --release --locked
./target/release/xtop
```

Use `./target/release/xtop --once` for a plain-text snapshot, or add `--depth 3`
to include three hierarchy levels (also sets the initial depth in the TUI). Run normally as your
own user; use `sudo ./target/release/xtop` if you need access to other users'
memory accounting. Root also gives signal controls broader permissions.

## Interaction

The summary uses three lines, irrespective of CPU count. Most horizontal space
belongs to workload/process names and working directories. CWDs appear in muted
text beside the name, without a dot separator. The current user's
home directory displays as `~`; paths below it display as `~/…`, including
paths in commands and the settings panel. Matching still uses the full paths. Version-named native
binaries use their process name, so Claude displays as `claude`, not `2.1.327`.

| Key | Action |
| --- | --- |
| `Tab` | Workloads → process tree → cgroups |
| `↑` / `↓`, `j` / `k` | Select a row |
| `PgUp` / `PgDn`, `Home` / `End` | Navigate larger lists |
| `Enter` / `Space` | Expand/collapse one node |
| `←` / `h`, `→` / `l` | Collapse/parent, expand/child |
| `e`, `c` | Expand visible level, collapse all |
| `1`–`9` | Show that many hierarchy levels: `1` roots only, `2` their children, etc. |
| `S` | Show/hide Linux/KDE system processes in every view |
| `H` | Toggle hierarchical/flat processes inside workloads |
| `/` | Search names, full commands, directories, terminals, PIDs, cgroups |
| `Enter` in search, `Esc` | Keep filter, clear filter |
| `s` | Cycle sibling sorting: name / PSS / CPU |
| `o` / `F2` | Visible settings: columns, details/history, mouse |
| `d` | Toggle details/history panel |
| `p` | Freeze/unfreeze the display |
| `t` | Preview SIGTERM for selected process |
| `x` | Preview SIGKILL for selected process |
| `K` (Shift+k) | Preview SIGTERM for selected group or subtree |
| `?` / `F1` | Help |
| `q` / `Ctrl+C` | Quit |

Mouse wheel navigates; click selects; clicking the selected row again expands
or collapses. Selection is tracked by PID **and start time** and remains at the
same screen position when possible, even as rows reorder. Signal previews show
a frozen target list, scrollable with arrows, and require `y` to send. Linux
pidfds bind signals to the intended process instance. PID 1 and xtop itself
are excluded. New children appearing after the preview are not included.

At widths below 90 columns, PID, swap, and combined columns are automatically
hidden to retain readable names. Details are hidden below 24 rows. Minimum size
is 48 columns × 12 rows.

## Grouping

Application identity unifies separate application scopes and background service
units. Built-in recognition covers Chrome/Chromium (including Snap), Docker,
Slack and Firefox. For example, Docker's GUI and daemon are one application,
with the daemon processes in its **Services** subsection. Reparented helpers can
still match by executable/cgroup; current ancestry also associates helpers with
known applications. Embedded Chromium helpers stay with their host app.

Other application names come from local `.desktop` files where available, then
from cleaned systemd scope names (instance IDs removed, escapes decoded).
Explicit TOML application overrides take precedence over these inferred names.

Terminal workloads belong to an owning application, with sessions as subsections.
TMUX servers and their pane processes form **TMUX**; GNU Screen servers and their
session descendants form **Screen**. These owners take precedence over inherited
terminal-emulator scopes, including after the servers daemonize. Nested PTYs from
Screen development commands remain in their outer Screen session. A tmux client
stays in its launching terminal's session, while the tmux server owns its panes.
Other terminals belong to their application scope (for example **Konsole**).
Unidentified terminal owners retain standalone shell sections. Explicit TOML rules
and direct application recognition still apply. Session totals and whole-application
totals each count their members once; signaling a session excludes sibling sessions.

`S` hides processes classified as Linux/KDE infrastructure: kernel threads,
init scopes, systemd managers, login/session components, accessibility,
audio/network/printing components and other recognized standard daemons.
This is an editable heuristic, **not** a package-install-history detector and
**not** a Unix-user distinction. A root-owned custom service remains visible;
`sddm-helper`, `ksecretd` and user systemd belong to system services. Unknown
services remain in **Other services**. Details state the classification reason.

Hierarchical workloads are enabled by default. Child processes nest under their
nearest displayed parent within the **same workload/session/Services section**, and
parent values include those visual descendants. `H` switches to flat mode.
Workload totals count every member once in either mode. Hiding system processes
promotes their remaining children and excludes hidden processes from displayed
PSS/CPU totals and signal targets. Whole-machine memory bars and raw cgroup
charges still include the hidden processes.

In workload view, `zsh`, `dash`, `bash`, `fish`, `node`, and `nodejs` parents
with exactly one direct child fold into a combined row when all processes share a workload section.
Consecutive wrappers fold into a chain such as **zsh: node: codex**. Each
process contributes to the totals exactly once. The PID, muted CWD and command
details identify the final child; details also identify the outer wrapper.
`t`/`x` target the final child, while `K` includes the entire chain and visual
subtree, with the usual signal preview.

All snapshot children count, including hidden system processes. A second child
stops folding at that parent. Process-tree and cgroup views retain real process
rows. Explicit TOML rules that separate parent and child into different groups
are honored.

Number keys set a depth limit for the whole current view, including currently
collapsed nodes. The limit persists across refreshes and newly appearing nodes;
manual expansion overrides it. Starting a search returns to automatic expansion
of matching paths. Applying a number during search imposes that depth limit.

Create `~/.config/xtop/config.toml` (or `$XDG_CONFIG_HOME/xtop/config.toml`):

```toml
refresh_ms = 1000
memory_refresh_ms = 5000
history_seconds = 300
mouse = true
show_system = false
workload_tree = true

[[applications]]
name = "My browser"
cgroup_regex = '(^|/)snap\.chromium\.'

[[rules]]
group = "Coding agents"
exe_contains = "codex"

[[rules]]
group = "My project"
cwd_prefix = "/home/me/projects/my-project"
```

Or try the local example:

```sh
./target/release/xtop --config examples/config.toml
```

Rules are **first-match-wins**. Custom `[[rules]]` precede `[[applications]]`,
which precede built-in recognition. Matchers within one table are ANDed.
Multiple application tables with the same `name` merge into one application.

Both table types support `exe_contains`, `command_contains`, `cwd_prefix`,
`cgroup_contains`, numeric `uid`, and `exe_regex`, `command_regex`,
`cgroup_regex`, `name_regex`. The latter matches Linux's process name (`comm`,
which may be truncated); `exe_regex` matches the full executable path. Regexes
use Rust regex syntax, compile once at startup, and invalid expressions are
reported as configuration errors. Use single-quoted TOML strings for regexes.
Literal matchers are case-sensitive. Paths do not expand `~` or environment
variables; `cwd_prefix` matches path components.

Optional `system = true/false` overrides the system flag. For custom workload
rules the flag otherwise retains built-in system classification; an explicit
application override defaults to non-system. Optional `service = true/false`
overrides placement in a **Services** subsection. Application matches default
to detecting a service unit, excluding the encompassing `user@` manager and
GUI autostart entries. Custom workload rules default to no service subsection.

Custom workload rules match each process individually; they don't automatically
capture descendants. Application recognition can follow current ancestry.
Groups never change kernel cgroup membership. Restart after editing the live
configuration; UI preferences save automatically in a separate state file.

## Where grouping comes from

| Layer | Source | Examples |
| --- | --- | --- |
| General heuristics | Built-in Rust code | Terminal ancestry, application scopes, service membership, process hierarchy, desktop-entry display names |
| Well-known identities | Built-in Rust rules | Chrome/Chromium, Docker, Slack, Firefox; known Linux/KDE processes and units |
| Local workload rules | `[[rules]]` in TOML | Coding agents, Work projects, Personal projects |
| Local application overrides | `[[applications]]` in TOML | Friendly names and regex matches spanning scopes; optional system/service overrides |

Precedence is custom workload rules → application overrides → built-in app
recognition → known system infrastructure → terminal/application ancestry →
application scopes → fallback. A matching table's conditions are ANDed, and
first match wins. System status is independent of Unix UID.

The shipped example contains four workload rules forming three groups: Codex
by executable, Claude by process name, and two project-directory prefixes. Its
two application overrides (Chromium and Docker) demonstrate configuration;
built-in recognition also covers these applications. The coding-agent category
itself is configured, not built in. Home-directory abbreviation, versioned-binary
labels and single-child zsh/Node chain folding are built-in presentation rules. Process details expose
the reason for classification.

## Accounting

- **RAM·PSS:** resident memory divided proportionally among processes sharing
  pages. Summing it avoids RSS's repeated counting of shared pages.
- **SWAP·PSS:** proportional swap (`SwapPss`), with the kernel's limitations for
  swapped shared-memory objects. This is logical swap usage, not the compressed
  physical RAM consumed by zram/zswap.
- **TOTAL:** PSS + SwapPss, enabled in settings.
- **CPU cores:** 1.00 is one fully occupied logical CPU. The system summary is instead
  normalized to the entire machine.
- **`—`:** measurement unavailable; **`*`:** partial aggregate. Unreadable PSS
  is never silently replaced with RSS. Kernel threads have no userspace PSS.
- Process-tree parent rows include current displayed descendants. Hierarchical
  workload parents include descendants within their visual section. Group/cgroup
  rows include all non-hidden members even during text filtering. Do not add parent totals
  to child totals. Individual RSS remains available in process details.
- Cgroup details separately show `memory.current` and `memory.swap.current`;
  cgroup charges include cache/kernel memory and are not equivalent to PSS.
- The system RAM bar shows used / estimated reclaimable cache / free. Cache is
  `Cached + Buffers + SReclaimable - Shmem`; `MemAvailable` is shown separately.
  This is a useful composition estimate, not a per-page reconciliation.

Collection runs on a background thread. CPU/process identity is sampled every
second by default; expensive PSS reads every five seconds. Memory remains a
cached snapshot between reads. Processes can exit while `/proc` is being read;
vanished or reused identities are discarded. A whole-machine snapshot is not
atomic. Restricted `/proc`, user namespaces, and permissions limit coverage.

## History and limitations

Recent CPU/PSS/swap trends are retained in memory for the current view,
including collapsed rows. Defaults retain five minutes; config supports up to
one hour. Switching views, system visibility, or workload hierarchy starts a new history. Charts fit the retained samples
to their width using maximum-per-bucket values, preserve spikes, and auto-scale
each series. Missing readings render as zero/partial values, so use coverage
labels to distinguish them from actual zero usage. Pausing introduces a gap;
charts are sample-indexed, not a continuous persisted time axis.

Not yet implemented: historical detached-process ancestry, persistent recording,
configuration editing inside the TUI, cross-view aggregate histories, and an independent memory sampler. Long PSS
walks can delay the next CPU snapshot, but do not block keyboard interaction.
Help and details truncate on small terminals; the full reference is here.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Tests cover memory parsing, unusual process names, aggregate accounting,
terminal identification, regex overrides, Chrome/Docker association, system
filtering, hierarchy depth, visual totals, selection/scroll stability, PID reuse,
terminal layouts, and pidfd signaling of a disposable
test child. The implementation requires Linux with pidfd support (Linux 5.3+
for the signal workflow); cgroup view uses cgroup v2.

In workload view, the `systemd` root in System services and `kthreadd` root in
Kernel threads are flattened: their children appear directly in the section.
Their own usage remains included in section totals. Raw tree/cgroup views retain them.

Session identification: TMUX subsection headings show the window/pane indices
and pane title, refreshed every five seconds from the current/default tmux server.
Titles also appear in process details and are searchable. Custom tmux sockets
outside that server and ordinary terminal window titles are not queried yet.
A bounded read-only query keeps an unavailable tmux server from stalling collection.

The AGE column shows elapsed time since process start (including time before
xtop started). Folded chains use the final child's age, matching the displayed PID.
Age is also in details; toggle its column in Settings. Narrow terminals hide the
column automatically. Group ages are left blank.

Memory columns use decimal MB (1 MB = 1,000,000 bytes), with one decimal
place and right-aligned values. Partial markers keep their own position so
decimal points line up. Colors indicate absolute size: cyan from 100 MB, amber
from 1,000 MB, red from 4,000 MB; zero/unavailable values are muted. These are
size cues, not system memory-pressure warnings. RAM, swap and TOTAL share this scale.

Repeated displayed CPU/RAM/swap/TOTAL values use a double quote (`"`) as a
ditto mark. Each column starts with a full value on the first visible row,
including after scrolling. Colors and partial (`*`) markers are preserved;
unavailable values remain `—`.

Shell headings display `Shell #<pts number>`, or
`TMUX / Window <index> / Pane <index> - <title>` for known tmux panes.
Name sorting keeps shells together: TMUX first (numeric window/pane order,
session name breaks ties), then interactive shells, then background terminals.
Both non-tmux categories use numeric pts order and are labeled in the list.
Interactive detection is a heuristic: a recognized shell explicitly requests
interactive mode, or has terminal stdin/stderr without a command/script argument.
Background runners can also own PTYs; a TTY alone does not imply interactivity.
Details retain the full terminal path; stable section identities survive title changes.

CPU cells show right-aligned logical core counts with two decimals. Values that
round to zero display `0   `, aligning the integer part with other values.
CPU colors: cyan from 0.10 cores, amber from 1.00, red from 4.00; zero/unknown
values are muted. Zero CPU and memory values never become ditto marks.

Table columns are process name, CPU, RAM, swap, optional footprint, AGE, then PID.
The table fits visible names and column widths, with an 80-column minimum
when space permits, capped at terminal width. Expanding, filtering, scrolling
and resizing recalculate its width so metrics stay near process names.

## Measurement mode

Settings (`o`) has one toggle each for CPU, RAM, SWAP and Footprint (RAM + swap).
The Measurement setting and hotkey `m` cycle through **live → 1 min average →
5 min average → 1 min peak → 5 min peak → live**. All four columns use the same
mode, shown prominently in the header; column labels stay plain. CPU and memory
sorting follow the selected measurement. Cycling does not reset retained history.
Units, alignment, colors and ditto behavior are unchanged; AGE and PID remain last.
Details/history charts continue showing live totals and retained samples.

Averages are time-weighted over known sampled intervals, excluding missing data
and long collection gaps. CPU samples describe the preceding interval; memory
samples are held until the next observation (including the memory-refresh cache).
Peaks are observed sample maxima, not a guarantee of capturing brief spikes.
Group statistics use recorded group totals, never sums of individual peaks.
`*` marks less than a full window, missing intervals, or partial process coverage;
`—` means no usable data (an average needs elapsed time between samples).
History starts when xtop starts and resets when switching view, system visibility,
or workload hierarchy. Membership changes contribute the totals observed then.
The existing default retention of 300 seconds supports both windows; a shorter
configured retention produces partial five-minute results.

## Remembered preferences

Interactive xtop automatically saves changes to columns and measurement mode, selected view, sort order, system visibility, workload hierarchy,
details, mouse interaction, and the numeric expansion-depth setting. Preferences
live beside the selected config: normally `~/.config/xtop/config.state.toml` (or
under `XDG_CONFIG_HOME`). A custom `--config example.toml` uses `example.state.toml`.
Writes replace the state file atomically; grouping rules in config.toml are never
rewritten. Concurrent instances use the last saved settings.

Saved preferences override config display defaults; `--depth` overrides remembered
depth for that launch. Delete the state file to return to defaults. Unreadable or
invalid state produces a status message and leaves startup defaults in place.
Search, selected process, scroll position, pause, pending signals, individual node
expansion, and resource history are not restored. `--once` uses config/CLI defaults
and does not read or write UI preferences. Older running versions cannot export
their settings: restart once, then make your selections in the new version.

Older saved history-column selections migrate to one shared mode (average takes
precedence if both average and peak were enabled), preserving the selected window
and enabling the corresponding resource columns.
