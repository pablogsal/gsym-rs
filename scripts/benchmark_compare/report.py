from __future__ import annotations

import csv
import math
from collections import defaultdict
from pathlib import Path
from typing import Iterable, Sequence

from .model import ResourceSummary, Summary


COLORS = {
    "gsym-rs": "#1769aa",
    "LLVM": "#c43d3d",
    "gsym-rs speedup": "#1769aa",
}


def write_csv(path: Path, rows: Iterable[dict[str, object]]) -> None:
    materialized = list(rows)
    if not materialized:
        return
    with path.open("w", newline="", encoding="utf-8") as output:
        writer = csv.DictWriter(output, fieldnames=list(materialized[0]))
        writer.writeheader()
        writer.writerows(materialized)


def escape_svg(text: str) -> str:
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def scientific(value: float) -> str:
    if value >= 1_000_000:
        return f"{value / 1_000_000:.1f}M"
    if value >= 1_000:
        return f"{value / 1_000:.1f}k"
    if value >= 10:
        return f"{value:.0f}"
    return f"{value:.2f}"


def svg_plot(
    path: Path,
    title: str,
    subtitle: str,
    x_label: str,
    y_label: str,
    series: dict[str, list[tuple[float, float, float, float]]],
    x_log: bool,
    y_log: bool,
    reference_y: float | None = None,
) -> None:
    width, height = 1080, 680
    left, right, top, bottom = 115, 45, 100, 100
    plot_width = width - left - right
    plot_height = height - top - bottom
    points = [point for values in series.values() for point in values]
    x_values = [point[0] for point in points]
    y_values = [bound for point in points for bound in point[1:]]
    if reference_y is not None:
        y_values.append(reference_y)
    transform_x = math.log10 if x_log else lambda value: value
    transform_y = math.log10 if y_log else lambda value: value
    x_min, x_max = min(map(transform_x, x_values)), max(map(transform_x, x_values))
    y_min, y_max = min(map(transform_y, y_values)), max(map(transform_y, y_values))
    x_pad = max((x_max - x_min) * 0.08, 0.08)
    y_pad = max((y_max - y_min) * 0.12, 0.08)
    x_min, x_max = x_min - x_pad, x_max + x_pad
    y_min, y_max = y_min - y_pad, y_max + y_pad

    def x_pixel(value: float) -> float:
        return left + (transform_x(value) - x_min) * plot_width / (x_max - x_min)

    def y_pixel(value: float) -> float:
        return top + (y_max - transform_y(value)) * plot_height / (y_max - y_min)

    svg = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">',
        '<rect width="100%" height="100%" fill="#ffffff"/>',
        '<g font-family="DejaVu Sans,Arial,sans-serif" fill="#1b1b1b">',
        f'<text x="{left}" y="38" font-size="25" font-weight="600">{escape_svg(title)}</text>',
        f'<text x="{left}" y="66" font-size="14" fill="#555">{escape_svg(subtitle)}</text>',
    ]
    add_y_axis(svg, left, top, plot_width, plot_height, y_min, y_max, y_log)
    distinct_x = sorted(set(x_values))
    labeled_x = {
        value
        for index, value in enumerate(distinct_x)
        if len(distinct_x) <= 9 or index % 2 == 0 or index == len(distinct_x) - 1
    }
    for value in distinct_x:
        x = x_pixel(value)
        svg.append(
            f'<line x1="{x:.2f}" y1="{top}" x2="{x:.2f}" y2="{top + plot_height}" stroke="#eeeeee" stroke-width="1"/>'
        )
        if value in labeled_x:
            svg.append(
                f'<text x="{x:.2f}" y="{top + plot_height + 28}" text-anchor="middle" font-size="13">{scientific(value)}</text>'
            )
    svg.extend(
        (
            f'<line x1="{left}" y1="{top + plot_height}" x2="{left + plot_width}" y2="{top + plot_height}" stroke="#222"/>',
            f'<line x1="{left}" y1="{top}" x2="{left}" y2="{top + plot_height}" stroke="#222"/>',
            f'<text x="{left + plot_width / 2}" y="{height - 25}" text-anchor="middle" font-size="15">{escape_svg(x_label)}</text>',
            f'<text transform="translate(28 {top + plot_height / 2}) rotate(-90)" text-anchor="middle" font-size="15">{escape_svg(y_label)}</text>',
        )
    )
    if reference_y is not None and min(y_values) <= reference_y <= max(y_values):
        y = y_pixel(reference_y)
        svg.extend(
            (
                f'<line x1="{left}" y1="{y:.2f}" x2="{left + plot_width}" y2="{y:.2f}" stroke="#555" stroke-width="1.5" stroke-dasharray="7 6"/>',
                f'<text x="{left + 8}" y="{y - 7:.2f}" font-size="12" fill="#555">parity</text>',
            )
        )
    add_series(svg, series, x_pixel, y_pixel, left + plot_width - 180, top)
    svg.append("</g></svg>\n")
    path.write_text("\n".join(svg), encoding="utf-8")


def add_y_axis(
    svg: list[str],
    left: int,
    top: int,
    width: int,
    height: int,
    minimum: float,
    maximum: float,
    logarithmic: bool,
) -> None:
    for tick in range(6):
        fraction = tick / 5
        transformed = minimum + fraction * (maximum - minimum)
        value = 10**transformed if logarithmic else transformed
        y = top + (1.0 - fraction) * height
        svg.extend(
            (
                f'<line x1="{left}" y1="{y:.2f}" x2="{left + width}" y2="{y:.2f}" stroke="#dedede" stroke-width="1"/>',
                f'<text x="{left - 12}" y="{y + 5:.2f}" text-anchor="end" font-size="13" fill="#444">{scientific(value)}</text>',
            )
        )


def add_series(
    svg: list[str],
    series: dict[str, list[tuple[float, float, float, float]]],
    x_pixel,
    y_pixel,
    legend_x: float,
    top: float,
) -> None:
    for offset, (tool, values) in enumerate(series.items()):
        color = COLORS[tool]
        if len(values) > 1:
            upper = " ".join(f"{x_pixel(x):.2f},{y_pixel(high):.2f}" for x, _, _, high in values)
            lower = " ".join(
                f"{x_pixel(x):.2f},{y_pixel(low):.2f}" for x, _, low, _ in reversed(values)
            )
            svg.append(
                f'<polygon points="{upper} {lower}" fill="{color}" fill-opacity="0.13" stroke="none"/>'
            )
        path_points = " ".join(
            f"{'M' if index == 0 else 'L'} {x_pixel(point[0]):.2f} {y_pixel(point[1]):.2f}"
            for index, point in enumerate(values)
        )
        svg.append(f'<path d="{path_points}" fill="none" stroke="{color}" stroke-width="2.5"/>')
        for x_value, center, low, high in values:
            add_point(
                svg,
                color,
                tool,
                x_pixel(x_value),
                y_pixel(center),
                y_pixel(low),
                y_pixel(high),
            )
        legend_y = top + 14 + offset * 25
        svg.extend(
            (
                f'<line x1="{legend_x}" y1="{legend_y}" x2="{legend_x + 28}" y2="{legend_y}" stroke="{color}" stroke-width="3"/>',
                f'<circle cx="{legend_x + 14}" cy="{legend_y}" r="4" fill="{color}"/>',
                f'<text x="{legend_x + 38}" y="{legend_y + 5}" font-size="14">{escape_svg(tool)}</text>',
            )
        )


def add_point(
    svg: list[str],
    color: str,
    tool: str,
    x: float,
    center_y: float,
    low_y: float,
    high_y: float,
) -> None:
    svg.extend(
        (
            f'<line x1="{x:.2f}" y1="{high_y:.2f}" x2="{x:.2f}" y2="{low_y:.2f}" stroke="{color}" stroke-width="2"/>',
            f'<line x1="{x - 6:.2f}" y1="{high_y:.2f}" x2="{x + 6:.2f}" y2="{high_y:.2f}" stroke="{color}" stroke-width="2"/>',
            f'<line x1="{x - 6:.2f}" y1="{low_y:.2f}" x2="{x + 6:.2f}" y2="{low_y:.2f}" stroke="{color}" stroke-width="2"/>',
        )
    )
    if tool == "LLVM":
        svg.append(
            f'<rect x="{x - 4.5:.2f}" y="{center_y - 4.5:.2f}" width="9" height="9" fill="{color}" stroke="#fff" stroke-width="1.5"/>'
        )
    else:
        svg.append(
            f'<circle cx="{x:.2f}" cy="{center_y:.2f}" r="5" fill="{color}" stroke="#fff" stroke-width="1.5"/>'
        )


def write_plots(
    output: Path,
    summaries: Sequence[Summary],
    speedups: Sequence[dict[str, float | int | str]],
    resources: Sequence[ResourceSummary],
    trials: int,
) -> None:
    conversion: dict[str, list[tuple[float, float, float, float]]] = defaultdict(list)
    conversion_throughput: dict[str, list[tuple[float, float, float, float]]] = defaultdict(list)
    lookup: dict[str, list[tuple[float, float, float, float]]] = defaultdict(list)
    hot_lookup: dict[str, list[tuple[float, float, float, float]]] = defaultdict(list)
    output_size: dict[str, list[tuple[float, float, float, float]]] = defaultdict(list)
    bytes_per_function: dict[str, list[tuple[float, float, float, float]]] = defaultdict(list)
    for summary in summaries:
        x = float(summary.scenario)
        if summary.operation == "convert":
            conversion[summary.tool].append(
                (
                    x,
                    summary.median_ns / 1_000_000,
                    summary.ci_low_ns / 1_000_000,
                    summary.ci_high_ns / 1_000_000,
                )
            )
            scale = summary.units * 1_000_000_000.0
            conversion_throughput[summary.tool].append(
                (
                    x,
                    scale / summary.median_ns,
                    scale / summary.ci_high_ns,
                    scale / summary.ci_low_ns,
                )
            )
            size_kib = summary.output_bytes / 1024
            output_size[summary.tool].append((x, size_kib, size_kib, size_kib))
            amortized = summary.output_bytes / summary.units
            bytes_per_function[summary.tool].append((x, amortized, amortized, amortized))
        elif summary.operation == "lookup":
            scale = summary.units * 1_000_000_000.0
            lookup[summary.tool].append(
                (
                    x,
                    scale / summary.median_ns,
                    scale / summary.ci_high_ns,
                    scale / summary.ci_low_ns,
                )
            )
        elif summary.operation == "hot_lookup":
            scale = summary.units * 1_000_000_000.0
            hot_lookup[summary.tool].append(
                (
                    x,
                    scale / summary.median_ns,
                    scale / summary.ci_high_ns,
                    scale / summary.ci_low_ns,
                )
            )
    for values in (
        *conversion.values(),
        *conversion_throughput.values(),
        *lookup.values(),
        *hot_lookup.values(),
        *output_size.values(),
        *bytes_per_function.values(),
    ):
        values.sort()
    subtitle = f"Median of {trials} trials; error bars are percentile 95% bootstrap CI"
    svg_plot(
        output / "conversion-time.svg",
        "ELF and DWARF to GSYM conversion",
        subtitle + "; LLVM fixed at one thread",
        "Generated source functions (log scale)",
        "Wall time, ms (log scale; lower is better)",
        conversion,
        x_log=True,
        y_log=True,
    )
    hot_trials = next(iter(hot_lookup.values()), [(0, 0, 0, 0)])
    hot_sample_count = next(
        (summary.samples for summary in summaries if summary.operation == "hot_lookup"),
        0,
    )
    if hot_trials and hot_sample_count:
        svg_plot(
            output / "hot-lookup-throughput.svg",
            "Initialized-reader lookup throughput",
            f"Median of {hot_sample_count} internally timed trials; 95% bootstrap CI; startup and parsing excluded",
            "Indexed functions in GSYM (log scale)",
            "Lookups per second (log scale; higher is better)",
            hot_lookup,
            x_log=True,
            y_log=True,
        )
    svg_plot(
        output / "conversion-throughput.svg",
        "GSYM conversion throughput",
        subtitle + "; LLVM fixed at one thread",
        "Generated source functions (log scale)",
        "Functions per second (log scale; higher is better)",
        conversion_throughput,
        x_log=True,
        y_log=True,
    )
    svg_plot(
        output / "lookup-throughput.svg",
        "End-to-end GSYM batch lookup",
        subtitle + "; same LLVM-generated GSYM; process startup included",
        "Addresses per invocation (log scale)",
        "Addresses per second (log scale; higher is better)",
        lookup,
        x_log=True,
        y_log=True,
    )
    svg_plot(
        output / "bytes-per-function.svg",
        "Amortized GSYM storage cost",
        "Exact verified output size divided by generated source-function count",
        "Generated source functions (log scale)",
        "GSYM bytes per source function (lower is better)",
        bytes_per_function,
        x_log=True,
        y_log=False,
    )
    svg_plot(
        output / "output-size.svg",
        "Verified GSYM output size",
        "Exact encoded size; both tools consume the same ELF and DWARF input",
        "Generated source functions (log scale)",
        "GSYM size, KiB (log scale; lower is better)",
        output_size,
        x_log=True,
        y_log=True,
    )
    write_speedup_plots(output, speedups, trials)
    write_resource_plots(output, resources)


def write_speedup_plots(
    output: Path,
    speedups: Sequence[dict[str, float | int | str]],
    trials: int,
) -> None:
    for operation, noun in (
        ("convert", "source functions"),
        ("lookup", "addresses per invocation"),
        ("hot_lookup", "indexed functions"),
    ):
        values = []
        operation_trials = trials
        for row in speedups:
            if row["operation"] != operation:
                continue
            operation_trials = int(row["samples"])
            values.append(
                (
                    float(row["scenario"]),
                    float(row["median_speedup"]),
                    float(row["ci_low"]),
                    float(row["ci_high"]),
                )
            )
        values.sort()
        svg_plot(
            output / f"{operation.replace('_', '-')}-speedup.svg",
            f"gsym-rs {operation.replace('_', ' ')} speedup relative to LLVM",
            f"Paired median of {operation_trials} randomized trials; 95% bootstrap CI; values above 1 favor gsym-rs",
            f"{noun.capitalize()} (log scale)",
            "LLVM time / gsym-rs time",
            {"gsym-rs speedup": values},
            x_log=True,
            y_log=False,
            reference_y=1.0,
        )


def write_resource_plots(output: Path, resources: Sequence[ResourceSummary]) -> None:
    for operation, noun in (("convert", "source functions"), ("lookup", "addresses per invocation")):
        cpu: dict[str, list[tuple[float, float, float, float]]] = defaultdict(list)
        rss: dict[str, list[tuple[float, float, float, float]]] = defaultdict(list)
        for summary in resources:
            if summary.operation != operation:
                continue
            x = float(summary.scenario)
            cpu[summary.tool].append(
                (
                    x,
                    summary.median_cpu_ns / 1_000_000,
                    summary.cpu_ci_low_ns / 1_000_000,
                    summary.cpu_ci_high_ns / 1_000_000,
                )
            )
            rss[summary.tool].append(
                (
                    x,
                    summary.median_max_rss_bytes / (1024 * 1024),
                    summary.rss_ci_low_bytes / (1024 * 1024),
                    summary.rss_ci_high_bytes / (1024 * 1024),
                )
            )
        for values in (*cpu.values(), *rss.values()):
            values.sort()
        svg_plot(
            output / f"{operation}-cpu-time.svg",
            f"{operation.capitalize()} CPU time",
            "Median child-process user plus system CPU time; 95% bootstrap CI",
            f"{noun.capitalize()} (log scale)",
            "CPU time, ms (log scale; lower is better)",
            cpu,
            x_log=True,
            y_log=True,
        )
        svg_plot(
            output / f"{operation}-peak-rss.svg",
            f"{operation.capitalize()} peak resident memory",
            "Median Linux ru_maxrss over independent child processes; 95% bootstrap CI",
            f"{noun.capitalize()} (log scale)",
            "Peak resident memory, MiB (lower is better)",
            rss,
            x_log=True,
            y_log=False,
        )
