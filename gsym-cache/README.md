<div align="center">
  <img src="https://raw.githubusercontent.com/pablogsal/gsym-rs/main/assets/mascot-compact.png" alt="gsym-rs detective crab mascot" width="325"><br>
  Concurrent, process-safe storage for immutable LLVM GSYM files.<br>
  Lock-free reads, verified atomic publication, and bounded maintenance.<br><br>
  <a href="https://github.com/pablogsal/gsym-rs/actions/workflows/ci.yml"><img src="https://github.com/pablogsal/gsym-rs/actions/workflows/ci.yml/badge.svg?branch=main" alt="Checks"></a>
  <a href="https://crates.io/crates/gsym-cache"><img src="https://img.shields.io/crates/v/gsym-cache.svg" alt="crates.io"></a>
  <a href="https://docs.rs/gsym-cache"><img src="https://docs.rs/gsym-cache/badge.svg" alt="docs.rs"></a>
</div>

## Install

Read-only lookup is the default:

```toml
[dependencies]
gsym-cache = "0.1"
```

Enable process-safe population and maintenance where GSYM files are produced:

```toml
[dependencies]
gsym-cache = { version = "0.1", features = ["manage"] }
```

The crate supports Linux. It stores GSYM but deliberately does not convert
binaries, download debug information, start workers, or choose resource limits.

## Look up

Opening a missing cache is valid and does not create it. A lookup takes no lock
and performs no write:

```rust,no_run
use gsym_cache::{BuildId, Cache, CacheEpoch};

let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let build_id: BuildId = "1212121212121212121212121212121212121212".parse()?;

if let Some(entry) = cache.lookup(&build_id)? {
    println!("cached GSYM: {} bytes", entry.len());
    consume(entry.into_file());
}
# fn consume(_: std::fs::File) {}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Lookup validates filesystem ownership and file type, but deliberately does not
decode the complete GSYM file on the profiler-facing hot path.

## Populate

With `manage`, population is an explicit nonblocking state machine. Acquiring
population grants permission to stream, verify, and publish one result; it does
not run a converter:

```rust,no_run
# #[cfg(feature = "manage")]
# fn run() -> Result<(), Box<dyn std::error::Error>> {
use std::fs::File;
use std::io;
use gsym_cache::{BuildId, Cache, CacheEpoch, PopulationOutcome};

let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let build_id: BuildId = "1212121212121212121212121212121212121212".parse()?;

match cache.try_begin_population(&build_id)? {
    PopulationOutcome::Present(entry) => drop(entry),
    PopulationOutcome::Acquired(population) => {
        let mut source = File::open("artifact.gsym")?;
        let mut writer = population.into_writer()?;
        io::copy(&mut source, &mut writer)?;
        drop(writer.publish()?.into_entry());
    }
    PopulationOutcome::Suppressed(failure) => {
        eprintln!("retry after {:?}", failure.expires_at());
    }
    PopulationOutcome::Busy => eprintln!("another process owns population"),
}
# Ok(())
# }
```

`Busy` is a retryable outcome and never waits. `Suppressed` is an unexpired
cached failure. Dropping `Population` or `PopulationWriter` abandons unpublished
work and releases the process lock.

## Maintain

Capacity pruning uses high-watermark triggering and continues to 80% of the
configured limit. Scrubbing fully verifies objects and reclaims confirmed
corruption, stale staging files, expired negative records, and orphan access
markers:

```rust,no_run
# #[cfg(feature = "manage")]
# fn run() -> Result<(), Box<dyn std::error::Error>> {
use std::time::Duration;
use gsym_cache::{ByteLimit, Cache, CacheEpoch, EntryLimit, PrunePolicy};

let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let policy = PrunePolicy::new(ByteLimit::try_from(10 * 1024 * 1024 * 1024_u64)?)
    .max_entries(EntryLimit::try_from(100_000_u64)?)
    .max_unused_age(Duration::from_secs(30 * 24 * 60 * 60));

if let Some(report) = cache.prune(policy)?.into_report() {
    println!("removed {}; {} bytes remain", report.removed, report.after.bytes);
}
if let Some(report) = cache.scrub()?.into_report() {
    println!("removed {} corrupt objects", report.removed_corrupt);
}
# Ok(())
# }
```

## Deploy safely

Use one private root per trust domain. Build IDs identify artifacts but do not
authenticate untrusted inputs. Worker CPU, memory, time, and output-size limits
remain application policy.

Unprivileged applications can use `Cache::open_xdg("my-profiler", epoch)` for
`$XDG_CACHE_HOME/my-profiler/gsym`, with the standard `$HOME/.cache` fallback.
Services and privileged profilers should configure an explicit root below a
parent that untrusted users cannot rename or replace.

Increment `CacheEpoch` whenever the same build ID could intentionally produce
different bytes. Remove old epoch directories only after their workers stop,
and create a new `Cache` after replacing a namespace directory.

The locking, durability, and no-clobber guarantees assume a private cache root
on a suitable local Linux filesystem.

## Cargo features

| Feature | Default | Provides |
| --- | --- | --- |
| `lookup` | Yes | Build IDs, cache opening, and lock-free lookup |
| `access` | No | Debounced recency markers; implies `lookup` |
| `manage` | No | Population, negative caching, pruning, and scrubbing; implies `access` |

## Documentation

The API documentation includes deployment, operations, and cookbook guides.
The repository also contains standalone
[`lookup`](examples/lookup.rs),
[`populate`](examples/populate.rs), and
[`maintain`](examples/maintain.rs) examples.

## License

Licensed under either Apache-2.0 or MIT, at your option.
