# Deployment guide

The cache is deliberately smaller than a converter or artifact service. It
stores immutable GSYM files by build ID and provides the synchronization needed
to publish and maintain them safely. The application still decides where GSYM
comes from, which inputs it trusts, and how much work a converter may perform.

## Pick a root

For an unprivileged desktop or command-line application, use
[`Cache::open_xdg`](crate::Cache::open_xdg):

```no_run
use gsym_cache::{Cache, CacheEpoch};

let cache = Cache::open_xdg("my-profiler", CacheEpoch::new(1))?;
assert!(cache.root().ends_with("my-profiler/gsym"));
# Ok::<(), gsym_cache::Error>(())
```

This follows `XDG_CACHE_HOME`, with the standard `$HOME/.cache` fallback. The
application name must be one normal path component. Existing cache-home and
application directories are checked before use; shared writable anchors are
rejected unless their replacement rules are safe.

Services and privileged processes should use [`Cache::open`](crate::Cache::open)
with an explicitly configured absolute root. The root must be a private,
non-symlink directory owned by the effective user. Its parent must prevent an
untrusted user from replacing the root.

Use a separate root for each trust domain. A build ID identifies an artifact;
it does not authenticate the bytes supplied by a downloader or converter.

## Version conversion policy with epochs

[`CacheEpoch`](crate::CacheEpoch) is part of the on-disk namespace. Increment it
whenever identical input and build ID might intentionally produce different
GSYM bytes—for example after changing the GSYM version, conversion options, or
converter semantics.

```rust
use gsym_cache::CacheEpoch;

const CURRENT_EPOCH: CacheEpoch = CacheEpoch::new(3);
assert_eq!(CURRENT_EPOCH.get(), 3);
```

Different epochs never share objects, negative records, or access markers.
Remove an obsolete epoch only after processes using it have stopped. Construct
a new [`Cache`](crate::Cache) after replacing any namespace directory because
opened directory descriptors are intentionally pinned.

## Filesystem contract

Use a private cache root on a local Linux filesystem. The implementation relies
on the usual semantics of:

- `flock` advisory locks shared by cooperating processes;
- no-clobber atomic rename for publication;
- read-only permissions after verification;
- file and directory `fsync` for durable publication; and
- directory descriptors plus no-follow opens to reject symlink traversal.

Network filesystems and unusual overlay filesystems may provide weaker locking
or durability guarantees. Validate those guarantees before using them for a
cache shared by multiple processes.

## Process and threat model

Lookups take `&self`, acquire no advisory lock, and do not write. A returned
[`CacheEntry`](crate::CacheEntry) owns its file descriptor, so pruning cannot
invalidate an already-open reader.

Managed population uses a bounded table of 4096 lock slots. Unrelated build IDs
can collide and briefly report `Busy`, but a collision cannot publish bytes
under the wrong key. Callers should treat `Busy` as a retry signal rather than
an error.

The cache validates directory ownership and file type. It is not a sandbox:
run converters with appropriate CPU, memory, time, and output-size limits when
their input is untrusted.

## Deployment checklist

- Use one private root per trust domain.
- Use `open_xdg` only for an unprivileged application cache.
- Use an explicit protected root for services or privileged profilers.
- Bump the epoch when conversion policy changes.
- Keep conversion resource limits outside the cache crate.
- Schedule maintenance when the `manage` feature is enabled.
- Recreate `Cache` values after replacing namespace directories.
