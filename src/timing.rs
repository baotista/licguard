//! The timing line: how long a run took and how many registry requests it
//! sent, so that a developer tells a warm-cache run from a cold one. It goes
//! to stderr only, so that the report stays deterministic (NF-03).

use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

use crate::evaluation::Evaluation;

/// Prints the timing line of a run that started at `start` and evaluated
/// `evaluation` on stderr, when `forced` (`--timings`) or stderr is a
/// terminal.
pub fn print(forced: bool, start: Instant, evaluation: &Evaluation) {
    if forced || io::stderr().is_terminal() {
        eprintln!(
            "licguard: {} packages in {} ({} registry {})",
            evaluation.evaluated.len(),
            duration(start.elapsed()),
            evaluation.requests,
            if evaluation.requests == 1 {
                "request"
            } else {
                "requests"
            }
        );
    }
}

/// `elapsed` with one decimal under 10 s, e.g. `1.8 s`, else in whole
/// seconds, e.g. `12 s`.
fn duration(elapsed: Duration) -> String {
    let tenths = (elapsed.as_secs_f64() * 10.0).round();
    if tenths < 100.0 {
        format!("{:.1} s", tenths / 10.0)
    } else {
        format!("{:.0} s", elapsed.as_secs_f64())
    }
}
