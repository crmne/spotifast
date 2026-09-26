//! Damping and release curves for elastic overscroll.

/// Maximum visible pull in points.
pub const MAX_PULL: f32 = 64.0;

/// Release duration in seconds.
pub const RELEASE_SECONDS: f32 = 0.22;

/// Smallest visible offset.
const SETTLED: f32 = 0.25;

/// Maps an unbounded pull to a visible offset below [`MAX_PULL`].
pub fn damp(pull: f32) -> f32 {
    // This order stays finite for very large pulls.
    let pull = pull.max(0.0);
    MAX_PULL * (pull / (pull + MAX_PULL))
}

/// Recovers the pull behind a visible offset.
pub fn undamp(offset: f32) -> f32 {
    // Avoid the singularity at MAX_PULL.
    let offset = offset.clamp(0.0, MAX_PULL - SETTLED);
    MAX_PULL * offset / (MAX_PULL - offset)
}

/// Eases an offset back to zero with smoothstep.
pub fn released(from: f32, elapsed: f32) -> f32 {
    if elapsed >= RELEASE_SECONDS {
        return 0.0;
    }
    let t = (elapsed / RELEASE_SECONDS).clamp(0.0, 1.0);
    let progress = t * t * (3.0 - 2.0 * t);
    let eased = from * (1.0 - progress);
    if eased.abs() < SETTLED { 0.0 } else { eased }
}

/// Whether an offset is too small to draw.
pub fn settled(offset: f32) -> bool {
    offset.abs() < SETTLED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_points_of_a_pull_move_the_content_almost_one_for_one() {
        assert!(damp(1.0) > 0.98, "got {}", damp(1.0));
        assert!(damp(4.0) > 0.94 * 4.0, "got {}", damp(4.0));
    }

    #[test]
    fn a_pull_gives_back_less_the_further_it_goes() {
        let steps: Vec<f32> = (0..8).map(|i| damp(i as f32 * 40.0)).collect();
        for window in steps.windows(3) {
            let [first, second, third] = [window[0], window[1], window[2]];
            assert!(
                second - first > third - second,
                "{first} -> {second} -> {third} did not shrink",
            );
        }
    }

    #[test]
    fn a_pull_never_reaches_the_limit_however_hard_it_is_scrolled() {
        assert!(damp(10_000.0) < MAX_PULL);
        assert!(damp(f32::MAX) <= MAX_PULL);
        assert_eq!(damp(0.0), 0.0);
        assert_eq!(damp(-50.0), 0.0);
    }

    #[test]
    fn undamp_returns_the_pull_that_produced_an_offset() {
        for pull in [0.0, 1.0, 25.0, 120.0, 600.0] {
            let round_trip = undamp(damp(pull));
            assert!(
                (round_trip - pull).abs() < 0.5,
                "{pull} came back as {round_trip}",
            );
        }
    }

    #[test]
    fn undamp_stays_finite_at_and_beyond_the_limit() {
        assert!(undamp(MAX_PULL).is_finite());
        assert!(undamp(MAX_PULL * 2.0).is_finite());
    }

    #[test]
    fn a_release_starts_and_finishes_gently() {
        let from = damp(200.0);
        let quarter = released(from, RELEASE_SECONDS * 0.25);
        let three_quarters = released(from, RELEASE_SECONDS * 0.75);

        let early_travel = from - quarter;
        let middle_travel = quarter - three_quarters;
        let late_travel = three_quarters;

        assert!(
            early_travel < middle_travel && late_travel < middle_travel,
            "traveled {early_travel} early, {middle_travel} midway, and {late_travel} late",
        );
    }

    #[test]
    fn a_release_ends_at_exactly_zero() {
        let from = damp(200.0);
        assert_eq!(released(from, RELEASE_SECONDS), 0.0);
        assert_eq!(released(from, RELEASE_SECONDS * 10.0), 0.0);
        assert_eq!(released(0.1, 0.0), 0.0);
    }

    #[test]
    fn a_release_keeps_the_side_it_started_on() {
        let up = released(-damp(200.0), RELEASE_SECONDS * 0.5);
        assert!(up < 0.0, "got {up}");
    }
}
