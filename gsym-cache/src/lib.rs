//! Concurrent, process-safe storage for immutable [LLVM GSYM] files.
//!
//! `gsym-cache` separates the profiler's read path from the machinery that
//! creates and maintains cached files. A lookup takes no lock and performs no
//! write. Optional features add debounced recency markers, nonblocking
//! population ownership, verified atomic publication, negative caching, and
//! bounded maintenance.
//!
//! Conversion, downloads, worker scheduling, and resource limits remain with
//! the application.
#![cfg_attr(
    feature = "lookup",
    doc = r#"
# Quick start

Open a cache without creating it, then look up a binary build identifier:

```no_run
use gsym_cache::{BuildId, Cache, CacheEpoch};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let build_id: BuildId = "1212121212121212121212121212121212121212".parse()?;
if let Some(entry) = cache.lookup(&build_id)? {
    println!("cached GSYM: {} bytes", entry.len());
}
# Ok(())
# }
```
"#
)]
#![cfg_attr(
    feature = "manage",
    doc = r#"
Population is an explicit, nonblocking state machine:

```no_run
use std::fs::File;
use std::io;
use gsym_cache::{BuildId, Cache, CacheEpoch, PopulationOutcome};

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let build_id = BuildId::new([0x12; 20])?;

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
"#
)]
#![cfg_attr(
    feature = "lookup",
    doc = r#"
# Choosing an entry point

| To … | Use | Notes |
| --- | --- | --- |
| use an explicit cache root | `Cache::open` | suitable for services and privileged applications |
| follow the XDG cache convention | `Cache::open_xdg` | validates the cache home and application component |
| read an immutable entry | `Cache::lookup` | lock-free and deliberately does not decode GSYM |
| record coarse recency | `Cache::record_access` | `access` feature; call only after a hit |
| coordinate a cache miss | `Cache::try_begin_population` | `manage` feature; never waits |
| enforce capacity or age limits | `Cache::prune` | `manage` feature; uses 80% low-watermark hysteresis |
| verify and repair stored state | `Cache::scrub` | `manage` feature; checks complete GSYM files |

# Usage notes

* A [`BuildId`] is an opaque cache key, not proof that input is trustworthy.
  Keep one private root per trust domain and isolate converters that consume
  untrusted binaries.

* [`CacheEpoch`] versions the application's conversion policy. Increment it
  when the same build ID could produce different bytes. Old epochs are separate
  namespaces and can be removed after their workers stop.

* [`Cache::lookup`] verifies ownership and file type, but not GSYM contents.
  Managed publication and scrubbing perform complete GSYM verification so the
  profiler-facing path stays small.

* Directory descriptors are pinned after they are opened. Create a new
  [`Cache`] after replacing a cache namespace directory.

* Durability and no-clobber behavior assume a private cache root on a local
  Linux filesystem with advisory `flock`, atomic rename, and directory `fsync`
  semantics.

# Feature flags

| Feature | Default | Adds |
| --- | --- | --- |
| `lookup` | yes | build IDs, cache opening, and lock-free lookup |
| `access` | no | debounced access markers; implies `lookup` |
| `manage` | no | population, negative caching, pruning, and scrubbing; implies `access` |

Read-only use needs only the default feature:

```toml
[dependencies]
gsym-cache = "0.1"
```

# Errors

Fallible entry points return [`Result<T>`](Result). [`Error`] distinguishes I/O,
unsafe directory or entry layouts, invalid GSYM, build-ID mismatch, and invalid
negative-cache lifetimes. It is `#[non_exhaustive]`, so exhaustive matches need
a fallback arm.

Cache misses, active population, and active maintenance are ordinary outcomes,
not errors.

# Guides

- [`docs::deployment`]: roots, epochs, trust boundaries, filesystems, and
  process behavior.
"#
)]
#![cfg_attr(
    feature = "manage",
    doc = r"- [`docs::operations`]: population, negative caching, access tracking, pruning, scrubbing, and crash recovery.
- [`docs::cookbook`]: complete lookup, publication, suppression, and maintenance recipes.
"
)]
//! [LLVM GSYM]: <https://llvm.org/doxygen/namespacellvm_1_1gsym.html>
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(not(target_os = "linux"))]
compile_error!("gsym-cache currently supports Linux only");

#[cfg(all(target_os = "linux", feature = "access"))]
mod access;
#[cfg(all(target_os = "linux", feature = "lookup"))]
mod build_id;
#[cfg(all(target_os = "linux", feature = "lookup"))]
pub mod docs;
#[cfg(target_os = "linux")]
mod error;
#[cfg(all(target_os = "linux", feature = "lookup"))]
mod layout;
#[cfg(all(target_os = "linux", feature = "lookup"))]
mod lookup;
#[cfg(all(target_os = "linux", feature = "manage"))]
mod maintenance;
#[cfg(all(target_os = "linux", feature = "manage"))]
mod manage;

#[cfg(all(target_os = "linux", feature = "access"))]
#[cfg_attr(docsrs, doc(cfg(feature = "access")))]
#[doc(inline)]
pub use access::AccessUpdate;
#[cfg(all(target_os = "linux", feature = "lookup"))]
#[cfg_attr(docsrs, doc(cfg(feature = "lookup")))]
#[doc(inline)]
pub use build_id::{BuildId, BuildIdError};
#[cfg(all(target_os = "linux", feature = "manage"))]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
#[doc(inline)]
pub use error::{BuildIdMismatchError, InvalidGsymError};
#[cfg(target_os = "linux")]
pub use error::{Error, Result};
#[cfg(all(target_os = "linux", feature = "lookup"))]
#[cfg_attr(docsrs, doc(cfg(feature = "lookup")))]
#[doc(inline)]
pub use lookup::{Cache, CacheEntry, CacheEpoch};
#[cfg(all(target_os = "linux", feature = "manage"))]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
#[doc(inline)]
pub use maintenance::{
    ByteLimit, CacheStats, EntryLimit, PruneOutcome, PrunePolicy, PruneReport, ScrubOutcome,
    ScrubReport,
};
#[cfg(all(target_os = "linux", feature = "manage"))]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
#[doc(inline)]
pub use manage::{
    CachedFailure, FailureKind, MAX_FAILURE_TTL, Population, PopulationOutcome, PopulationWriter,
    PublishOutcome,
};

/// Compiles the README's examples as doctests without rendering it twice.
#[cfg(all(doctest, feature = "lookup"))]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;
