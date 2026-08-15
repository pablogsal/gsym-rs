from __future__ import annotations

import hashlib
import math
import random
import statistics
from collections import defaultdict
from dataclasses import dataclass
from typing import Sequence


@dataclass(frozen=True)
class Sample:
    operation: str
    scenario: str
    tool: str
    trial: int
    elapsed_ns: int
    units: int
    input_bytes: int
    output_bytes: int


@dataclass(frozen=True)
class Summary:
    operation: str
    scenario: str
    tool: str
    samples: int
    units: int
    input_bytes: int
    output_bytes: int
    median_ns: float
    ci_low_ns: float
    ci_high_ns: float
    mean_ns: float
    stdev_ns: float

    @property
    def throughput(self) -> float:
        return self.units * 1_000_000_000.0 / self.median_ns


@dataclass(frozen=True)
class ResourceSample:
    operation: str
    scenario: str
    tool: str
    trial: int
    cpu_ns: int
    max_rss_bytes: int


@dataclass(frozen=True)
class ResourceSummary:
    operation: str
    scenario: str
    tool: str
    samples: int
    median_cpu_ns: float
    cpu_ci_low_ns: float
    cpu_ci_high_ns: float
    median_max_rss_bytes: float
    rss_ci_low_bytes: float
    rss_ci_high_bytes: float


def percentile(sorted_values: Sequence[float], probability: float) -> float:
    position = (len(sorted_values) - 1) * probability
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return sorted_values[lower]
    fraction = position - lower
    return sorted_values[lower] * (1.0 - fraction) + sorted_values[upper] * fraction


def bootstrap_median(
    values: Sequence[int], iterations: int, seed_material: str
) -> tuple[float, float]:
    seed = int.from_bytes(hashlib.sha256(seed_material.encode()).digest()[:8], "big")
    rng = random.Random(seed)
    medians = []
    for _ in range(iterations):
        sample = [values[rng.randrange(len(values))] for _ in values]
        medians.append(statistics.median(sample))
    medians.sort()
    return percentile(medians, 0.025), percentile(medians, 0.975)


def summarize(samples: Sequence[Sample], bootstrap: int) -> list[Summary]:
    grouped: dict[tuple[str, str, str], list[Sample]] = defaultdict(list)
    for sample in samples:
        grouped[(sample.operation, sample.scenario, sample.tool)].append(sample)
    summaries = []
    for (operation, scenario, tool), group in sorted(grouped.items()):
        values = [sample.elapsed_ns for sample in group]
        low, high = bootstrap_median(values, bootstrap, f"{operation}/{scenario}/{tool}")
        summaries.append(
            Summary(
                operation=operation,
                scenario=scenario,
                tool=tool,
                samples=len(values),
                units=group[0].units,
                input_bytes=group[0].input_bytes,
                output_bytes=group[0].output_bytes,
                median_ns=statistics.median(values),
                ci_low_ns=low,
                ci_high_ns=high,
                mean_ns=statistics.mean(values),
                stdev_ns=statistics.stdev(values) if len(values) > 1 else 0.0,
            )
        )
    return summaries


def paired_speedups(
    samples: Sequence[Sample], bootstrap: int
) -> list[dict[str, float | int | str]]:
    grouped: dict[tuple[str, str, int], dict[str, Sample]] = defaultdict(dict)
    for sample in samples:
        grouped[(sample.operation, sample.scenario, sample.trial)][sample.tool] = sample
    ratios: dict[tuple[str, str], list[float]] = defaultdict(list)
    for (operation, scenario, _), pair in grouped.items():
        if pair.keys() >= {"gsym-rs", "LLVM"}:
            ratios[(operation, scenario)].append(
                pair["LLVM"].elapsed_ns / pair["gsym-rs"].elapsed_ns
            )
    rows = []
    for (operation, scenario), values in sorted(ratios.items()):
        scaled = [round(value * 1_000_000_000) for value in values]
        low, high = bootstrap_median(scaled, bootstrap, f"ratio/{operation}/{scenario}")
        rows.append(
            {
                "operation": operation,
                "scenario": scenario,
                "samples": len(values),
                "median_speedup": statistics.median(values),
                "ci_low": low / 1_000_000_000,
                "ci_high": high / 1_000_000_000,
            }
        )
    return rows


def summarize_resources(
    samples: Sequence[ResourceSample], bootstrap: int
) -> list[ResourceSummary]:
    grouped: dict[tuple[str, str, str], list[ResourceSample]] = defaultdict(list)
    for sample in samples:
        grouped[(sample.operation, sample.scenario, sample.tool)].append(sample)
    summaries = []
    for (operation, scenario, tool), group in sorted(grouped.items()):
        cpu = [sample.cpu_ns for sample in group]
        rss = [sample.max_rss_bytes for sample in group]
        cpu_low, cpu_high = bootstrap_median(
            cpu, bootstrap, f"resource/cpu/{operation}/{scenario}/{tool}"
        )
        rss_low, rss_high = bootstrap_median(
            rss, bootstrap, f"resource/rss/{operation}/{scenario}/{tool}"
        )
        summaries.append(
            ResourceSummary(
                operation=operation,
                scenario=scenario,
                tool=tool,
                samples=len(group),
                median_cpu_ns=statistics.median(cpu),
                cpu_ci_low_ns=cpu_low,
                cpu_ci_high_ns=cpu_high,
                median_max_rss_bytes=statistics.median(rss),
                rss_ci_low_bytes=rss_low,
                rss_ci_high_bytes=rss_high,
            )
        )
    return summaries
