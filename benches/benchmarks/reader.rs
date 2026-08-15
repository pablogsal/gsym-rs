use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput};
use gsym::{Endian, FrameLookupOptions, Gsym, GsymVersion, LookupOptions, LookupScratch};

use super::fixture::{
    BASE_ADDRESS, FUNCTION_STRIDE, Shape, encoded, gap, hit, mixed_queries, shuffled_hits,
    sorted_hits,
};

const INDEXED_FUNCTIONS: usize = 100_000;
const RICH_FUNCTIONS: usize = 10_000;
const BATCH_QUERIES: usize = 4_096;

pub(crate) fn benchmarks(criterion: &mut Criterion) {
    parse(criterion);
    lookup_scaling(criterion);
    lookup_records(criterion);
    batches(criterion);
    full_scans(criterion);
    file_open(criterion);
}

fn parse(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("reader/parse_memory");
    for version in [GsymVersion::V1, GsymVersion::V2] {
        for endian in [Endian::Little, Endian::Big] {
            let bytes = encoded(version, endian, INDEXED_FUNCTIONS, Shape::Lean);
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(
                BenchmarkId::new(format!("{version:?}"), format!("{endian:?}")),
                bytes.as_slice(),
                |bencher, bytes| {
                    bencher.iter(|| {
                        black_box(
                            Gsym::parse(black_box(bytes))
                                .expect("known-valid benchmark image must parse"),
                        )
                    });
                },
            );
        }
    }
    group.finish();
}

fn lookup_scaling(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("reader/address_index_scaling");
    group.throughput(Throughput::Elements(1));
    for count in [1_000, 10_000, INDEXED_FUNCTIONS] {
        let bytes = encoded(GsymVersion::V1, Endian::Little, count, Shape::Lean);
        let gsym = Gsym::parse(bytes.as_slice()).expect("fixture must parse");
        let address = hit(count / 2);
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &address,
            |bencher, address| {
                bencher.iter(|| {
                    black_box(
                        gsym.lookup(black_box(*address))
                            .expect("known-valid lookup must not fail"),
                    )
                });
            },
        );
    }
    group.finish();
}

fn lookup_records(criterion: &mut Criterion) {
    let lean_bytes = encoded(GsymVersion::V1, Endian::Little, RICH_FUNCTIONS, Shape::Lean);
    let rich_bytes = encoded(GsymVersion::V1, Endian::Little, RICH_FUNCTIONS, Shape::Rich);
    let lean = Gsym::parse(lean_bytes.as_slice()).expect("lean fixture must parse");
    let rich = Gsym::parse(rich_bytes.as_slice()).expect("rich fixture must parse");
    let address = hit(RICH_FUNCTIONS / 2);
    let mut group = criterion.benchmark_group("reader/lookup_records");
    group.throughput(Throughput::Elements(1));

    group.bench_function("lean_allocating", |bencher| {
        bencher.iter(|| {
            black_box(
                lean.lookup(black_box(address))
                    .expect("lean lookup must not fail"),
            )
        });
    });
    group.bench_function("rich_allocating", |bencher| {
        bencher.iter(|| {
            black_box(
                rich.lookup(black_box(address))
                    .expect("rich lookup must not fail"),
            )
        });
    });

    let mut scratch = LookupScratch::with_capacity(4);
    group.bench_function("rich_without_optional_records", |bencher| {
        let options = LookupOptions {
            line_information: false,
            inline_frames: false,
            call_sites: false,
        };
        bencher.iter(|| {
            black_box(
                rich.lookup_with_options(black_box(address), options, &mut scratch)
                    .expect("option-filtered lookup must not fail"),
            )
        });
    });

    let mut scratch = LookupScratch::with_capacity(4);
    group.bench_function("rich_frame_visitor", |bencher| {
        bencher.iter(|| {
            let mut frames = 0_usize;
            let found = rich
                .for_each_frame(
                    black_box(address),
                    FrameLookupOptions::default(),
                    &mut scratch,
                    |frame| {
                        frames += 1;
                        black_box(frame);
                    },
                )
                .expect("frame visitation must not fail");
            black_box((found, frames));
        });
    });

    for (name, address) in [
        ("miss_before", BASE_ADDRESS - 1),
        ("miss_gap", gap(RICH_FUNCTIONS / 2)),
        (
            "miss_after",
            BASE_ADDRESS + RICH_FUNCTIONS as u64 * FUNCTION_STRIDE,
        ),
    ] {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                black_box(
                    rich.lookup(black_box(address))
                        .expect("miss lookup must not fail"),
                )
            });
        });
    }
    group.finish();
}

fn batches(criterion: &mut Criterion) {
    let bytes = encoded(GsymVersion::V1, Endian::Little, RICH_FUNCTIONS, Shape::Lean);
    let gsym = Gsym::parse(bytes.as_slice()).expect("batch fixture must parse");
    let workloads = [
        ("sorted_hits", sorted_hits(BATCH_QUERIES)),
        ("shuffled_hits", shuffled_hits(BATCH_QUERIES)),
        ("mixed_25_percent_miss", mixed_queries(BATCH_QUERIES)),
    ];
    let mut group = criterion.benchmark_group("reader/batch_lookup");
    group.throughput(Throughput::Elements(BATCH_QUERIES as u64));
    for (name, addresses) in workloads {
        group.bench_with_input(name, addresses.as_slice(), |bencher, addresses| {
            bencher.iter(|| {
                for &address in addresses {
                    black_box(
                        gsym.lookup(black_box(address))
                            .expect("batch lookup must not fail"),
                    );
                }
            });
        });
    }
    group.finish();
}

fn full_scans(criterion: &mut Criterion) {
    let bytes = encoded(GsymVersion::V2, Endian::Little, RICH_FUNCTIONS, Shape::Rich);
    let gsym = Gsym::parse(bytes.as_slice()).expect("scan fixture must parse");
    let mut group = criterion.benchmark_group("reader/full_file");
    group.throughput(Throughput::Elements(RICH_FUNCTIONS as u64));
    group.bench_function("function_iteration", |bencher| {
        bencher.iter(|| {
            for function in gsym.functions() {
                black_box(
                    function
                        .expect("fixture function must decode its header")
                        .start(),
                );
            }
        });
    });
    group.bench_function("verify", |bencher| {
        bencher.iter(|| black_box(gsym.verify().expect("fixture must verify")));
    });
    group.bench_function("decode_all", |bencher| {
        bencher.iter(|| black_box(gsym.decode_all().expect("fixture must fully decode")));
    });
    group.finish();
}

#[cfg(feature = "mmap")]
#[expect(
    unsafe_code,
    reason = "the benchmark maps a fixture file to measure the mapped reader path"
)]
fn file_open(criterion: &mut Criterion) {
    use gsym::MappedGsym;

    let bytes = encoded(
        GsymVersion::V1,
        Endian::Little,
        INDEXED_FUNCTIONS,
        Shape::Lean,
    );
    let directory = tempfile::tempdir().expect("benchmark temporary directory must be created");
    let path = directory.path().join("open.gsym");
    std::fs::write(&path, &bytes).expect("benchmark fixture must be written");

    // Prime the filesystem cache so this group measures reader construction,
    // not a machine-specific disk seek.
    black_box(Gsym::open(&path).expect("owned warm-up open must succeed"));
    // SAFETY: this benchmark owns the file and never modifies it while mapped.
    black_box(unsafe { MappedGsym::map(&path) }.expect("mapped warm-up open must succeed"));

    let mut group = criterion.benchmark_group("reader/open_warm_page_cache");
    group.throughput(Throughput::Bytes(bytes.len() as u64));
    group.bench_function("owned_snapshot", |bencher| {
        bencher.iter(|| black_box(Gsym::open(&path).expect("owned open must succeed")));
    });
    group.bench_function("memory_map", |bencher| {
        bencher.iter(|| {
            // SAFETY: the benchmark-owned file remains immutable for every map.
            black_box(unsafe { MappedGsym::map(&path) }.expect("mmap open must succeed"))
        });
    });
    group.finish();
}

#[cfg(not(feature = "mmap"))]
const fn file_open(_criterion: &mut Criterion) {}
