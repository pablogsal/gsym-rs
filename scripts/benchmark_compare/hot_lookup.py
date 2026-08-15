from __future__ import annotations

import os
import random
import shlex
import shutil
from pathlib import Path
from typing import Sequence

from .model import Sample
from .runner import LookupFixture, command_output, run_checked


RUST_SOURCE = r'''
use std::hint::black_box;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let gsym_path = args.next().ok_or("missing GSYM path")?;
    let addresses_path = args.next().ok_or("missing address path")?;
    let repetitions: usize = args
        .next()
        .ok_or("missing repetition count")?
        .to_string_lossy()
        .parse()?;
    let addresses = std::fs::read_to_string(addresses_path)?
        .lines()
        .map(|line| u64::from_str_radix(line, 16))
        .collect::<Result<Vec<_>, _>>()?;
    let reader = gsym::Gsym::open(gsym_path)?;

    let mut checksum = 0_u64;
    for _ in 0..2 {
        checksum = run(&reader, &addresses, 1, checksum)?;
    }
    let start = Instant::now();
    checksum = run(&reader, &addresses, repetitions, checksum)?;
    let elapsed = start.elapsed().as_nanos();
    println!("{elapsed} {} {checksum}", addresses.len() * repetitions);
    Ok(())
}

fn run<D: AsRef<[u8]>>(
    reader: &gsym::Gsym<D>,
    addresses: &[u64],
    repetitions: usize,
    mut checksum: u64,
) -> Result<u64, gsym::Error> {
    for _ in 0..repetitions {
        for &address in addresses {
            let Some(result) = reader.lookup(black_box(address))? else {
                return Err(gsym::Error::InvalidFormat("benchmark address did not resolve"));
            };
            checksum = checksum
                .wrapping_add(result.function.start)
                .wrapping_add(result.frames.len() as u64)
                .wrapping_add(result.frames[0].name.len() as u64);
            black_box(&result);
        }
    }
    Ok(checksum)
}
'''


CPP_SOURCE = r'''
#include "llvm/DebugInfo/GSYM/GsymReader.h"
#include "llvm/Support/Error.h"
#include <chrono>
#include <cstdint>
#include <fstream>
#include <iostream>
#include <string>
#include <vector>

using llvm::gsym::GsymReader;

static bool run(const GsymReader &reader, const std::vector<uint64_t> &addresses,
                size_t repetitions, uint64_t &checksum) {
  for (size_t repetition = 0; repetition < repetitions; ++repetition) {
    for (uint64_t address : addresses) {
      auto result = reader.lookup(address);
      if (!result) {
        llvm::errs() << llvm::toString(result.takeError()) << '\n';
        return false;
      }
      checksum += result->FuncRange.start() + result->Locations.size() +
                  result->FuncName.size();
      asm volatile("" : "+r"(checksum) : : "memory");
    }
  }
  return true;
}

int main(int argc, char **argv) {
  if (argc != 4) {
    std::cerr << "usage: llvm-hot-lookup GSYM ADDRESSES REPETITIONS\n";
    return 2;
  }
  auto reader_or = GsymReader::openFile(argv[1]);
  if (!reader_or) {
    llvm::errs() << llvm::toString(reader_or.takeError()) << '\n';
    return 1;
  }
  std::ifstream input(argv[2]);
  std::vector<uint64_t> addresses;
  std::string line;
  while (std::getline(input, line))
    addresses.push_back(std::stoull(line, nullptr, 16));
  const size_t repetitions = std::stoull(argv[3]);
  uint64_t checksum = 0;
  if (!run(*reader_or, addresses, 2, checksum))
    return 1;
  const auto start = std::chrono::steady_clock::now();
  if (!run(*reader_or, addresses, repetitions, checksum))
    return 1;
  const auto elapsed = std::chrono::duration_cast<std::chrono::nanoseconds>(
                           std::chrono::steady_clock::now() - start)
                           .count();
  std::cout << elapsed << ' ' << addresses.size() * repetitions << ' '
            << checksum << '\n';
  return 0;
}
'''


def build_harnesses(root: Path, work: Path) -> dict[str, Path]:
    rust_dir = work / "rust-hot-lookup"
    (rust_dir / "src").mkdir(parents=True)
    (rust_dir / "Cargo.toml").write_text(
        "\n".join(
            (
                "[package]",
                'name = "gsym-hot-lookup"',
                'version = "0.0.0"',
                'edition = "2024"',
                "",
                "[dependencies]",
                f'gsym-rs = {{ path = "{root}", default-features = false }}',
                "",
                "[profile.release]",
                "codegen-units = 1",
                'lto = "thin"',
            )
        )
        + "\n",
        encoding="utf-8",
    )
    (rust_dir / "src/main.rs").write_text(RUST_SOURCE, encoding="utf-8")
    run_checked(("cargo", "build", "--release", "--manifest-path", rust_dir / "Cargo.toml"))
    rust_binary = rust_dir / "target/release/gsym-hot-lookup"

    llvm_config = shutil.which("llvm-config-21") or shutil.which("llvm-config")
    cxx = shutil.which("clang++-21") or shutil.which("clang++") or shutil.which("c++")
    if not llvm_config or not cxx:
        raise SystemExit("llvm-config and a C++ compiler are required for hot lookup comparison")
    cpp_source = work / "llvm-hot-lookup.cpp"
    cpp_binary = work / "llvm-hot-lookup"
    cpp_source.write_text(CPP_SOURCE, encoding="utf-8")
    flags = shlex.split(
        command_output(
            (
                llvm_config,
                "--cxxflags",
                "--ldflags",
                "--libs",
                "debuginfogsym",
                "--system-libs",
            )
        )
    )
    run_checked((cxx, "-O3", "-DNDEBUG", cpp_source, "-o", cpp_binary, *flags))
    return {"gsym-rs": rust_binary, "LLVM": cpp_binary}


def benchmark_hot_lookup(
    rng: random.Random,
    trials: int,
    warmups: int,
    target_lookups: int,
    harnesses: dict[str, Path],
    fixtures: Sequence[LookupFixture],
    work: Path,
) -> list[Sample]:
    samples = []
    for fixture in fixtures:
        address_file = work / f"addresses-{fixture.functions}.txt"
        address_file.write_text(
            "".join(f"{address:x}\n" for address in fixture.addresses), encoding="ascii"
        )
        repetitions = max(1, target_lookups // len(fixture.addresses))
        commands = {
            tool: (
                binary,
                fixture.gsym,
                address_file,
                str(repetitions),
            )
            for tool, binary in harnesses.items()
        }
        for _ in range(warmups):
            run_pair(rng, commands)
        for trial in range(trials):
            for tool, elapsed_ns, lookups in run_pair(rng, commands):
                samples.append(
                    Sample(
                        operation="hot_lookup",
                        scenario=str(fixture.functions),
                        tool=tool,
                        trial=trial,
                        elapsed_ns=elapsed_ns,
                        units=lookups,
                        input_bytes=fixture.gsym.stat().st_size,
                        output_bytes=0,
                    )
                )
    return samples


def run_pair(
    rng: random.Random, commands: dict[str, tuple[os.PathLike[str] | str, ...]]
) -> list[tuple[str, int, int]]:
    order = list(commands)
    rng.shuffle(order)
    results = []
    for tool in order:
        fields = command_output(commands[tool]).split()
        if len(fields) != 3:
            raise SystemExit(f"invalid {tool} hot-lookup result: {' '.join(fields)}")
        results.append((tool, int(fields[0]), int(fields[1])))
    return results
