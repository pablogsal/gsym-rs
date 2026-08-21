use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::NamedTempFile;

use crate::access::ensure_parent;
use crate::error::io_error;
use crate::lookup::open_read_only;
use crate::{
    BuildId, BuildIdMismatchError, Cache, CacheEntry, Error, InvalidGsymError, Result, layout,
};

const FAILURE_RECORD_LEN: usize = 16;
const FAILURE_MAGIC: [u8; 4] = *b"GSNF";
const FAILURE_VERSION: u8 = 1;
/// Longest accepted negative-cache lifetime.
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub const MAX_FAILURE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
impl Cache {
    /// Tries to become the sole population owner for a build identifier.
    ///
    /// This never waits for another process. After acquiring the advisory
    /// lock it rechecks the cache, closing the race between lookup and lock. An
    /// existing entry is fully verified before it is returned as
    /// [`PopulationOutcome::Present`]. Confirmed corruption is removed only
    /// while holding the population lock; transient I/O failures never cause
    /// deletion.
    ///
    /// See [`docs::operations`](crate::docs::operations) for the complete state
    /// machine and [`docs::cookbook`](crate::docs::cookbook) for recipes.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache directories or lock file cannot be
    /// created, inspected, or locked.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::fs::File;
    /// use std::io;
    /// use gsym_cache::{BuildId, Cache, CacheEpoch, PopulationOutcome};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let cache = Cache::open("/var/cache/my-profiler/gsym", CacheEpoch::new(1))?;
    /// let build_id = BuildId::new([0x12; 20])?;
    ///
    /// match cache.try_begin_population(&build_id)? {
    ///     PopulationOutcome::Present(entry) => drop(entry),
    ///     PopulationOutcome::Acquired(population) => {
    ///         let mut source = File::open("artifact.gsym")?;
    ///         let mut writer = population.into_writer()?;
    ///         io::copy(&mut source, &mut writer)?;
    ///         let entry = writer.publish()?.into_entry();
    ///         drop(entry);
    ///     }
    ///     PopulationOutcome::Suppressed(failure) => {
    ///         eprintln!("retry after {:?}", failure.expires_at());
    ///     }
    ///     PopulationOutcome::Busy => eprintln!("another population is active"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_begin_population<'cache>(
        &'cache self,
        build_id: &'cache BuildId,
    ) -> Result<PopulationOutcome<'cache>> {
        self.prepare()?;
        let object_path = layout::object(self.base(), build_id);
        let corrupt = match inspect_existing(self, build_id, &object_path)? {
            ExistingEntry::Missing => false,
            ExistingEntry::Valid(entry) => return Ok(PopulationOutcome::Present(entry)),
            ExistingEntry::Corrupt => true,
        };
        if !corrupt && let FailureRecord::Cached(failure) = self.failure_record(build_id)? {
            return Ok(PopulationOutcome::Suppressed(failure));
        }

        let path = layout::lock(self.base(), build_id);
        let Some(lock) = try_lock_file(self, &path, "lock cache population")? else {
            return Ok(PopulationOutcome::Busy);
        };
        match inspect_existing(self, build_id, &object_path)? {
            ExistingEntry::Missing => {}
            ExistingEntry::Valid(entry) => return Ok(PopulationOutcome::Present(entry)),
            ExistingEntry::Corrupt => self.remove_corrupt_entry(build_id, &object_path)?,
        }
        match self.failure_record(build_id)? {
            FailureRecord::Cached(failure) => return Ok(PopulationOutcome::Suppressed(failure)),
            FailureRecord::Stale => self.clear_failure(build_id),
            FailureRecord::Missing => {}
        }
        Ok(PopulationOutcome::Acquired(Population {
            ownership: PopulationLock {
                cache: self,
                build_id,
                _lock: lock,
            },
        }))
    }

    fn record_failure(
        &self,
        build_id: &BuildId,
        kind: FailureKind,
        lifetime: Duration,
    ) -> Result<CachedFailure> {
        let (expires, expires_at) = failure_expiration(lifetime, SystemTime::now())?;
        let path = layout::negative(self.base(), build_id);
        let (parent, parent_directory) = ensure_parent(&path)?;
        let mut temporary = NamedTempFile::new_in(parent)
            .map_err(|source| io_error("create negative-cache temporary file", parent, source))?;
        let [magic_0, magic_1, magic_2, magic_3] = FAILURE_MAGIC;
        let [
            expires_0,
            expires_1,
            expires_2,
            expires_3,
            expires_4,
            expires_5,
            expires_6,
            expires_7,
        ] = expires.to_le_bytes();
        let record = [
            magic_0,
            magic_1,
            magic_2,
            magic_3,
            FAILURE_VERSION,
            kind.code(),
            0,
            0,
            expires_0,
            expires_1,
            expires_2,
            expires_3,
            expires_4,
            expires_5,
            expires_6,
            expires_7,
        ];
        temporary
            .write_all(&record)
            .map_err(|source| io_error("write negative-cache record", &path, source))?;
        set_read_only(temporary.as_file(), &path)?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|source| io_error("sync negative-cache record", &path, source))?;
        temporary
            .persist(&path)
            .map_err(|error| io_error("publish negative-cache record", &path, error.error))?;
        sync_open_directory(&parent_directory, parent)?;
        Ok(CachedFailure { kind, expires_at })
    }

    fn clear_failure(&self, build_id: &BuildId) {
        drop(std::fs::remove_file(layout::negative(
            self.base(),
            build_id,
        )));
    }

    pub(crate) fn clear_auxiliary(&self, build_id: &BuildId) {
        drop(std::fs::remove_file(layout::access(self.base(), build_id)));
        self.clear_failure(build_id);
    }

    fn remove_corrupt_entry(&self, build_id: &BuildId, path: &Path) -> Result<()> {
        let removed = remove_file_if_exists("remove corrupt cache entry", path)?;
        self.clear_auxiliary(build_id);
        if removed && let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }

    /// Returns an unexpired cached failure.
    ///
    /// Expired or malformed records are ignored. Population or a scrub pass
    /// removes them while holding the entry lock.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem failures or an untrusted cache entry.
    pub fn cached_failure(&self, build_id: &BuildId) -> Result<Option<CachedFailure>> {
        if self.lookup(build_id)?.is_some() {
            return Ok(None);
        }
        match self.failure_record(build_id)? {
            FailureRecord::Cached(failure) => Ok(Some(failure)),
            FailureRecord::Missing | FailureRecord::Stale => Ok(None),
        }
    }

    pub(crate) fn failure_record(&self, build_id: &BuildId) -> Result<FailureRecord> {
        let path = layout::negative(self.base(), build_id);
        let mut file = match open_read_only(&path) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(FailureRecord::Missing);
            }
            Err(source) => return Err(io_error("open negative-cache record", path, source)),
        };
        let metadata = file
            .metadata()
            .map_err(|source| io_error("inspect negative-cache record", &path, source))?;
        self.validate_entry(&path, &metadata)?;
        if metadata.len() != FAILURE_RECORD_LEN as u64 {
            return Ok(FailureRecord::Stale);
        }
        let mut record = [0_u8; FAILURE_RECORD_LEN];
        file.read_exact(&mut record)
            .map_err(|source| io_error("read negative-cache record", &path, source))?;
        let [
            magic_0,
            magic_1,
            magic_2,
            magic_3,
            version,
            kind,
            reserved_0,
            reserved_1,
            expires_0,
            expires_1,
            expires_2,
            expires_3,
            expires_4,
            expires_5,
            expires_6,
            expires_7,
        ] = record;
        let Some(kind) = FailureKind::from_code(kind) else {
            return Ok(FailureRecord::Stale);
        };
        let Some(expires_at) = UNIX_EPOCH.checked_add(Duration::from_secs(u64::from_le_bytes([
            expires_0, expires_1, expires_2, expires_3, expires_4, expires_5, expires_6, expires_7,
        ]))) else {
            return Ok(FailureRecord::Stale);
        };
        if [magic_0, magic_1, magic_2, magic_3] != FAILURE_MAGIC
            || version != FAILURE_VERSION
            || reserved_0 != 0
            || reserved_1 != 0
        {
            return Ok(FailureRecord::Stale);
        }
        let now = SystemTime::now();
        if expires_at <= now
            || expires_at
                .duration_since(now)
                .is_ok_and(|ttl| ttl >= MAX_FAILURE_TTL.saturating_add(Duration::from_secs(1)))
        {
            return Ok(FailureRecord::Stale);
        }
        Ok(FailureRecord::Cached(CachedFailure { kind, expires_at }))
    }

    pub(crate) fn prepare(&self) -> Result<()> {
        if self.manage_prepared.load(Ordering::Acquire) {
            return Ok(());
        }
        self.prepare_access()?;
        for path in [
            self.base().join(layout::OBJECTS).join(layout::BUILD_ID),
            self.base().join(layout::LOCKS),
            self.base().join(layout::NEGATIVE).join(layout::BUILD_ID),
            self.base().join(layout::TEMPORARY).join(layout::BUILD_ID),
        ] {
            crate::lookup::ensure_private_directory(&path)?;
        }
        self.manage_prepared.store(true, Ordering::Release);
        Ok(())
    }
}

fn failure_expiration(lifetime: Duration, now: SystemTime) -> Result<(u64, SystemTime)> {
    if lifetime.is_zero() || lifetime > MAX_FAILURE_TTL {
        return Err(Error::InvalidFailureTtl {
            lifetime,
            maximum: MAX_FAILURE_TTL,
        });
    }
    let expires_at = now
        .checked_add(lifetime)
        .ok_or(Error::FailureExpirationUnrepresentable { now, lifetime })?;
    let epoch_duration = expires_at
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::FailureExpirationUnrepresentable { now, lifetime })?;
    let expires = epoch_duration
        .as_secs()
        .checked_add(u64::from(epoch_duration.subsec_nanos() != 0))
        .ok_or(Error::FailureExpirationUnrepresentable { now, lifetime })?;
    let rounded = UNIX_EPOCH
        .checked_add(Duration::from_secs(expires))
        .ok_or(Error::FailureExpirationUnrepresentable { now, lifetime })?;
    Ok((expires, rounded))
}

#[cfg(test)]
mod tests {
    use super::{MAX_FAILURE_TTL, failure_expiration};
    use crate::Error;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn failure_expiration_rounds_up_without_accepting_invalid_lifetimes() {
        let now = UNIX_EPOCH + Duration::from_secs(10);
        assert!(matches!(
            failure_expiration(Duration::from_millis(1), now),
            Ok((11, expiration)) if expiration == UNIX_EPOCH + Duration::from_secs(11)
        ));
        assert!(matches!(
            failure_expiration(Duration::ZERO, now),
            Err(Error::InvalidFailureTtl { .. })
        ));
        assert!(matches!(
            failure_expiration(MAX_FAILURE_TTL + Duration::from_secs(1), now),
            Err(Error::InvalidFailureTtl { .. })
        ));
    }
}

/// Outcome of trying to acquire population ownership.
#[derive(Debug)]
#[must_use]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub enum PopulationOutcome<'cache> {
    /// The caller owns population for this build identifier.
    Acquired(Population<'cache>),
    /// Another process currently owns the same bounded lock slot.
    ///
    /// Lock-slot collisions may delay an unrelated build ID.
    Busy,
    /// A cached failure suppresses population until its expiration.
    Suppressed(CachedFailure),
    /// Population completed before the lock was needed or acquired.
    Present(CacheEntry),
}

/// Exclusive population capability for one build identifier.
///
/// Dropping the guard releases its process lock. A staging file is not created
/// until [`Population::into_writer`] consumes this value.
#[must_use = "record a failure, create a writer, or drop it to abandon population"]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub struct Population<'cache> {
    ownership: PopulationLock<'cache>,
}

struct PopulationLock<'cache> {
    cache: &'cache Cache,
    build_id: &'cache BuildId,
    _lock: File,
}

impl<'cache> Population<'cache> {
    /// Creates a staged GSYM file and transitions to its write-only owner.
    ///
    /// The returned writer owns this population capability. Writers can stream
    /// directly into it without a second full-size allocation or copy.
    ///
    /// # Errors
    ///
    /// Returns an error if the temporary file cannot be created.
    pub fn into_writer(self) -> Result<PopulationWriter<'cache>> {
        let directory = layout::temporary(self.ownership.cache.base(), self.ownership.build_id);
        crate::lookup::ensure_private_directory(&directory)?;
        let temporary = match NamedTempFile::new_in(&directory) {
            Ok(temporary) => temporary,
            Err(source) => {
                drop(std::fs::remove_dir(&directory));
                return Err(io_error("create staged GSYM file", directory, source));
            }
        };
        Ok(PopulationWriter {
            temporary,
            _directory: StagingDirectory(directory),
            ownership: self.ownership,
        })
    }

    /// Atomically records an expiring population failure.
    ///
    /// Consuming the population guard ensures no successful publisher for the
    /// same build identifier races this record. Applications should use a
    /// short lifetime for missing inputs and transient resource failures.
    /// The lifetime is rounded up to whole Unix seconds and must be nonzero and
    /// at most [`MAX_FAILURE_TTL`].
    ///
    /// Returns the cached failure, including its rounded expiration time.
    ///
    /// # Errors
    ///
    /// Returns an error if `lifetime` is zero, exceeds [`MAX_FAILURE_TTL`],
    /// cannot be represented, or the record cannot be published.
    pub fn record_failure_for(
        self,
        kind: FailureKind,
        lifetime: Duration,
    ) -> Result<CachedFailure> {
        self.ownership
            .cache
            .record_failure(self.ownership.build_id, kind, lifetime)
    }
}

impl std::fmt::Debug for Population<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Population")
            .field("build_id", self.ownership.build_id)
            .finish_non_exhaustive()
    }
}

/// Exclusive write-only owner of a staged GSYM file.
///
/// Dropping the writer removes the unpublished temporary file and releases the
/// population lock.
#[must_use = "publish the staged GSYM, record a failure, or drop it to abandon population"]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub struct PopulationWriter<'cache> {
    temporary: NamedTempFile,
    _directory: StagingDirectory,
    ownership: PopulationLock<'cache>,
}

struct StagingDirectory(std::path::PathBuf);

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        drop(std::fs::remove_dir(&self.0));
    }
}

impl PopulationWriter<'_> {
    /// Atomically records an expiring population failure and discards staged output.
    ///
    /// # Errors
    ///
    /// Returns an error if `lifetime` is zero, exceeds [`MAX_FAILURE_TTL`],
    /// cannot be represented, or the record cannot be published.
    pub fn record_failure_for(
        self,
        kind: FailureKind,
        lifetime: Duration,
    ) -> Result<CachedFailure> {
        let Self {
            temporary,
            _directory: directory,
            ownership,
        } = self;
        drop(temporary);
        drop(directory);
        ownership
            .cache
            .record_failure(ownership.build_id, kind, lifetime)
    }

    /// Verifies and atomically publishes the staged GSYM file.
    ///
    /// Publication consumes the writer. A racing winner is validated and
    /// returned instead of being replaced.
    ///
    /// # Errors
    ///
    /// Returns an error if verification fails, the GSYM build identifier
    /// differs from the cache key, or durable publication fails.
    pub fn publish(self) -> Result<PublishOutcome> {
        verify_file(
            self.temporary.as_file(),
            self.ownership.build_id,
            self.temporary.path(),
        )?;
        set_read_only(self.temporary.as_file(), self.temporary.path())?;
        self.temporary.as_file().sync_all().map_err(|source| {
            io_error(
                "sync staged GSYM metadata",
                self.temporary.path().to_path_buf(),
                source,
            )
        })?;

        publish_noclobber(
            self.ownership.cache,
            self.ownership.build_id,
            self.temporary,
        )
    }
}

impl Write for PopulationWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.temporary.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.temporary.flush()
    }
}

impl std::fmt::Debug for PopulationWriter<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PopulationWriter")
            .field("build_id", self.ownership.build_id)
            .finish_non_exhaustive()
    }
}

/// Result of publishing a verified staged file.
#[derive(Debug)]
#[must_use]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub enum PublishOutcome {
    /// This process published the entry.
    Published(CacheEntry),
    /// Another process had already published an equivalent valid entry.
    Existing(CacheEntry),
}

impl PublishOutcome {
    /// Borrows the published or concurrently existing cache entry.
    #[must_use]
    pub const fn entry(&self) -> &CacheEntry {
        match self {
            Self::Published(entry) | Self::Existing(entry) => entry,
        }
    }

    /// Consumes the outcome and returns its cache entry.
    #[must_use]
    pub fn into_entry(self) -> CacheEntry {
        match self {
            Self::Published(entry) | Self::Existing(entry) => entry,
        }
    }

    /// Returns whether this process published the entry.
    #[must_use]
    pub const fn is_published(&self) -> bool {
        matches!(self, Self::Published(_))
    }
}

/// Stable class of a population failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub enum FailureKind {
    /// Required executable or debug information is not currently available.
    MissingInput,
    /// The input format or conversion mode is unsupported.
    UnsupportedInput,
    /// The input is malformed.
    MalformedInput,
    /// A temporary I/O failure occurred.
    TransientIo,
    /// Conversion exceeded a resource limit.
    ResourceExhausted,
}

impl FailureKind {
    const fn code(self) -> u8 {
        match self {
            Self::MissingInput => 1,
            Self::UnsupportedInput => 2,
            Self::MalformedInput => 3,
            Self::TransientIo => 4,
            Self::ResourceExhausted => 5,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::MissingInput),
            2 => Some(Self::UnsupportedInput),
            3 => Some(Self::MalformedInput),
            4 => Some(Self::TransientIo),
            5 => Some(Self::ResourceExhausted),
            _ => None,
        }
    }
}

/// An unexpired persistent population failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(docsrs, doc(cfg(feature = "manage")))]
pub struct CachedFailure {
    kind: FailureKind,
    expires_at: SystemTime,
}

pub(crate) enum FailureRecord {
    Missing,
    Stale,
    Cached(CachedFailure),
}

impl CachedFailure {
    /// Returns the stable failure class.
    #[must_use]
    pub const fn kind(self) -> FailureKind {
        self.kind
    }

    /// Returns when this failure should be retried.
    #[must_use]
    pub const fn expires_at(self) -> SystemTime {
        self.expires_at
    }
}

#[expect(
    unsafe_code,
    reason = "the consumed population guard owns the only writable handle while verification is mapped"
)]
pub(crate) fn verify_file(file: &File, build_id: &BuildId, path: &Path) -> Result<()> {
    let invalid_gsym = |source| Error::InvalidGsym(Box::new(InvalidGsymError::new(path, source)));
    // SAFETY: population exposes only a Write facade, publish consumes it, and
    // cache entries are immutable after publication.
    let gsym = unsafe { gsym::MappedGsym::map_file(file) }.map_err(&invalid_gsym)?;
    let actual = gsym.build_id();
    if actual != build_id.as_bytes() {
        return Err(Error::BuildIdMismatch(Box::new(BuildIdMismatchError::new(
            path,
            build_id.clone(),
            actual,
        ))));
    }
    gsym.verify().map_err(invalid_gsym)?;
    Ok(())
}

enum ExistingEntry {
    Missing,
    Valid(CacheEntry),
    Corrupt,
}

fn inspect_existing(cache: &Cache, build_id: &BuildId, path: &Path) -> Result<ExistingEntry> {
    let Some(entry) = cache.lookup_path(path)? else {
        return Ok(ExistingEntry::Missing);
    };
    match verify_file(entry.file(), build_id, path) {
        Ok(()) => Ok(ExistingEntry::Valid(entry)),
        Err(error) if is_corrupt(&error) => Ok(ExistingEntry::Corrupt),
        Err(error) => Err(error),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "future error classes must not cause cache deletion"
)]
pub(crate) const fn is_corrupt(error: &Error) -> bool {
    match error {
        Error::InvalidGsym(error)
            if matches!(
                error.gsym_error(),
                gsym::Error::Io(_) | gsym::Error::IoAtPath { .. }
            ) =>
        {
            false
        }
        Error::BuildIdMismatch(_) | Error::InvalidGsym(_) => true,
        _ => false,
    }
}

fn publish_noclobber(
    cache: &Cache,
    build_id: &BuildId,
    mut temporary: NamedTempFile,
) -> Result<PublishOutcome> {
    let path = layout::object(cache.base(), build_id);
    let (parent, parent_directory) = ensure_parent(&path)?;
    let len = temporary
        .as_file()
        .metadata()
        .map_err(|source| io_error("inspect staged GSYM file", temporary.path(), source))?
        .len();
    loop {
        match temporary.persist_noclobber(&path) {
            Ok(file) => {
                sync_open_directory(&parent_directory, parent)?;
                drop(file);
                let entry = cache.lookup(build_id)?.ok_or_else(|| {
                    io_error(
                        "reopen published GSYM file",
                        &path,
                        io::Error::from(io::ErrorKind::NotFound),
                    )
                })?;
                debug_assert_eq!(entry.len(), len);
                return Ok(PublishOutcome::Published(entry));
            }
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                temporary = error.file;
                match inspect_existing(cache, build_id, &path)? {
                    ExistingEntry::Missing => {}
                    ExistingEntry::Valid(entry) => {
                        drop(temporary);
                        return Ok(PublishOutcome::Existing(entry));
                    }
                    ExistingEntry::Corrupt => cache.remove_corrupt_entry(build_id, &path)?,
                }
            }
            Err(error) => return Err(io_error("publish GSYM file", path, error.error)),
        }
    }
}

fn set_read_only(file: &File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    file.set_permissions(std::fs::Permissions::from_mode(0o400))
        .map_err(|source| io_error("set cache file permissions", path, source))?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    sync_open_directory(&crate::lookup::open_existing_directory(path)?, path)
}

fn sync_open_directory(directory: &File, path: &Path) -> Result<()> {
    directory
        .sync_all()
        .map_err(|source| io_error("sync cache directory", path, source))
}

pub(crate) fn open_lock(cache: &Cache, path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};

    loop {
        match rustix::fs::open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CREATE,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(file) => {
                let file = File::from(file);
                cache.validate_file(path, &file)?;
                return Ok(file);
            }
            Err(source) if source == rustix::io::Errno::NOENT => {
                let _parent = ensure_parent(path)?;
            }
            Err(source) => {
                return Err(io_error("open cache lock", path, io::Error::from(source)));
            }
        }
    }
}

pub(crate) fn remove_file_if_exists(operation: &'static str, path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(operation, path, source)),
    }
}

pub(crate) fn try_lock_file(
    cache: &Cache,
    path: &Path,
    description: &'static str,
) -> Result<Option<File>> {
    let file = open_lock(cache, path)?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(source) if source == rustix::io::Errno::WOULDBLOCK => Ok(None),
        Err(source) => Err(io_error(description, path, io::Error::from(source))),
    }
}
