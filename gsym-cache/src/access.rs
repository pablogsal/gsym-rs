use std::fs::{File, FileTimes};
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::error::io_error;
use crate::{BuildId, Cache, Result, layout};

const ACCESS_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[cfg_attr(docsrs, doc(cfg(feature = "access")))]
impl Cache {
    /// Records a cache hit without changing the immutable GSYM file's mtime.
    ///
    /// Marker updates are limited to once per hour to avoid write traffic on
    /// repeated hits. Callers should invoke this only after a successful
    /// [`Cache::lookup`]; recording a missing build ID creates an orphan marker
    /// that a later scrub pass will remove.
    ///
    /// # Errors
    ///
    /// Returns an error when the marker cannot be inspected or updated.
    pub fn record_access(&self, build_id: &BuildId) -> Result<AccessUpdate> {
        let directory = self.prepare_access()?;
        let key = layout::access_key(build_id);
        let key_path = key.as_c_str().ok_or_else(|| {
            io_error(
                "encode access marker path",
                layout::access(self.base(), build_id),
                io::Error::from(io::ErrorKind::InvalidInput),
            )
        })?;
        let now = SystemTime::now();
        match rustix::fs::statat(directory, key_path, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => {
                if rustix::fs::FileType::from_raw_mode(metadata.st_mode)
                    != rustix::fs::FileType::RegularFile
                    || !self.owns_uid(metadata.st_uid)?
                {
                    return Err(crate::Error::UntrustedEntry {
                        path: layout::access(self.base(), build_id),
                    });
                }
                if marker_is_recent(metadata.st_mtime, metadata.st_mtime_nsec, now) {
                    return Ok(AccessUpdate::Debounced);
                }
            }
            Err(source) if source == rustix::io::Errno::NOENT => {}
            Err(source) => {
                return Err(io_error(
                    "inspect access marker",
                    layout::access(self.base(), build_id),
                    io::Error::from(source),
                ));
            }
        }
        let (file, created) = open_marker(self, directory, key_path, build_id)?;
        if created {
            return Ok(AccessUpdate::Recorded);
        }
        file.set_times(FileTimes::new().set_modified(now))
            .map_err(|source| {
                io_error(
                    "update access marker",
                    layout::access(self.base(), build_id),
                    source,
                )
            })?;
        Ok(AccessUpdate::Recorded)
    }

    pub(crate) fn prepare_access(&self) -> Result<&File> {
        if let Some(directory) = self.access_directory.get() {
            return Ok(directory);
        }
        if let Some((cache_home, application)) = self.xdg_paths() {
            crate::lookup::ensure_xdg_anchor(cache_home)?;
            crate::lookup::ensure_xdg_anchor(application)?;
        }
        crate::lookup::ensure_private_directory(self.root())?;
        let _ = self.ensure_identity()?;
        crate::lookup::ensure_private_directory(self.base())?;
        let path = self.base().join(layout::ACCESS).join(layout::BUILD_ID);
        let directory = crate::lookup::open_private_directory(&path)?;
        drop(self.access_directory.set(directory));
        self.access_directory
            .get()
            .ok_or_else(|| crate::Error::InsecureDirectory {
                path: self.base().join(layout::ACCESS).join(layout::BUILD_ID),
            })
    }
}

/// Whether access-marker write traffic was required.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
#[cfg_attr(docsrs, doc(cfg(feature = "access")))]
pub enum AccessUpdate {
    /// The marker timestamp was updated.
    Recorded,
    /// A recent marker made an update unnecessary.
    Debounced,
}

pub(crate) fn ensure_parent(path: &Path) -> Result<(&Path, File)> {
    let parent = path.parent().ok_or_else(|| {
        io_error(
            "resolve cache entry parent",
            path,
            io::Error::from(io::ErrorKind::InvalidInput),
        )
    })?;
    let directory = crate::lookup::open_private_directory(parent)?;
    Ok((parent, directory))
}

fn open_marker(
    cache: &Cache,
    directory: &File,
    key: &std::ffi::CStr,
    build_id: &BuildId,
) -> Result<(File, bool)> {
    use rustix::fs::{Mode, OFlags};

    let mut repaired_parent = false;
    loop {
        match rustix::fs::openat(
            directory,
            key,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(file) => {
                let file = File::from(file);
                let path = layout::access(cache.base(), build_id);
                cache.validate_file(&path, &file)?;
                return Ok((file, false));
            }
            Err(source) if source == rustix::io::Errno::NOENT => {
                match rustix::fs::openat(
                    directory,
                    key,
                    OFlags::RDONLY
                        | OFlags::CLOEXEC
                        | OFlags::NOFOLLOW
                        | OFlags::NONBLOCK
                        | OFlags::CREATE
                        | OFlags::EXCL,
                    Mode::RUSR | Mode::WUSR,
                ) {
                    Ok(file) => return Ok((File::from(file), true)),
                    Err(source) if source == rustix::io::Errno::EXIST => {}
                    Err(source) if source == rustix::io::Errno::NOENT && !repaired_parent => {
                        let path = layout::access(cache.base(), build_id);
                        let _parent = ensure_parent(&path)?;
                        repaired_parent = true;
                    }
                    Err(source) => {
                        return Err(io_error(
                            "create access marker",
                            layout::access(cache.base(), build_id),
                            io::Error::from(source),
                        ));
                    }
                }
            }
            Err(source) => {
                return Err(io_error(
                    "open access marker",
                    layout::access(cache.base(), build_id),
                    io::Error::from(source),
                ));
            }
        }
    }
}

fn marker_is_recent<S, N>(seconds: S, nanoseconds: N, now: SystemTime) -> bool
where
    S: TryInto<u64>,
    N: TryInto<u32>,
{
    let Some(modified) = system_time_from_unix(seconds, nanoseconds) else {
        return false;
    };
    now.duration_since(modified)
        .is_ok_and(|age| age < ACCESS_INTERVAL)
}

pub(crate) fn system_time_from_unix<S, N>(seconds: S, nanoseconds: N) -> Option<SystemTime>
where
    S: TryInto<u64>,
    N: TryInto<u32>,
{
    let Ok(seconds) = seconds.try_into() else {
        return None;
    };
    let Ok(nanoseconds) = nanoseconds.try_into() else {
        return None;
    };
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(seconds, nanoseconds))
}
