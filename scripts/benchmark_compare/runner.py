from __future__ import annotations

import os
import random
import re
import shutil
import subprocess
import tempfile
import time
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence

from .model import ResourceSample, Sample


@dataclass(frozen=True)
class LookupFixture:
    functions: int
    gsym: Path
    addresses: tuple[int, ...]


def find_program(explicit: Path | None, base: str) -> Path:
    if explicit is not None:
        name = os.fspath(explicit.expanduser())
        discovered = shutil.which(name) if explicit.parent == Path(".") else None
        path = Path(discovered or name).resolve()
        if not path.is_file():
            raise SystemExit(f"{base} not found: {path}")
        return path
    plain = shutil.which(base)
    if plain:
        return Path(plain).resolve()
    candidates: list[tuple[int, Path]] = []
    for directory in (Path("/usr/bin"), Path("/usr/local/bin")):
        for path in directory.glob(f"{base}-*"):
            match = re.fullmatch(rf"{re.escape(base)}-(\d+)", path.name)
            if match and path.is_file():
                candidates.append((int(match.group(1)), path.resolve()))
    if not candidates:
        raise SystemExit(f"could not find {base}; pass --{base.replace('_', '-')}")
    return max(candidates)[1]


def command_output(command: Sequence[os.PathLike[str] | str]) -> str:
    result = subprocess.run(
        [os.fspath(part) for part in command],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return result.stdout.strip()


def run_checked(
    command: Sequence[os.PathLike[str] | str], input_bytes: bytes | None = None
) -> None:
    normalized = [os.fspath(part) for part in command]
    result = subprocess.run(
        normalized,
        input=input_bytes,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        stderr = result.stderr.decode("utf-8", "replace")
        raise SystemExit(f"command failed ({result.returncode}): {' '.join(normalized)}\n{stderr}")


def measure(
    command: Sequence[os.PathLike[str] | str], input_bytes: bytes | None = None
) -> int:
    start = time.perf_counter_ns()
    run_checked(command, input_bytes)
    return time.perf_counter_ns() - start


def measure_resources(
    command: Sequence[os.PathLike[str] | str], input_bytes: bytes | None = None
) -> tuple[int, int]:
    if not hasattr(os, "wait4"):
        raise SystemExit("resource measurement requires Linux wait4 support")
    normalized = [os.fspath(part) for part in command]
    with tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen(
            normalized,
            stdin=subprocess.PIPE if input_bytes is not None else subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=stderr,
        )
        if input_bytes is not None:
            if process.stdin is None:
                process.kill()
                raise SystemExit("failed to create benchmark input pipe")
            process.stdin.write(input_bytes)
            process.stdin.close()
        _, status, usage = os.wait4(process.pid, 0)
        process.returncode = os.waitstatus_to_exitcode(status)
        if process.returncode != 0:
            stderr.seek(0)
            message = stderr.read().decode("utf-8", "replace")
            raise SystemExit(
                f"command failed ({process.returncode}): {' '.join(normalized)}\n{message}"
            )
    cpu_ns = round((usage.ru_utime + usage.ru_stime) * 1_000_000_000)
    max_rss_bytes = usage.ru_maxrss * 1024
    return cpu_ns, max_rss_bytes


def pin_cpu(requested: int | None) -> int | None:
    if not hasattr(os, "sched_getaffinity") or not hasattr(os, "sched_setaffinity"):
        return None
    available = sorted(os.sched_getaffinity(0))
    if not available:
        return None
    cpu = available[0] if requested is None else requested
    if cpu not in available:
        raise SystemExit(f"CPU {cpu} is outside this process's affinity: {available}")
    os.sched_setaffinity(0, {cpu})
    return cpu


def compile_fixture(cc: Path, path: Path, functions: int) -> None:
    source = path.with_suffix(".c")
    lines = [
        "typedef unsigned (*function_pointer)(unsigned);",
        "static inline unsigned mix(unsigned value, unsigned salt) {",
        "  value ^= salt + 0x9e3779b9u;",
        "  value *= 0x85ebca6bu;",
        "  return value ^ (value >> 13);",
        "}",
    ]
    for index in range(functions):
        lines.extend(
            (
                f"__attribute__((noinline, used)) unsigned function_{index}(unsigned value) {{",
                f"  unsigned mixed = mix(value, {index}u);",
                f"  return (mixed & 1u) ? mixed + {index}u : mixed ^ {index}u;",
                "}",
            )
        )
    lines.append("static function_pointer const all_functions[] = {")
    lines.extend(f"  function_{index}," for index in range(functions))
    lines.extend(
        (
            "};",
            "int main(int argc, char **argv) {",
            "  unsigned value = (unsigned)argc + (unsigned)(argv != 0);",
            f"  for (unsigned i = 0; i < {functions}u; ++i)",
            "    value = all_functions[i](value);",
            "  return (int)(value & 0x7fu);",
            "}",
        )
    )
    source.write_text("\n".join(lines) + "\n", encoding="ascii")
    run_checked(
        (
            cc,
            "-g",
            "-gdwarf-5",
            "-O2",
            "-fno-omit-frame-pointer",
            "-fno-pie",
            "-no-pie",
            "-Wl,--build-id=none",
            "-o",
            path,
            source,
        )
    )


def function_addresses(elf: Path) -> list[int]:
    nm = shutil.which("nm")
    if not nm:
        raise SystemExit("nm is required to obtain deterministic lookup addresses")
    output = command_output((nm, "-n", "--defined-only", elf))
    addresses = []
    for line in output.splitlines():
        fields = line.split()
        if len(fields) >= 3 and fields[2].startswith("function_"):
            addresses.append(int(fields[0], 16) + 1)
    if not addresses:
        raise SystemExit(f"no benchmark functions found in {elf}")
    return addresses


def gsym_convert_command(tool: Path, elf: Path, output: Path) -> tuple[str, ...]:
    return (
        os.fspath(tool),
        "--color",
        "never",
        "--quiet",
        "convert",
        os.fspath(elf),
        "--output",
        os.fspath(output),
        "--no-discovery",
    )


def llvm_convert_command(tool: Path, elf: Path, output: Path) -> tuple[str, ...]:
    return (
        os.fspath(tool),
        "--convert",
        os.fspath(elf),
        "--out-file",
        os.fspath(output),
        "--quiet",
        "--num-threads=1",
    )


def benchmark_conversion(
    rng: random.Random,
    trials: int,
    warmups: int,
    resource_trials: int,
    function_counts: Sequence[int],
    gsymtool: Path,
    llvm: Path,
    cc: Path,
    work: Path,
) -> tuple[list[Sample], list[ResourceSample], list[LookupFixture]]:
    samples = []
    resources = []
    lookup_fixtures = []
    for functions in function_counts:
        elf = work / f"fixture-{functions}"
        compile_fixture(cc, elf, functions)
        addresses = function_addresses(elf)
        if len(addresses) != functions:
            raise SystemExit(
                f"expected {functions} benchmark symbols in {elf}, found {len(addresses)}"
            )
        outputs = {
            "gsym-rs": work / f"fixture-{functions}-rust.gsym",
            "LLVM": work / f"fixture-{functions}-llvm.gsym",
        }
        commands = {
            "gsym-rs": gsym_convert_command(gsymtool, elf, outputs["gsym-rs"]),
            "LLVM": llvm_convert_command(llvm, elf, outputs["LLVM"]),
        }
        run_trials(rng, warmups, commands, outputs)
        for trial in range(trials):
            order = list(commands)
            rng.shuffle(order)
            for tool in order:
                outputs[tool].unlink(missing_ok=True)
                elapsed = measure(commands[tool])
                samples.append(
                    Sample(
                        operation="convert",
                        scenario=str(functions),
                        tool=tool,
                        trial=trial,
                        elapsed_ns=elapsed,
                        units=functions,
                        input_bytes=elf.stat().st_size,
                        output_bytes=outputs[tool].stat().st_size,
                    )
                )
        verify_outputs(gsymtool, outputs.values())
        for trial in range(resource_trials):
            order = list(commands)
            rng.shuffle(order)
            for tool in order:
                outputs[tool].unlink(missing_ok=True)
                cpu_ns, max_rss_bytes = measure_resources(commands[tool])
                resources.append(
                    ResourceSample(
                        operation="convert",
                        scenario=str(functions),
                        tool=tool,
                        trial=trial,
                        cpu_ns=cpu_ns,
                        max_rss_bytes=max_rss_bytes,
                    )
                )
        lookup_fixtures.append(
            LookupFixture(functions, outputs["LLVM"], tuple(addresses))
        )
    if not lookup_fixtures:
        raise SystemExit("conversion benchmark produced no GSYM input for lookup")
    return samples, resources, lookup_fixtures


def run_trials(
    rng: random.Random,
    count: int,
    commands: dict[str, tuple[str, ...]],
    outputs: dict[str, Path],
) -> None:
    for _ in range(count):
        order = list(commands)
        rng.shuffle(order)
        for tool in order:
            outputs[tool].unlink(missing_ok=True)
            run_checked(commands[tool])


def verify_outputs(gsymtool: Path, outputs: Sequence[Path]) -> None:
    for output in outputs:
        run_checked((gsymtool, "--color", "never", "--quiet", "verify", output))


def benchmark_lookup(
    rng: random.Random,
    trials: int,
    warmups: int,
    resource_trials: int,
    batch_sizes: Sequence[int],
    gsymtool: Path,
    llvm: Path,
    gsym: Path,
    addresses: Sequence[int],
) -> tuple[list[Sample], list[ResourceSample]]:
    shuffled = list(addresses)
    rng.shuffle(shuffled)
    samples = []
    resources = []
    gsym.read_bytes()
    for batch_size in batch_sizes:
        if batch_size > len(shuffled):
            raise SystemExit(
                f"lookup batch {batch_size} exceeds available functions ({len(shuffled)})"
            )
        selected = shuffled[:batch_size]
        rust_command = (
            os.fspath(gsymtool),
            "--color",
            "never",
            "--quiet",
            "lookup",
            os.fspath(gsym),
            *(f"0x{address:x}" for address in selected),
        )
        llvm_command = (os.fspath(llvm), "--addresses-from-stdin")
        llvm_input = "".join(f"0x{address:x} {gsym}\n" for address in selected).encode()
        commands = {
            "gsym-rs": (rust_command, None),
            "LLVM": (llvm_command, llvm_input),
        }
        for _ in range(warmups):
            run_lookup_pair(rng, commands)
        for trial in range(trials):
            for tool, elapsed in run_lookup_pair(rng, commands, timed=True):
                samples.append(
                    Sample(
                        operation="lookup",
                        scenario=str(batch_size),
                        tool=tool,
                        trial=trial,
                        elapsed_ns=elapsed,
                        units=batch_size,
                        input_bytes=gsym.stat().st_size,
                        output_bytes=0,
                    )
                )
        for trial in range(resource_trials):
            for tool, command, input_bytes in ordered_lookup_commands(rng, commands):
                cpu_ns, max_rss_bytes = measure_resources(command, input_bytes)
                resources.append(
                    ResourceSample(
                        operation="lookup",
                        scenario=str(batch_size),
                        tool=tool,
                        trial=trial,
                        cpu_ns=cpu_ns,
                        max_rss_bytes=max_rss_bytes,
                    )
                )
    return samples, resources


def run_lookup_pair(
    rng: random.Random,
    commands: dict[str, tuple[tuple[str, ...], bytes | None]],
    timed: bool = False,
) -> list[tuple[str, int]]:
    results = []
    for tool, command, input_bytes in ordered_lookup_commands(rng, commands):
        elapsed = measure(command, input_bytes) if timed else 0
        if not timed:
            run_checked(command, input_bytes)
        results.append((tool, elapsed))
    return results


def ordered_lookup_commands(
    rng: random.Random,
    commands: dict[str, tuple[tuple[str, ...], bytes | None]],
) -> Iterator[tuple[str, tuple[str, ...], bytes | None]]:
    order = list(commands)
    rng.shuffle(order)
    for tool in order:
        command, input_bytes = commands[tool]
        yield tool, command, input_bytes
