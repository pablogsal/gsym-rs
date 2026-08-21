# Cookbook

These recipes use only the cache API. Fetching binaries, converting them to
GSYM, and enforcing worker limits remain application responsibilities.

## Parse or construct a build ID

```rust
use gsym_cache::BuildId;

let parsed: BuildId = "0123abcdef".parse()?;
let binary = BuildId::new([0x01, 0x23, 0xab, 0xcd, 0xef])?;
assert_eq!(parsed, binary);
# Ok::<(), gsym_cache::BuildIdError>(())
```

The hexadecimal parser accepts upper- or lowercase input and formatting always
produces lowercase. Empty IDs, odd-length hexadecimal strings, invalid digits,
and IDs too long for the `.build-id` layout are rejected.

## Read an entry

```no_run
use gsym_cache::{BuildId, Cache, CacheEpoch};

let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let build_id: BuildId = "0123abcdef".parse()?;

if let Some(entry) = cache.lookup(&build_id)? {
    println!("{} cached bytes", entry.len());
    let file = entry.into_file();
    consume(file);
}
# fn consume(_: std::fs::File) {}
# Ok::<(), Box<dyn std::error::Error>>(())
```

The entry owns a read-only file descriptor. It can be borrowed with
[`file`](crate::CacheEntry::file), converted to [`std::fs::File`], or passed to
an API accepting [`std::os::fd::AsFd`].

## Publish a generated file

```no_run
use std::fs::File;
use std::io;
use gsym_cache::{BuildId, Cache, CacheEpoch, PopulationOutcome};

let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let build_id: BuildId = "0123abcdef".parse()?;

match cache.try_begin_population(&build_id)? {
    PopulationOutcome::Acquired(population) => {
        let mut input = File::open("generated.gsym")?;
        let mut output = population.into_writer()?;
        io::copy(&mut input, &mut output)?;
        let outcome = output.publish()?;
        println!("published: {}", outcome.is_published());
    }
    PopulationOutcome::Present(entry) => {
        println!("already cached: {} bytes", entry.len());
    }
    PopulationOutcome::Suppressed(failure) => {
        println!("suppressed until {:?}", failure.expires_at());
    }
    PopulationOutcome::Busy => println!("retry later"),
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Do not reopen the staging path or make the staged file externally writable.
The writer capability exists to keep verification and publication within one
exclusive lifecycle.

## Suppress a failed conversion

```no_run
use std::time::Duration;
use gsym_cache::{BuildId, Cache, CacheEpoch, FailureKind, PopulationOutcome};

let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let build_id = BuildId::new([0x12; 20])?;

if let PopulationOutcome::Acquired(population) = cache.try_begin_population(&build_id)? {
    let failure = population.record_failure_for(
        FailureKind::MissingInput,
        Duration::from_secs(30),
    )?;
    println!("retry at {:?}", failure.expires_at());
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

A writer that already contains partial output can record the same failure; its
temporary file is discarded before the lock is released.

## Track access and maintain capacity

```no_run
use std::time::Duration;
use gsym_cache::{ByteLimit, Cache, CacheEpoch, EntryLimit, PrunePolicy};

let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let policy = PrunePolicy::new(ByteLimit::try_from(10 * 1024 * 1024 * 1024_u64)?)
    .max_entries(EntryLimit::try_from(100_000_u64)?)
    .max_unused_age(Duration::from_secs(30 * 24 * 60 * 60));

if let Some(report) = cache.prune(policy)?.into_report() {
    println!(
        "removed {}; {} entries and {} bytes remain",
        report.removed, report.after.entries, report.after.bytes,
    );
}

if let Some(report) = cache.scrub()?.into_report() {
    println!(
        "checked {}; removed {} corrupt objects",
        report.checked, report.removed_corrupt,
    );
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Applications with many worker processes normally elect one periodic
maintenance caller. The cache still returns `Busy` safely if schedules overlap.
