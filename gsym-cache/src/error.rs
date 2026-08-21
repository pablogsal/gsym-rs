use std::io;
#[cfg(feature = "manage")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(feature = "manage")]
use std::time::{Duration, SystemTime};

#[cfg(feature = "manage")]
use crate::BuildId;

/// Result type used by this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Cache operation failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A filesystem operation failed.
    #[error("failed to {operation} at {path}: {source}")]
    Io {
        /// Operation that failed.
        operation: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },

    /// A cache directory is missing, untrusted, or unsafe to traverse.
    #[cfg(feature = "lookup")]
    #[error("cache directory is not secure: {path}")]
    InsecureDirectory {
        /// Rejected directory.
        path: PathBuf,
    },

    /// A cache entry is not an owned regular file.
    #[cfg(feature = "lookup")]
    #[error("cache entry is not a trusted regular file: {path}")]
    UntrustedEntry {
        /// Rejected entry.
        path: PathBuf,
    },

    /// No absolute XDG cache directory or home directory is available.
    #[cfg(feature = "lookup")]
    #[error("XDG cache home is unavailable")]
    CacheHomeUnavailable,

    /// An XDG application identifier is not one normal path component.
    #[cfg(feature = "lookup")]
    #[error("invalid XDG cache application identifier: {value}")]
    InvalidApplicationId {
        /// Rejected identifier.
        value: PathBuf,
    },

    /// A cache file is not valid GSYM.
    #[cfg(feature = "manage")]
    #[error(transparent)]
    InvalidGsym(Box<InvalidGsymError>),

    /// The GSYM build identifier differs from its cache key.
    #[cfg(feature = "manage")]
    #[error(transparent)]
    BuildIdMismatch(Box<BuildIdMismatchError>),

    /// A negative-cache lifetime is zero or exceeds the maximum.
    #[cfg(feature = "manage")]
    #[error(
        "negative-cache lifetime {lifetime:?} must be greater than zero and at most {maximum:?}"
    )]
    InvalidFailureTtl {
        /// Rejected cache lifetime.
        lifetime: Duration,
        /// Maximum accepted lifetime.
        maximum: Duration,
    },

    /// A negative-cache expiration cannot be represented by the system clock.
    #[cfg(feature = "manage")]
    #[error("negative-cache expiration cannot be represented from {now:?} with {lifetime:?}")]
    FailureExpirationUnrepresentable {
        /// Clock value used to calculate the expiration.
        now: SystemTime,
        /// Requested cache lifetime.
        lifetime: Duration,
    },
}

/// Invalid GSYM file with its cache path.
#[cfg(feature = "manage")]
#[derive(Debug, thiserror::Error)]
#[error("invalid GSYM file at {}: {source}", path.display())]
pub struct InvalidGsymError {
    path: PathBuf,
    source: gsym::Error,
}

#[cfg(feature = "manage")]
impl InvalidGsymError {
    pub(crate) fn new(path: &Path, source: gsym::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Returns the rejected file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the GSYM parsing or verification error.
    #[must_use]
    pub const fn gsym_error(&self) -> &gsym::Error {
        &self.source
    }
}

#[cfg(feature = "manage")]
const BUILD_ID_PREFIX_LEN: usize = 32;

/// GSYM build-identifier mismatch with bounded diagnostic data.
#[cfg(feature = "manage")]
#[derive(Debug, thiserror::Error)]
#[error(
    "GSYM build identifier prefix {} ({actual_len} bytes) at {} does not match cache key {expected}",
    HexBytes(actual_prefix, actual_len),
    path.display()
)]
pub struct BuildIdMismatchError {
    path: PathBuf,
    expected: BuildId,
    actual_len: usize,
    actual_prefix: [u8; BUILD_ID_PREFIX_LEN],
}

#[cfg(feature = "manage")]
impl BuildIdMismatchError {
    pub(crate) fn new(path: &Path, expected: BuildId, actual: &[u8]) -> Self {
        let mut actual_prefix = [0; BUILD_ID_PREFIX_LEN];
        let prefix_len = actual.len().min(BUILD_ID_PREFIX_LEN);
        if let (Some(destination), Some(source)) = (
            actual_prefix.get_mut(..prefix_len),
            actual.get(..prefix_len),
        ) {
            destination.copy_from_slice(source);
        }
        Self {
            path: path.to_path_buf(),
            expected,
            actual_len: actual.len(),
            actual_prefix,
        }
    }

    /// Returns the rejected file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the requested cache key.
    #[must_use]
    pub const fn expected(&self) -> &BuildId {
        &self.expected
    }

    /// Returns the total GSYM build-identifier length.
    #[must_use]
    pub const fn actual_len(&self) -> usize {
        self.actual_len
    }

    /// Returns at most the first 32 bytes of the GSYM build identifier.
    #[must_use]
    pub fn actual_prefix(&self) -> &[u8] {
        self.actual_prefix
            .get(..self.actual_len.min(BUILD_ID_PREFIX_LEN))
            .unwrap_or_default()
    }
}

#[cfg(feature = "manage")]
struct HexBytes<'bytes>(&'bytes [u8; BUILD_ID_PREFIX_LEN], &'bytes usize);

#[cfg(feature = "manage")]
impl std::fmt::Display for HexBytes<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self
            .0
            .get(..(*self.1).min(BUILD_ID_PREFIX_LEN))
            .unwrap_or_default()
        {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "manage"))]
mod tests {
    use super::Error;
    use std::mem::size_of;

    #[test]
    fn cold_diagnostics_do_not_inflate_the_cache_result() {
        assert!(size_of::<Error>() <= 48);
    }
}

#[cfg(feature = "lookup")]
pub(crate) fn io_error(
    operation: &'static str,
    path: impl Into<PathBuf>,
    source: io::Error,
) -> Error {
    Error::Io {
        operation,
        path: path.into(),
        source,
    }
}
