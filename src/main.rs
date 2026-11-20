mod config;
mod grouping;
mod history;
mod model;
mod preferences;
mod procfs;
mod ui;

use anyhow::{Result, bail};
use std::{
    io::IsTerminal,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

fn main() -> Result<()> {
    let mut path = config::default_path();
    let mut explicit = false;
    let mut once = false;
    let mut depth = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                path = PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--config needs a path"))?,
                );
                explicit = true;
            }
            "--once" => once = true,
            "--depth" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--depth needs a level (1–9)"))?
                    .parse::<usize>()?;
                if !(1..=9).contains(&value) {
                    bail!("--depth must be between 1 and 9");
                }
                depth = Some(value);
            }
            "--help" | "-h" => {
                println!(
                    "xtop — Linux workload explorer\n\nUsage: xtop [--config PATH] [--once] [--depth 1–9]\n\nTab views · arrows navigate · Enter expand · / filter · ? help · q quit\nDefault configuration: {}",
                    model::display_home(&path.display().to_string())
                );
                return Ok(());
            }
            "--version" | "-V" => {
                println!("xtop {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => bail!("Unknown option: {arg}"),
        }
    }
    let config = config::Config::load(&path, explicit)?;
    if once {
        let mut c = procfs::Collector::new(config.clone());
        let s = c.sample()?;
        println!(
            "{} processes · PSS readable for {} · RAM {} · swap {}",
            s.processes.len(),
            s.processes.values().filter(|p| p.memory.is_some()).count(),
            model::bytes(s.system.total),
            model::bytes(s.system.swap_used)
        );
        let mut app = model::App::new(config, path);
        app.update(s);
        if let Some(level) = depth {
            app.set_depth(level);
        }
        for row in &app.rows {
            println!(
                "{:<60} {:>9} {:>9} {:>9}",
                format!("{}{}", "  ".repeat(row.depth), row.label),
                row.metrics.pss_text(),
                row.metrics.swap_text(),
                row.metrics.cpu_text()
            );
        }
        return Ok(());
    }
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("Interactive mode requires a terminal; use --once for a snapshot");
    }
    // A bounded queue prevents stale snapshots piling up while a popup is open.
    let (tx, rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker_config = config.clone();
    let worker = std::thread::spawn(move || {
        let mut collector = procfs::Collector::new(worker_config.clone());
        while !worker_stop.load(Ordering::Relaxed) {
            let started = std::time::Instant::now();
            let result = collector.sample().map_err(|e| format!("{e:#}"));
            if let Err(mpsc::TrySendError::Disconnected(_)) = tx.try_send(result) {
                break;
            }
            while started.elapsed() < Duration::from_millis(worker_config.refresh_ms)
                && !worker_stop.load(Ordering::Relaxed)
            {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    });
    let mut app = model::App::new(config, path);
    preferences::restore(&mut app);
    if let Some(level) = depth {
        app.set_depth(level);
    }
    let result = ui::run(app, rx);
    stop.store(true, Ordering::Relaxed);
    let _ = worker.join();
    result
}
