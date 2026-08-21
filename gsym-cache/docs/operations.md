# Operations guide

The `manage` feature turns a miss into an explicit state machine. It never
starts a worker and never waits for another process.

## Population state machine

Call [`Cache::try_begin_population`](crate::Cache::try_begin_population) after a
miss, or directly when the caller is prepared to handle every outcome:

| Outcome | Meaning | Caller action |
| --- | --- | --- |
| [`Present`](crate::PopulationOutcome::Present) | a valid entry already exists | use the returned entry |
| [`Acquired`](crate::PopulationOutcome::Acquired) | this caller owns population | publish bytes, record a failure, or abandon |
| [`Suppressed`](crate::PopulationOutcome::Suppressed) | an unexpired failure is cached | retry after its expiration |
| [`Busy`](crate::PopulationOutcome::Busy) | another process owns the lock slot | retry later without treating it as failure |

Acquisition rechecks both positive and negative entries while holding the lock.
That closes the race between an initial miss and another process completing
population.

An acquired [`Population`](crate::Population) creates no staging file until
[`into_writer`](crate::Population::into_writer) is called. The resulting
[`PopulationWriter`](crate::PopulationWriter) implements [`std::io::Write`], so
converters can stream directly into the cache without a second full-size copy.

[`publish`](crate::PopulationWriter::publish) verifies the complete GSYM file
and its build ID, makes it read-only, synchronizes it, and publishes without
replacing a valid racing winner. Dropping either capability abandons work and
releases its lock.

## Negative caching

Use [`record_failure_for`](crate::Population::record_failure_for) when retrying
immediately would repeat a stable or transient failure. The duration must be
nonzero and no longer than [`MAX_FAILURE_TTL`](crate::MAX_FAILURE_TTL).

Choose short durations for conditions that may clear quickly. Failure classes
are stable data, while retry policy belongs to the application:

- [`MissingInput`](crate::FailureKind::MissingInput): executable or debug data
  is not currently available;
- [`UnsupportedInput`](crate::FailureKind::UnsupportedInput): the selected
  conversion mode cannot handle the input;
- [`MalformedInput`](crate::FailureKind::MalformedInput): input validation
  failed;
- [`TransientIo`](crate::FailureKind::TransientIo): a temporary I/O operation
  failed; and
- [`ResourceExhausted`](crate::FailureKind::ResourceExhausted): conversion hit
  an application resource limit.

Malformed and expired records are ignored by lookup and reclaimed during
population or scrubbing. A positive entry always takes precedence.

## Access tracking

With `access`, call [`Cache::record_access`](crate::Cache::record_access) only
after a successful lookup. The marker is updated at most once per hour, keeping
the GSYM object's own modification time immutable and limiting write traffic.

```no_run
use gsym_cache::{BuildId, Cache, CacheEpoch};

let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
let build_id = BuildId::new([0x12; 20])?;
if let Some(entry) = cache.lookup(&build_id)? {
    cache.record_access(&build_id)?;
    consume(entry);
}
# fn consume(_: gsym_cache::CacheEntry) {}
# Ok::<(), Box<dyn std::error::Error>>(())
```

Calling `record_access` for a miss creates an orphan marker. Scrubbing removes
such markers, but avoiding them keeps maintenance and write traffic smaller.

## Pruning and scrubbing

[`Cache::prune`](crate::Cache::prune) applies a required byte limit and optional
entry-count and unused-age limits. Capacity pruning begins above the configured
high watermark and continues to 80% of it, avoiding deletion on every new
publication. Age pruning applies even when the cache is below capacity.

[`Cache::scrub`](crate::Cache::scrub) performs the expensive integrity pass. It
fully verifies recognized GSYM objects and removes confirmed corruption,
expired or malformed negative records, orphan access markers, and staging files
left by crashes for at least one day.

Both operations take a nonblocking maintenance lock and skip active population
slots. A `Busy` outcome means another process is already maintaining the epoch.

## Crash behavior

- A crash before publication leaves no visible object. The temporary file is
  later eligible for scrubbing.
- A crash after atomic rename may leave a valid object even if the caller did
  not observe success. The next population attempt verifies and returns it.
- A crash while writing a negative record leaves either the previous complete
  record or a temporary file; partial records are never authoritative.
- Open readers retain their file descriptors if pruning unlinks an object.

Schedule pruning according to storage pressure and scrubbing according to the
acceptable delay for reclaiming corruption and crash leftovers.
