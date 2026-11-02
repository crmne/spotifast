// Included by benchmark-limiter.py after it selects the two real source files.
use std::hint::black_box;
use std::time::Instant;

const FRAMES: usize = 1024;
const BLOCKS: usize = 500;
const ROUNDS: usize = 9;

fn time(mut process: impl FnMut(&mut [f64]), input: &[f64]) -> f64 {
    let mut packet = input.to_vec();
    for _ in 0..20 {
        packet.copy_from_slice(input);
        process(black_box(&mut packet));
    }
    let start = Instant::now();
    for _ in 0..BLOCKS {
        packet.copy_from_slice(black_box(input));
        process(black_box(&mut packet));
        black_box(&packet);
    }
    start.elapsed().as_secs_f64() * 1e6 / BLOCKS as f64
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn main() {
    println!("case,baseline_us_per_block,candidate_us_per_block,reduction_percent");
    for (name, amplitude, ceiling) in [
        ("quiet", 0.5, 1.0),
        ("near_knee", 0.9, 1.0),
        ("boosted", 4.0, 1.0),
        ("volume_applied_later", 1.0, 4.0),
    ] {
        let input: Vec<f64> = (0..FRAMES)
            .flat_map(|frame| {
                let phase = frame as f64 * std::f64::consts::TAU * 440.0 / 44_100.0;
                [amplitude * phase.sin(), amplitude * (phase + 0.3).sin()]
            })
            .collect();
        // Also check complete output, with state retained across packets.
        let mut old = baseline::Limiter::new(44_100.0);
        let mut new = candidate::Limiter::new(44_100.0);
        for _ in 0..10 {
            let mut a = input.clone();
            let mut b = input.clone();
            old.process(&mut a, ceiling);
            new.process(&mut b, ceiling);
            assert!(a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 1e-12));
        }
        let mut before = Vec::new();
        let mut after = Vec::new();
        for round in 0..ROUNDS {
            let mut old = baseline::Limiter::new(44_100.0);
            let mut new = candidate::Limiter::new(44_100.0);
            // Alternate order to reduce bias from warming and CPU scheduling.
            if round % 2 == 0 {
                before.push(time(|samples| old.process(samples, ceiling), &input));
                after.push(time(|samples| new.process(samples, ceiling), &input));
            } else {
                after.push(time(|samples| new.process(samples, ceiling), &input));
                before.push(time(|samples| old.process(samples, ceiling), &input));
            }
        }
        let (before, after) = (median(before), median(after));
        println!(
            "{name},{before:.3},{after:.3},{:.1}",
            (1.0 - after / before) * 100.0
        );
    }
}
