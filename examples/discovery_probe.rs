//! Compare batch publication with live Cast publication, without signing in.
//! `cargo run --example discovery_probe -- [--batch] [runs]`

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let batch = args.iter().any(|arg| arg == "--batch");
    let runs = args
        .iter()
        .find_map(|arg| arg.parse::<usize>().ok())
        .unwrap_or(3);
    for run in 1..=runs {
        let start = Instant::now();
        let first = AtomicU64::new(u64::MAX);
        let receivers =
            spotifast::zeroconf::discover_with_updates(Duration::from_secs(3), |rows| {
                if !batch && !rows.is_empty() {
                    let _ = first.compare_exchange(
                        u64::MAX,
                        start.elapsed().as_millis() as u64,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
            })?;
        let receivers = spotifast::zeroconf::resolve_receivers(receivers)?;
        let final_ms = start.elapsed().as_millis();
        if batch && receivers.iter().any(|row| row.cast) {
            first.store(final_ms as u64, Ordering::Relaxed);
        }
        let first = first.load(Ordering::Relaxed);
        println!(
            "run={run} mode={} first_cast_ms={} complete_ms={final_ms} cast_count={} total_count={}",
            if batch { "batch" } else { "stream" },
            if first == u64::MAX {
                "none".into()
            } else {
                first.to_string()
            },
            receivers.iter().filter(|row| row.cast).count(),
            receivers.len()
        );
    }
    Ok(())
}
