use std::collections::BinaryHeap;
use std::fs::File;
use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::build_id::{MAX_BUILD_ID_LEN, hex_nibble};
use crate::error::io_error;
use crate::manage::{FailureRecord, is_corrupt, remove_file_if_exists, try_lock_file};
use crate::{BuildId, Cache, Result, layout};

const STALE_TEMPORARY_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MIN_PRUNE_BATCH_SIZE: usize = 256;
const MAX_PRUNE_BATCH_SIZE: usize = 65_536;

/// Nonzero cache-size limit in bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub struct ByteLimit(NonZeroU64);

impl ByteLimit {
    /// Creates a byte limit, returning `None` for zero.
    #[must_use]
    pub const fn new(bytes: u64) -> Option<Self> {
        match NonZeroU64::new(bytes) {
            Some(bytes) => Some(Self(bytes)),
            None => None,
        }
    }

    /// Returns the limit in bytes.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Nonzero cache-entry limit.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub struct EntryLimit(NonZeroU64);

impl EntryLimit {
    /// Creates an entry limit, returning `None` for zero.
    #[must_use]
    pub const fn new(entries: u64) -> Option<Self> {
        match NonZeroU64::new(entries) {
            Some(entries) => Some(Self(entries)),
            None => None,
        }
    }

    /// Returns the entry limit.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

macro_rules! impl_limit_conversions {
    ($limit:ty) => {
        impl From<NonZeroU64> for $limit {
            fn from(value: NonZeroU64) -> Self {
                Self(value)
            }
        }

        impl From<$limit> for NonZeroU64 {
            fn from(limit: $limit) -> Self {
                limit.0
            }
        }

        impl From<$limit> for u64 {
            fn from(limit: $limit) -> Self {
                limit.get()
            }
        }

        impl TryFrom<u64> for $limit {
            type Error = std::num::TryFromIntError;

            fn try_from(value: u64) -> std::result::Result<Self, Self::Error> {
                NonZeroU64::try_from(value).map(Self)
            }
        }
    };
}

impl_limit_conversions!(ByteLimit);
impl_limit_conversions!(EntryLimit);

/// Size, count, and age policy for cache pruning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub struct PrunePolicy {
    bytes: ByteLimit,
    entries: Option<EntryLimit>,
    unused_age: Option<Duration>,
}

impl PrunePolicy {
    /// Creates a byte-bounded policy.
    #[must_use]
    pub const fn new(max_bytes: ByteLimit) -> Self {
        Self {
            bytes: max_bytes,
            entries: None,
            unused_age: None,
        }
    }

    /// Adds a maximum entry count.
    #[must_use]
    pub const fn max_entries(mut self, limit: EntryLimit) -> Self {
        self.entries = Some(limit);
        self
    }

    /// Removes entries unused for at least `age`, even below capacity.
    #[must_use]
    pub const fn max_unused_age(mut self, age: Duration) -> Self {
        self.unused_age = Some(age);
        self
    }

    /// Returns the high byte watermark.
    #[must_use]
    pub const fn byte_limit(self) -> ByteLimit {
        self.bytes
    }

    /// Returns the optional high entry watermark.
    #[must_use]
    pub const fn entry_limit(self) -> Option<EntryLimit> {
        self.entries
    }

    /// Returns the optional maximum unused age.
    #[must_use]
    pub const fn unused_age(self) -> Option<Duration> {
        self.unused_age
    }
}

#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
impl Cache {
    /// Counts recognized GSYM objects in this converter epoch without writing
    /// to the cache.
    ///
    /// # Errors
    ///
    /// Returns an error when the object tree cannot be scanned.
    pub fn stats(&self) -> Result<CacheStats> {
        object_totals(self)
    }

    /// Prunes this converter epoch's least-recently-used objects under a
    /// nonblocking maintenance lock.
    ///
    /// Capacity pruning starts only above a high watermark and continues to
    /// 80% of it. This hysteresis prevents a deletion on every subsequent
    /// publication. Objects whose population lock is held are skipped.
    ///
    /// See [`docs::operations`](crate::docs::operations) for scheduling and
    /// recovery guidance.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache cannot be scanned, locked, or modified.
    pub fn prune(&self, policy: PrunePolicy) -> Result<PruneOutcome> {
        self.prepare()?;
        let Some(_maintenance_lock) = try_maintenance_lock(self)? else {
            return Ok(PruneOutcome::Busy);
        };

        let now = SystemTime::now();
        let mut progress = match policy.unused_age {
            Some(age) => prune_by_age(self, age, now)?,
            None => PruneProgress::new(object_totals(self)?),
        };
        let byte_triggered = progress.before.bytes > policy.bytes.get();
        let entry_triggered = policy
            .entries
            .is_some_and(|limit| progress.before.entries > limit.get());
        if !byte_triggered && !entry_triggered {
            return Ok(PruneOutcome::Completed(progress.report()));
        }
        let target_bytes = low_watermark(policy.bytes.get());
        let target_entries = policy.entries.map(|limit| low_watermark(limit.get()));
        let over_target = |stats: CacheStats| {
            (byte_triggered && stats.bytes > target_bytes)
                || (entry_triggered && target_entries.is_some_and(|target| stats.entries > target))
        };
        let batch_size = prune_batch_size(
            progress.after,
            byte_triggered.then_some(target_bytes),
            target_entries.filter(|_| entry_triggered),
        );
        let mut candidate_storage = Vec::with_capacity(batch_size);
        let mut retried_changed_batch = false;

        'prune: loop {
            if !over_target(progress.after) {
                break;
            }

            let mut candidates = BinaryHeap::from(std::mem::take(&mut candidate_storage));
            visit_object_entries(self, |prefix, suffix, decoded_len, metadata| {
                let Some(build_id) = decode_build_id_bytes(prefix, suffix, decoded_len) else {
                    return Ok(());
                };
                if progress
                    .busy_slots
                    .contains(layout::lock_slot(build_id.as_bytes()))
                {
                    return Ok(());
                }
                let last_used = access_time_bytes(self, build_id.as_bytes(), now)
                    .or_else(|| metadata.modified().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                retain_oldest(
                    &mut candidates,
                    batch_size,
                    build_id,
                    metadata.len(),
                    last_used,
                );
                Ok(())
            })?;
            if candidates.is_empty() {
                break;
            }

            let mut advanced = false;
            let mut changed = false;
            candidate_storage = candidates.into_sorted_vec();
            #[expect(
                clippy::iter_with_drain,
                reason = "draining retains the bounded candidate allocation for the next scan"
            )]
            for candidate in candidate_storage.drain(..) {
                if !over_target(progress.after) {
                    break 'prune;
                }
                let slot = layout::lock_slot(candidate.build_id.as_bytes());
                if progress.busy_slots.contains(slot) {
                    continue;
                }
                match try_remove_prune_candidate(self, &candidate, now)? {
                    PruneCandidateOutcome::Removed => {
                        progress.record_removed(candidate.len);
                        advanced = true;
                    }
                    PruneCandidateOutcome::Busy => {
                        if progress.record_busy(slot) {
                            advanced = true;
                        }
                    }
                    PruneCandidateOutcome::Changed => changed = true,
                }
            }

            if advanced {
                retried_changed_batch = false;
            } else if changed && !retried_changed_batch {
                retried_changed_batch = true;
            } else {
                break;
            }
        }

        Ok(PruneOutcome::Completed(progress.report()))
    }

    /// Verifies every recognized object and removes corrupt entries, expired
    /// negative records, and stale crash-leftover staging files.
    ///
    /// Temporary files younger than one day are retained. Older files are
    /// removed only after acquiring their build identifier's population lock,
    /// so a long-running conversion cannot lose its staging path.
    ///
    /// See [`docs::operations`](crate::docs::operations) for the full repair
    /// contract and crash behavior.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache cannot be scanned, locked, mapped, or
    /// modified. Transient mapping failures do not cause object deletion.
    pub fn scrub(&self) -> Result<ScrubOutcome> {
        self.prepare()?;
        let Some(_maintenance_lock) = try_maintenance_lock(self)? else {
            return Ok(ScrubOutcome::Busy);
        };

        let mut report = ScrubReport::default();
        visit_object_build_ids(self, |build_id, object| {
            let Some(_entry_lock) = try_entry_lock(self, &build_id)? else {
                report.skipped_busy = report.skipped_busy.saturating_add(1);
                return Ok(());
            };
            let path = object.path();
            let Some(entry) = self.lookup_path(&path)? else {
                return Ok(());
            };
            report.checked = report.checked.saturating_add(1);
            match crate::manage::verify_file(entry.file(), &build_id, &path) {
                Ok(()) => {}
                Err(error) if is_corrupt(&error) => {
                    drop(entry);
                    if remove_object(self, &build_id, &path)? {
                        report.removed_corrupt = report.removed_corrupt.saturating_add(1);
                    }
                }
                Err(error) => return Err(error),
            }
            Ok(())
        })?;
        let (removed_temporary, skipped_temporary) =
            scrub_temporary_files(self, SystemTime::now())?;
        report.removed_negative = scrub_negative_records(self)?;
        report.removed_access = scrub_orphan_access_markers(self)?;
        report.removed_temporary = removed_temporary;
        report.skipped_busy = report.skipped_busy.saturating_add(skipped_temporary);
        Ok(ScrubOutcome::Completed(report))
    }
}

fn prune_by_age(cache: &Cache, age: Duration, now: SystemTime) -> Result<PruneProgress> {
    let mut progress = PruneProgress::default();
    visit_object_entries(cache, |prefix, suffix, decoded_len, metadata| {
        let Some(build_id) = decode_build_id_bytes(prefix, suffix, decoded_len) else {
            return Ok(());
        };
        let len = metadata.len();
        progress.before.entries = progress.before.entries.saturating_add(1);
        progress.before.bytes = progress.before.bytes.saturating_add(len);
        progress.after.entries = progress.after.entries.saturating_add(1);
        progress.after.bytes = progress.after.bytes.saturating_add(len);
        let slot = layout::lock_slot(build_id.as_bytes());
        if progress.busy_slots.contains(slot) {
            return Ok(());
        }
        let last_used = access_time_bytes(cache, build_id.as_bytes(), now)
            .or_else(|| metadata.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if now.duration_since(last_used).unwrap_or_default() < age {
            return Ok(());
        }
        let Some(build_id) = CandidateBuildId::from_decoded(build_id) else {
            return Ok(());
        };
        let candidate = Candidate {
            last_used,
            build_id,
            len,
        };
        match try_remove_prune_candidate(cache, &candidate, now)? {
            PruneCandidateOutcome::Removed => {
                progress.record_removed(len);
            }
            PruneCandidateOutcome::Busy => {
                let _ = progress.record_busy(slot);
            }
            PruneCandidateOutcome::Changed => {}
        }
        Ok(())
    })?;
    Ok(progress)
}

#[derive(Default)]
struct PruneProgress {
    before: CacheStats,
    after: CacheStats,
    removed: u64,
    skipped_busy: u64,
    busy_slots: BusySlots,
}

impl PruneProgress {
    fn new(stats: CacheStats) -> Self {
        Self {
            before: stats,
            after: stats,
            ..Self::default()
        }
    }

    const fn report(self) -> PruneReport {
        PruneReport {
            before: self.before,
            after: self.after,
            removed: self.removed,
            skipped_busy: self.skipped_busy,
        }
    }

    const fn record_removed(&mut self, len: u64) {
        self.after.bytes = self.after.bytes.saturating_sub(len);
        self.after.entries = self.after.entries.saturating_sub(1);
        self.removed = self.removed.saturating_add(1);
    }

    fn record_busy(&mut self, slot: u16) -> bool {
        let inserted = self.busy_slots.insert(slot);
        if inserted {
            self.skipped_busy = self.skipped_busy.saturating_add(1);
        }
        inserted
    }
}

/// Current size of the recognized object tree.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub struct CacheStats {
    /// Number of immutable GSYM objects.
    pub entries: u64,
    /// Sum of their logical file lengths.
    pub bytes: u64,
}

/// Outcome of nonblocking cache pruning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub enum PruneOutcome {
    /// This process completed a prune pass.
    Completed(PruneReport),
    /// Another process currently owns cache maintenance.
    Busy,
}

impl PruneOutcome {
    /// Returns whether another process currently owns cache maintenance.
    #[must_use]
    pub const fn is_busy(&self) -> bool {
        matches!(self, Self::Busy)
    }

    /// Consumes the outcome and returns the report when pruning completed.
    #[must_use]
    pub const fn into_report(self) -> Option<PruneReport> {
        match self {
            Self::Completed(report) => Some(report),
            Self::Busy => None,
        }
    }
}

/// Measurements from a completed prune pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub struct PruneReport {
    /// Object-tree measurements before pruning.
    pub before: CacheStats,
    /// Object-tree measurements after pruning.
    pub after: CacheStats,
    /// Number of objects removed.
    pub removed: u64,
    /// Number of busy population-lock slots encountered.
    pub skipped_busy: u64,
}

/// Outcome of nonblocking cache scrubbing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub enum ScrubOutcome {
    /// This process completed a scrub pass.
    Completed(ScrubReport),
    /// Another process currently owns cache maintenance.
    Busy,
}

impl ScrubOutcome {
    /// Returns whether another process currently owns cache maintenance.
    #[must_use]
    pub const fn is_busy(&self) -> bool {
        matches!(self, Self::Busy)
    }

    /// Consumes the outcome and returns the report when scrubbing completed.
    #[must_use]
    pub const fn into_report(self) -> Option<ScrubReport> {
        match self {
            Self::Completed(report) => Some(report),
            Self::Busy => None,
        }
    }
}

/// Measurements from a completed scrub pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub struct ScrubReport {
    /// Number of immutable GSYM objects fully verified.
    pub checked: u64,
    /// Number of malformed or build-ID-mismatched objects removed.
    pub removed_corrupt: u64,
    /// Number of old crash-leftover staging files removed.
    pub removed_temporary: u64,
    /// Number of expired or malformed negative records removed.
    pub removed_negative: u64,
    /// Number of access markers without a corresponding object removed.
    pub removed_access: u64,
    /// Number of objects or staging files skipped because population was active.
    pub skipped_busy: u64,
}

fn scrub_negative_records(cache: &Cache) -> Result<u64> {
    let mut removed = 0_u64;
    visit_keyed_entries(
        cache,
        layout::NEGATIVE,
        layout::NEGATIVE_EXTENSION,
        |prefix, suffix, entry| {
            let Some(build_id) = decode_build_id(prefix, suffix) else {
                return Ok(());
            };
            let Some(_entry_lock) = try_entry_lock(cache, &build_id)? else {
                return Ok(());
            };
            let path = entry.path();
            if !matches!(cache.failure_record(&build_id)?, FailureRecord::Cached(_))
                && remove_file_if_exists("remove negative-cache record", &path)?
            {
                removed = removed.saturating_add(1);
            }
            Ok(())
        },
    )?;
    Ok(removed)
}

fn scrub_orphan_access_markers(cache: &Cache) -> Result<u64> {
    let mut removed = 0_u64;
    visit_keyed_entries(
        cache,
        layout::ACCESS,
        layout::ACCESS_EXTENSION,
        |prefix, suffix, entry| {
            let Some(build_id) = decode_build_id(prefix, suffix) else {
                return Ok(());
            };
            let path = entry.path();
            let object = layout::object(cache.base(), &build_id);
            match std::fs::symlink_metadata(&object) {
                Ok(metadata) if metadata.is_file() => return Ok(()),
                Ok(_) => {}
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(io_error("inspect cache object", object, source)),
            }
            let Some(_entry_lock) = try_entry_lock(cache, &build_id)? else {
                return Ok(());
            };
            match std::fs::symlink_metadata(&object) {
                Ok(metadata) if metadata.is_file() => return Ok(()),
                Ok(_) => {}
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(io_error("recheck cache object", object, source)),
            }
            if remove_file_if_exists("remove orphan access marker", &path)? {
                removed = removed.saturating_add(1);
            }
            Ok(())
        },
    )?;
    Ok(removed)
}

fn visit_keyed_entries(
    cache: &Cache,
    kind: &str,
    extension: &str,
    mut visitor: impl FnMut(&str, &str, std::fs::DirEntry) -> Result<()>,
) -> Result<()> {
    let root = cache.base().join(kind).join(layout::BUILD_ID);
    let shards = match std::fs::read_dir(&root) {
        Ok(shards) => shards,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error("scan cache metadata shards", root, source)),
    };
    for shard in shards {
        let shard = shard.map_err(|source| io_error("read cache metadata shard", &root, source))?;
        let prefix_os = shard.file_name();
        let Some(prefix) = prefix_os.to_str() else {
            continue;
        };
        if prefix.len() != 2 || !is_lower_hex(prefix) {
            continue;
        }
        let shard_path = shard.path();
        let metadata = std::fs::symlink_metadata(&shard_path)
            .map_err(|source| io_error("inspect cache metadata shard", &shard_path, source))?;
        if !metadata.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&shard_path)
            .map_err(|source| io_error("scan cache metadata shard", &shard_path, source))?
        {
            let entry = entry
                .map_err(|source| io_error("read cache metadata shard", &shard_path, source))?;
            let name_os = entry.file_name();
            let Some(name) = name_os.to_str() else {
                continue;
            };
            let Some(suffix) = name.strip_suffix(extension) else {
                continue;
            };
            if !is_lower_hex(suffix) {
                continue;
            }
            visitor(prefix, suffix, entry)?;
        }
    }
    Ok(())
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Candidate {
    last_used: SystemTime,
    build_id: CandidateBuildId,
    len: u64,
}

// Covers SHA-1 and SHA-256 identifiers without inflating the bounded heap for
// the format's uncommon 126-byte maximum.
const INLINE_CANDIDATE_BUILD_ID_LEN: usize = 32;

#[derive(Debug)]
enum CandidateBuildId {
    Inline {
        bytes: [u8; INLINE_CANDIDATE_BUILD_ID_LEN],
        len: u8,
    },
    Heap(BuildId),
}

impl CandidateBuildId {
    fn from_decoded(decoded: BuildIdBytes) -> Option<Self> {
        let source = decoded.as_bytes();
        if source.len() > INLINE_CANDIDATE_BUILD_ID_LEN {
            return decoded.into_owned().map(Self::Heap);
        }
        let mut bytes = [0; INLINE_CANDIDATE_BUILD_ID_LEN];
        bytes.get_mut(..source.len())?.copy_from_slice(source);
        Some(Self::Inline {
            bytes,
            len: u8::try_from(source.len()).ok()?,
        })
    }

    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Inline { bytes, len } => bytes.get(..usize::from(*len)).unwrap_or_default(),
            Self::Heap(build_id) => build_id.as_bytes(),
        }
    }
}

impl PartialEq for CandidateBuildId {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for CandidateBuildId {}

impl PartialOrd for CandidateBuildId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CandidateBuildId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_bytes().cmp(other.as_bytes())
    }
}

fn retain_oldest(
    candidates: &mut BinaryHeap<Candidate>,
    capacity: usize,
    build_id: BuildIdBytes,
    len: u64,
    last_used: SystemTime,
) {
    let retain = candidates.len() < capacity
        || candidates.peek().is_some_and(|newest| {
            last_used
                .cmp(&newest.last_used)
                .then_with(|| build_id.as_bytes().cmp(newest.build_id.as_bytes()))
                .then_with(|| len.cmp(&newest.len))
                .is_lt()
        });
    if !retain {
        return;
    }
    let Some(build_id) = CandidateBuildId::from_decoded(build_id) else {
        return;
    };
    let candidate = Candidate {
        last_used,
        build_id,
        len,
    };
    if candidates.len() == capacity {
        drop(candidates.pop());
    }
    candidates.push(candidate);
}

fn object_totals(cache: &Cache) -> Result<CacheStats> {
    let mut stats = CacheStats::default();
    visit_object_entries(cache, |_prefix, _suffix, _decoded_len, metadata| {
        stats.entries = stats.entries.saturating_add(1);
        stats.bytes = stats.bytes.saturating_add(metadata.len());
        Ok(())
    })?;
    Ok(stats)
}

fn visit_object_entries(
    cache: &Cache,
    mut visitor: impl FnMut(&str, &str, usize, std::fs::Metadata) -> Result<()>,
) -> Result<()> {
    visit_object_names(cache, |prefix, suffix, decoded_len, file| {
        let metadata = file
            .metadata()
            .map_err(|source| io_error("inspect cache object", file.path(), source))?;
        if metadata.is_file() {
            visitor(prefix, suffix, decoded_len, metadata)?;
        }
        Ok(())
    })
}

fn visit_object_build_ids(
    cache: &Cache,
    mut visitor: impl FnMut(BuildId, std::fs::DirEntry) -> Result<()>,
) -> Result<()> {
    visit_object_names(cache, |prefix, suffix, decoded_len, file| {
        let file_type = file
            .file_type()
            .map_err(|source| io_error("inspect cache object type", file.path(), source))?;
        #[expect(
            clippy::filetype_is_file,
            reason = "cache objects must be regular files, not merely non-directories"
        )]
        if file_type.is_file()
            && let Some(build_id) = decode_build_id_with_len(prefix, suffix, decoded_len)
        {
            visitor(build_id, file)?;
        }
        Ok(())
    })
}

fn visit_object_names(
    cache: &Cache,
    mut visitor: impl FnMut(&str, &str, usize, std::fs::DirEntry) -> Result<()>,
) -> Result<()> {
    visit_keyed_entries(
        cache,
        layout::OBJECTS,
        layout::OBJECT_EXTENSION,
        |prefix, suffix, file| {
            if let Some(decoded_len) = decoded_build_id_len(prefix, suffix) {
                visitor(prefix, suffix, decoded_len, file)?;
            }
            Ok(())
        },
    )
}

fn decode_build_id(prefix: &str, suffix: &str) -> Option<BuildId> {
    decode_build_id_with_len(prefix, suffix, decoded_build_id_len(prefix, suffix)?)
}

fn decode_build_id_with_len(prefix: &str, suffix: &str, decoded_len: usize) -> Option<BuildId> {
    let mut bytes = vec![0; decoded_len];
    decode_build_id_into(prefix, suffix, &mut bytes)?;
    BuildId::try_from(bytes).ok()
}

struct BuildIdBytes {
    bytes: [u8; MAX_BUILD_ID_LEN],
    len: u8,
}

impl BuildIdBytes {
    fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or_default()
    }

    fn into_owned(self) -> Option<BuildId> {
        BuildId::try_from(self.as_bytes()).ok()
    }
}

fn decode_build_id_bytes(prefix: &str, suffix: &str, decoded_len: usize) -> Option<BuildIdBytes> {
    let mut bytes = [0_u8; MAX_BUILD_ID_LEN];
    let decoded = bytes.get_mut(..decoded_len)?;
    decode_build_id_into(prefix, suffix, decoded)?;
    Some(BuildIdBytes {
        bytes,
        len: u8::try_from(decoded_len).ok()?,
    })
}

fn decoded_build_id_len(prefix: &str, suffix: &str) -> Option<usize> {
    let encoded_len = prefix.len().checked_add(suffix.len())?;
    let len = encoded_len.checked_div(2)?;
    (encoded_len % 2 == 0 && len <= MAX_BUILD_ID_LEN).then_some(len)
}

fn decode_build_id_into(prefix: &str, suffix: &str, decoded: &mut [u8]) -> Option<()> {
    let mut encoded = prefix.bytes().chain(suffix.bytes());
    for byte in decoded {
        let high = hex_nibble(encoded.next()?)?;
        let low = hex_nibble(encoded.next()?)?;
        *byte = high << 4 | low;
    }
    encoded.next().is_none().then_some(())
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn access_time(cache: &Cache, build_id: &BuildId, now: SystemTime) -> Option<SystemTime> {
    access_time_bytes(cache, build_id.as_bytes(), now)
}

fn access_time_bytes(cache: &Cache, build_id: &[u8], now: SystemTime) -> Option<SystemTime> {
    let directory = cache.access_directory.get()?;
    let key = layout::access_key_bytes(build_id);
    let key = key.as_c_str()?;
    let metadata =
        rustix::fs::statat(directory, key, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).ok()?;
    if rustix::fs::FileType::from_raw_mode(metadata.st_mode) != rustix::fs::FileType::RegularFile {
        return None;
    }
    let modified = crate::access::system_time_from_unix(metadata.st_mtime, metadata.st_mtime_nsec)?;
    Some(modified.min(now))
}

struct BusySlots([u64; 64]);

impl Default for BusySlots {
    fn default() -> Self {
        Self([0; 64])
    }
}

impl BusySlots {
    fn contains(&self, slot: u16) -> bool {
        let index = usize::from(slot / 64);
        let mask = 1_u64 << u32::from(slot % 64);
        self.0.get(index).is_some_and(|word| word & mask != 0)
    }

    fn insert(&mut self, slot: u16) -> bool {
        let index = usize::from(slot / 64);
        let mask = 1_u64 << u32::from(slot % 64);
        let Some(word) = self.0.get_mut(index) else {
            return false;
        };
        let new = *word & mask == 0;
        *word |= mask;
        new
    }
}

fn current_candidate_path(
    cache: &Cache,
    build_id: &BuildId,
    candidate: &Candidate,
    now: SystemTime,
) -> Result<Option<PathBuf>> {
    let path = layout::object(cache.base(), build_id);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(io_error("recheck cache entry before pruning", path, source));
        }
    };
    let last_used = access_time(cache, build_id, now)
        .or_else(|| metadata.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    if !metadata.is_file() || metadata.len() != candidate.len || last_used != candidate.last_used {
        return Ok(None);
    }
    Ok(Some(path))
}

enum PruneCandidateOutcome {
    Removed,
    Busy,
    Changed,
}

fn try_remove_prune_candidate(
    cache: &Cache,
    candidate: &Candidate,
    now: SystemTime,
) -> Result<PruneCandidateOutcome> {
    match &candidate.build_id {
        CandidateBuildId::Inline { .. } => {
            let Ok(build_id) = BuildId::new(candidate.build_id.as_bytes()) else {
                return Ok(PruneCandidateOutcome::Changed);
            };
            try_remove_prune_candidate_with_id(cache, &build_id, candidate, now)
        }
        CandidateBuildId::Heap(build_id) => {
            try_remove_prune_candidate_with_id(cache, build_id, candidate, now)
        }
    }
}

fn try_remove_prune_candidate_with_id(
    cache: &Cache,
    build_id: &BuildId,
    candidate: &Candidate,
    now: SystemTime,
) -> Result<PruneCandidateOutcome> {
    let Some(_entry_lock) = try_entry_lock(cache, build_id)? else {
        return Ok(PruneCandidateOutcome::Busy);
    };
    let Some(path) = current_candidate_path(cache, build_id, candidate, now)? else {
        return Ok(PruneCandidateOutcome::Changed);
    };
    if remove_object(cache, build_id, &path)? {
        Ok(PruneCandidateOutcome::Removed)
    } else {
        Ok(PruneCandidateOutcome::Changed)
    }
}

fn remove_object(cache: &Cache, build_id: &BuildId, path: &std::path::Path) -> Result<bool> {
    if !remove_file_if_exists("remove cache entry", path)? {
        return Ok(false);
    }
    cache.clear_auxiliary(build_id);
    Ok(true)
}

fn scrub_temporary_files(cache: &Cache, now: SystemTime) -> Result<(u64, u64)> {
    let mut removed = 0_u64;
    let mut skipped_busy = 0_u64;
    visit_keyed_entries(
        cache,
        layout::TEMPORARY,
        layout::TEMPORARY_EXTENSION,
        |prefix, suffix, identifier| {
            let Some(build_id) = decode_build_id(prefix, suffix) else {
                return Ok(());
            };
            let identifier_path = identifier.path();
            let metadata = std::fs::symlink_metadata(&identifier_path).map_err(|source| {
                io_error(
                    "inspect staged GSYM identifier directory",
                    &identifier_path,
                    source,
                )
            })?;
            if !metadata.is_dir() {
                return Ok(());
            }
            let Some(_entry_lock) = try_entry_lock(cache, &build_id)? else {
                skipped_busy = skipped_busy.saturating_add(1);
                return Ok(());
            };
            let files = std::fs::read_dir(&identifier_path).map_err(|source| {
                io_error("scan staged GSYM identifier", &identifier_path, source)
            })?;
            for file in files {
                let file = file.map_err(|source| {
                    io_error("read staged GSYM identifier", &identifier_path, source)
                })?;
                let path = file.path();
                let metadata = std::fs::symlink_metadata(&path)
                    .map_err(|source| io_error("inspect staged GSYM file", &path, source))?;
                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let stale = now
                    .duration_since(modified)
                    .map_or(true, |age| age >= STALE_TEMPORARY_AGE);
                if !metadata.is_file() || !stale {
                    continue;
                }
                if remove_file_if_exists("remove staged GSYM file", &path)? {
                    removed = removed.saturating_add(1);
                }
            }
            drop(std::fs::remove_dir(&identifier_path));
            Ok(())
        },
    )?;
    Ok((removed, skipped_busy))
}

fn try_maintenance_lock(cache: &Cache) -> Result<Option<File>> {
    let path = cache.base().join("gc.lock");
    try_lock_file(cache, &path, "cache maintenance")
}

fn try_entry_lock(cache: &Cache, build_id: &BuildId) -> Result<Option<File>> {
    let path = layout::lock(cache.base(), build_id);
    try_lock_file(cache, &path, "cache entry")
}

fn prune_batch_size(
    before: CacheStats,
    target_bytes: Option<u64>,
    target_entries: Option<u64>,
) -> usize {
    let byte_estimate = target_bytes.map_or(0, |target| {
        let excess = before.bytes.saturating_sub(target);
        if excess == 0 || before.bytes == 0 {
            return 0;
        }
        let scaled = u128::from(excess).saturating_mul(u128::from(before.entries));
        let total = u128::from(before.bytes);
        let quotient = scaled.checked_div(total).unwrap_or_default();
        let has_remainder = scaled.checked_rem(total).is_some_and(|value| value != 0);
        let estimate = quotient.saturating_add(u128::from(has_remainder));
        u64::try_from(estimate).unwrap_or(before.entries)
    });
    let entry_estimate = target_entries.map_or(0, |target| before.entries.saturating_sub(target));
    let desired = byte_estimate
        .max(entry_estimate)
        .max(MIN_PRUNE_BATCH_SIZE as u64);
    usize::try_from(desired.min(MAX_PRUNE_BATCH_SIZE as u64)).unwrap_or(MAX_PRUNE_BATCH_SIZE)
}

const fn low_watermark(high: u64) -> u64 {
    let quotient = high / 5;
    let remainder = high % 5;
    quotient
        .saturating_mul(4)
        .saturating_add(remainder.saturating_mul(4) / 5)
}

#[cfg(test)]
mod tests {
    use super::low_watermark;

    #[test]
    fn low_watermark_does_not_saturate_before_division() {
        for high in [1, 4, 5, 6, u64::MAX] {
            let expected =
                u64::try_from(u128::from(high) * 4 / 5).expect("80% of a u64 fits in a u64");
            assert_eq!(low_watermark(high), expected);
        }
    }
}
