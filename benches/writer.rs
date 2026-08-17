//! Criterion benchmarks for writing, transcoding, and converting GSYM data.

#[path = "benchmarks/convert.rs"]
mod convert;
#[expect(
    dead_code,
    reason = "the shared fixture also supports the separate reader benchmark target"
)]
#[path = "benchmarks/fixture.rs"]
mod fixture;
#[path = "benchmarks/transform.rs"]
mod transform;
#[path = "benchmarks/writer.rs"]
mod writer;

use std::time::Duration;

use criterion::{Criterion, criterion_main};

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
    use super::{configured, convert, transform, writer};
    use criterion::criterion_group;

    criterion_group! {
        name = benches;
        config = configured();
        targets = writer::benchmarks, transform::benchmarks, convert::benchmarks
    }
}

criterion_main!(harness::benches);
