//! Criterion benchmarks for the reader, writer, transcoder, and converter.

mod benchmarks;

use std::time::Duration;

use criterion::{Criterion, criterion_main};

/// Shared Criterion settings.
fn configured() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .sample_size(30)
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the CodSpeed compatibility macro owns and drops its generated runner"
)]
mod harness {
    use super::{benchmarks, configured};
    use criterion::criterion_group;

    criterion_group! {
        name = benches;
        config = configured();
        targets =
            benchmarks::reader::benchmarks,
            benchmarks::writer::benchmarks,
            benchmarks::transform::benchmarks,
            benchmarks::convert::benchmarks
    }
}

criterion_main!(harness::benches);
