use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput};
use gsym::{Endian, GsymVersion};

use super::fixture::{Shape, builder};

const FORMAT_FUNCTIONS: usize = 10_000;

pub(crate) fn benchmarks(criterion: &mut Criterion) {
    formats(criterion);
    scaling(criterion);
    record_richness(criterion);
}

fn formats(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("writer/format");
    group.throughput(Throughput::Elements(FORMAT_FUNCTIONS as u64));
    for version in [GsymVersion::V1, GsymVersion::V2] {
        for endian in [Endian::Little, Endian::Big] {
            group.bench_with_input(
                BenchmarkId::new(format!("{version:?}"), format!("{endian:?}")),
                &(version, endian),
                |bencher, &(version, endian)| {
                    bencher.iter_batched(
                        || builder(version, endian, FORMAT_FUNCTIONS, Shape::Lean),
                        |builder| {
                            black_box(builder.to_bytes().expect("fixture writer must succeed"))
                        },
                        BatchSize::LargeInput,
                    );
                },
            );
        }
    }
    group.finish();
}

fn scaling(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("writer/scaling");
    for count in [1_000, 10_000, 100_000] {
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &count,
            |bencher, &count| {
                bencher.iter_batched(
                    || builder(GsymVersion::V1, Endian::Little, count, Shape::Lean),
                    |builder| black_box(builder.to_bytes().expect("fixture writer must succeed")),
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

fn record_richness(criterion: &mut Criterion) {
    let count = 5_000;
    let mut group = criterion.benchmark_group("writer/record_richness");
    group.throughput(Throughput::Elements(count as u64));
    for shape in [Shape::Lean, Shape::Rich] {
        group.bench_function(shape.name(), |bencher| {
            bencher.iter_batched(
                || builder(GsymVersion::V1, Endian::Little, count, shape),
                |builder| black_box(builder.to_bytes().expect("fixture writer must succeed")),
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}
