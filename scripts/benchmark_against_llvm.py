#!/usr/bin/env python3
"""Reproducible end-to-end comparison against llvm-gsymutil."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import random
import tempfile
from dataclasses import asdict
from pathlib import Path

from benchmark_compare.model import paired_speedups, summarize, summarize_resources
from benchmark_compare.hot_lookup import benchmark_hot_lookup, build_harnesses
from benchmark_compare.report import write_csv, write_plots
from benchmark_compare.runner import (
    benchmark_conversion,
    benchmark_lookup,
    command_output,
    find_program,
    pin_cpu,
    run_checked,
)


ROOT = Path(__file__).resolve().parents[1]


def positive_int(text: str) -> int:
    value = int(text)
    if value <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return value


def nonnegative_int(text: str) -> int:
    value = int(text)
    if value < 0:
        raise argparse.ArgumentTypeError("must not be negative")
    return value


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Compare gsymtool with llvm-gsymutil and generate SVG plots."
    )
    parser.add_argument("--trials", type=positive_int, default=30)
    parser.add_argument("--warmups", type=nonnegative_int, default=5)
    parser.add_argument("--resource-trials", type=positive_int, default=10)
    parser.add_argument("--hot-trials", type=positive_int, default=15)
    parser.add_argument("--hot-warmups", type=nonnegative_int, default=2)
    parser.add_argument("--hot-lookups", type=positive_int, default=200_000)
    parser.add_argument("--bootstrap", type=positive_int, default=20_000)
    parser.add_argument("--seed", type=int, default=0x4753594D)
    parser.add_argument(
        "--function-counts", default="64,128,256,512,1024,2048,4096,8192,16384"
    )
    parser.add_argument(
        "--lookup-batches", default="1,2,4,8,16,32,64,128,256,512,1024,2048,4096,8192,16384"
    )
    parser.add_argument("--cpu", type=nonnegative_int)
    parser.add_argument("--cc", type=Path)
    parser.add_argument("--llvm-gsymutil", type=Path)
    parser.add_argument(
        "--gsymtool", type=Path, default=ROOT / "target/release/gsymtool"
    )
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def integer_list(text: str, name: str) -> list[int]:
    try:
        values = sorted({positive_int(value.strip()) for value in text.split(",")})
    except (ValueError, argparse.ArgumentTypeError) as error:
        raise SystemExit(f"invalid {name}: {error}") from error
    if not values:
        raise SystemExit(f"{name} must contain at least one integer")
    return values


def cpu_metadata(cpu: int | None) -> dict[str, object]:
    model = "unknown"
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.exists():
        for line in cpuinfo.read_text(errors="replace").splitlines():
            if line.startswith("model name"):
                model = line.partition(":")[2].strip()
                break
    governor = None
    if cpu is not None:
        path = Path(f"/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor")
        if path.exists():
            governor = path.read_text().strip()
    return {
        "model": model,
        "logical_cpus": os.cpu_count(),
        "pinned_cpu": cpu,
        "scaling_governor": governor,
    }


def make_environment(
    args: argparse.Namespace,
    timestamp: str,
    counts: list[int],
    batches: list[int],
    cpu: int | None,
    gsymtool: Path,
    llvm: Path,
    cc: Path,
) -> dict[str, object]:
    return {
        "timestamp_utc": timestamp,
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cpu": cpu_metadata(cpu),
        "load_average_before": os.getloadavg() if hasattr(os, "getloadavg") else None,
        "trials": args.trials,
        "warmups": args.warmups,
        "resource_trials": args.resource_trials,
        "hot_trials": args.hot_trials,
        "hot_warmups": args.hot_warmups,
        "hot_lookups_per_trial": args.hot_lookups,
        "bootstrap_resamples": args.bootstrap,
        "random_seed": args.seed,
        "function_counts": counts,
        "lookup_batches": batches,
        "estimator": "median",
        "confidence_interval": "95% percentile bootstrap of the median",
        "gsymtool": command_output((gsymtool, "--version")),
        "llvm_gsymutil": command_output((llvm, "--version")),
        "compiler": command_output((cc, "--version")).splitlines()[0],
        "rustc": command_output(("rustc", "--version", "--verbose")),
        "conversion_policy": "fresh output, discovery disabled, call sites disabled, LLVM one thread",
        "lookup_policy": "same LLVM-generated GSYM, warm page cache, stdout discarded",
        "hot_lookup_policy": "internally timed initialized readers; parsing, startup, and address loading excluded",
        "resource_policy": "independent Linux wait4 child rusage; CPU=user+system; RSS=ru_maxrss",
    }


def main() -> None:
    args = arguments()
    os.environ["LC_ALL"] = "C"
    os.environ["NO_COLOR"] = "1"
    counts = integer_list(args.function_counts, "--function-counts")
    batches = integer_list(args.lookup_batches, "--lookup-batches")
    llvm = find_program(args.llvm_gsymutil, "llvm-gsymutil")
    cc = find_program(args.cc, "cc")
    gsymtool = args.gsymtool.expanduser().resolve()
    if not args.no_build:
        run_checked(("cargo", "build", "--release", "--package", "gsymtool"))
    if not gsymtool.is_file():
        raise SystemExit(f"gsymtool not found: {gsymtool}")

    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = (args.output or ROOT / "benchmark-results" / timestamp).resolve()
    try:
        output.mkdir(parents=True, exist_ok=False)
    except FileExistsError as error:
        raise SystemExit(f"output directory already exists: {output}") from error
    cpu = pin_cpu(args.cpu)
    rng = random.Random(args.seed)
    environment = make_environment(args, timestamp, counts, batches, cpu, gsymtool, llvm, cc)
    print(f"output: {output}")
    print(f"cpu: {cpu_metadata(cpu)['model']} (pinned CPU {cpu})")
    print(f"llvm: {llvm}")
    print(f"trials: {args.trials} measured, {args.warmups} warmup")

    with tempfile.TemporaryDirectory(prefix="gsym-comparison-") as temporary:
        work = Path(temporary)
        conversion, conversion_resources, lookup_fixtures = benchmark_conversion(
            rng,
            args.trials,
            args.warmups,
            args.resource_trials,
            counts,
            gsymtool,
            llvm,
            cc,
            work,
        )
        largest = lookup_fixtures[-1]
        lookup, lookup_resources = benchmark_lookup(
            rng,
            args.trials,
            args.warmups,
            args.resource_trials,
            batches,
            gsymtool,
            llvm,
            largest.gsym,
            largest.addresses,
        )
        harnesses = build_harnesses(ROOT, work)
        hot_lookup = benchmark_hot_lookup(
            rng,
            args.hot_trials,
            args.hot_warmups,
            args.hot_lookups,
            harnesses,
            lookup_fixtures,
            work,
        )
    samples = conversion + lookup + hot_lookup
    resources = conversion_resources + lookup_resources
    summaries = summarize(samples, args.bootstrap)
    resource_summaries = summarize_resources(resources, args.bootstrap)
    speedups = paired_speedups(samples, args.bootstrap)
    environment["load_average_after"] = (
        os.getloadavg() if hasattr(os, "getloadavg") else None
    )

    write_csv(output / "samples.csv", (asdict(sample) for sample in samples))
    write_csv(output / "summary.csv", (asdict(summary) for summary in summaries))
    write_csv(output / "resources.csv", (asdict(sample) for sample in resources))
    write_csv(
        output / "resource-summary.csv",
        (asdict(summary) for summary in resource_summaries),
    )
    write_csv(output / "paired-speedups.csv", speedups)
    (output / "environment.json").write_text(
        json.dumps(environment, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    write_plots(output, summaries, speedups, resource_summaries, args.trials)

    print("\npaired median speedup (LLVM time / gsym-rs time):")
    for row in speedups:
        print(
            f"  {row['operation']:7} {row['scenario']:>5}: "
            f"{row['median_speedup']:.3f}x "
            f"(95% CI {row['ci_low']:.3f}x to {row['ci_high']:.3f}x)"
        )
    plot_count = sum(1 for _ in output.glob("*.svg"))
    print(f"\nplots: {output} ({plot_count} SVG figures)")


if __name__ == "__main__":
    main()
