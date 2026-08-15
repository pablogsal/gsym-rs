//! Criterion benchmarks for the reader, writer, transcoder, and converter.

mod benchmarks;

use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .sample_size(30);
    targets =
        benchmarks::reader::benchmarks,
        benchmarks::writer::benchmarks,
        benchmarks::transform::benchmarks,
        benchmarks::convert::benchmarks
}
criterion_main!(benches);
