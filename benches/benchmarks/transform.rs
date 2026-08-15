use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput};
use gsym::{Endian, Gsym, GsymVersion, TranscodeOptions};

use super::fixture::{Shape, encoded};

const FUNCTION_COUNT: usize = 10_000;

pub(crate) fn benchmarks(criterion: &mut Criterion) {
    let v1_bytes = encoded(GsymVersion::V1, Endian::Little, FUNCTION_COUNT, Shape::Rich);
    let v2_bytes = encoded(GsymVersion::V2, Endian::Big, FUNCTION_COUNT, Shape::Rich);
    let v1 = Gsym::parse(v1_bytes.as_slice()).expect("v1 transform fixture must parse");
    let v2 = Gsym::parse(v2_bytes.as_slice()).expect("v2 transform fixture must parse");

    {
        let mut decode = criterion.benchmark_group("transform/decode");
        decode.throughput(Throughput::Elements(FUNCTION_COUNT as u64));
        decode.bench_function("v1_rich", |bencher| {
            bencher.iter(|| black_box(v1.decode_all().expect("v1 fixture must decode")));
        });
        decode.bench_function("v2_rich", |bencher| {
            bencher.iter(|| black_box(v2.decode_all().expect("v2 fixture must decode")));
        });
        decode.finish();
    }

    {
        let mut transcode = criterion.benchmark_group("transform/transcode");
        transcode.throughput(Throughput::Elements(FUNCTION_COUNT as u64));
        for (name, input, options) in [
            (
                "v1_little_to_v2_big",
                &v1,
                TranscodeOptions {
                    version: Some(GsymVersion::V2),
                    endian: Some(Endian::Big),
                },
            ),
            (
                "v2_big_to_v1_little",
                &v2,
                TranscodeOptions {
                    version: Some(GsymVersion::V1),
                    endian: Some(Endian::Little),
                },
            ),
            (
                "v1_normalize",
                &v1,
                TranscodeOptions {
                    version: None,
                    endian: None,
                },
            ),
        ] {
            transcode.bench_function(name, |bencher| {
                bencher.iter(|| {
                    black_box(
                        input
                            .transcode(black_box(options))
                            .expect("fixture transcode must succeed"),
                    )
                });
            });
        }
        transcode.finish();
    }

    let model = v1.decode_all().expect("segment fixture must decode");
    let options = TranscodeOptions::default();
    {
        let mut segment = criterion.benchmark_group("transform/segment");
        segment.throughput(Throughput::Elements(FUNCTION_COUNT as u64));
        for target in [256 * 1024, 1024 * 1024] {
            segment.bench_with_input(
                BenchmarkId::new("target_bytes", target),
                &target,
                |bencher, &target| {
                    bencher.iter(|| {
                        black_box(
                            model
                                .segments(black_box(target), options)
                                .expect("fixture segmentation must succeed"),
                        )
                    });
                },
            );
        }
        segment.finish();
    }
}
