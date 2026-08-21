use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{File, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
#[cfg(feature = "manage")]
use std::sync::atomic::AtomicBool;

use crate::error::io_error;
use crate::{BuildId, Error, Result, layout};

/// Version of the conversion policy used to create cached GSYM files.
///
/// Incrementing the epoch creates a namespace without reusing artifacts from
/// an older conversion policy.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(docsrs, doc(cfg(feature = "lookup")))]
pub struct CacheEpoch(u32);

impl CacheEpoch {
    /// Creates an epoch from its stable numeric value.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric epoch.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl From<u32> for CacheEpoch {
    fn from(value: u32) -> Self {
        Self::new(value)
    }
}

impl From<CacheEpoch> for u32 {
    fn from(epoch: CacheEpoch) -> Self {
        epoch.get()
    }
}

impl fmt::Display for CacheEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A configured GSYM filesystem cache.
///
/// Directory descriptors are pinned once opened. Create a new `Cache` after
/// replacing a cache namespace directory; ordinary entry creation and removal
/// do not require reopening it.
///
/// See [`docs::deployment`](crate::docs::deployment) for root selection,
/// epochs, filesystem requirements, and the trust model.
#[cfg_attr(docsrs, doc(cfg(feature = "lookup")))]
pub struct Cache {
    root: PathBuf,
    base: PathBuf,
    object_directory_path: PathBuf,
    epoch: CacheEpoch,
    identity: OnceLock<RootIdentity>,
    object_directory: OnceLock<File>,
    #[cfg(feature = "access")]
    is_xdg: bool,
    #[cfg(feature = "access")]
    pub(crate) access_directory: OnceLock<File>,
    #[cfg(feature = "manage")]
    pub(crate) manage_prepared: AtomicBool,
}

impl Cache {
    /// Opens a cache namespace without creating it.
    ///
    /// A missing root is valid and produces cache misses. An existing root
    /// must be a private non-symlink directory owned by the effective user.
    ///
    /// # Errors
    ///
    /// Returns an error if the existing root is insecure or cannot be
    /// inspected.
    pub fn open(root: impl AsRef<Path>, epoch: CacheEpoch) -> Result<Self> {
        let supplied_root = root.as_ref();
        let root = std::path::absolute(supplied_root)
            .map_err(|source| io_error("resolve absolute cache root", supplied_root, source))?;
        let identity = OnceLock::new();
        if let Some(existing) = inspect_root(&root)? {
            let _ = identity.set(existing);
        }
        let base = layout::namespace(&root, epoch);
        let object_directory_path = base.join(layout::OBJECTS).join(layout::BUILD_ID);
        Ok(Self {
            base,
            root,
            object_directory_path,
            epoch,
            identity,
            object_directory: OnceLock::new(),
            #[cfg(feature = "access")]
            is_xdg: false,
            #[cfg(feature = "access")]
            access_directory: OnceLock::new(),
            #[cfg(feature = "manage")]
            manage_prepared: AtomicBool::new(false),
        })
    }

    /// Opens an application cache below `$XDG_CACHE_HOME`.
    ///
    /// An unset, empty, or relative `XDG_CACHE_HOME` falls back to
    /// `$HOME/.cache` as required by the XDG Base Directory Specification.
    /// `application` must be one normal path component. The resulting root is
    /// `<cache-home>/<application>/gsym`.
    ///
    /// Privileged applications should use [`Cache::open`] with an explicitly
    /// configured root instead of trusting process environment variables.
    ///
    /// # Errors
    ///
    /// Returns an error when no absolute cache home is available, the
    /// application identifier is invalid, or the resulting root is insecure.
    pub fn open_xdg(application: impl AsRef<OsStr>, epoch: CacheEpoch) -> Result<Self> {
        let root = xdg_cache_root(
            application.as_ref(),
            std::env::var_os("XDG_CACHE_HOME"),
            std::env::var_os("HOME"),
        )?;
        validated_xdg_paths(&root)?;
        let cache = Self::open(root, epoch)?;
        #[cfg(feature = "access")]
        {
            let mut cache = cache;
            cache.is_xdg = true;
            Ok(cache)
        }
        #[cfg(not(feature = "access"))]
        {
            Ok(cache)
        }
    }

    /// Returns the user-configured cache root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns this cache's converter epoch.
    #[must_use]
    pub const fn epoch(&self) -> CacheEpoch {
        self.epoch
    }

    /// Opens a cached GSYM file without taking a lock.
    ///
    /// The returned entry owns its read-only file descriptor, preventing a
    /// lookup-to-open race with pruning. Lookup validates filesystem ownership
    /// and file type but deliberately does not decode or fully verify GSYM on
    /// this hot path. Managed population and scrubbing perform full verification.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem failures or an untrusted entry. A file
    /// that does not exist returns `Ok(None)`.
    pub fn lookup(&self, build_id: &BuildId) -> Result<Option<CacheEntry>> {
        let Some(directory) = self.object_directory()? else {
            return Ok(None);
        };
        let key = layout::object_key(build_id);
        let file = match open_read_only_at(directory, &key) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(io_error(
                    "open cache entry",
                    layout::object(&self.base, build_id),
                    source,
                ));
            }
        };
        let metadata = file.metadata().map_err(|source| {
            io_error(
                "inspect cache entry",
                layout::object(&self.base, build_id),
                source,
            )
        })?;
        if !self.is_trusted_entry(&metadata)? {
            return Err(Error::UntrustedEntry {
                path: layout::object(&self.base, build_id),
            });
        }
        Ok(Some(CacheEntry::new(file, metadata.len())))
    }

    fn object_directory(&self) -> Result<Option<&File>> {
        if let Some(directory) = self.object_directory.get() {
            return Ok(Some(directory));
        }
        // Keep repeated lookups against a cache that has never been created to
        // one syscall; the secure component walk below remains authoritative.
        match probe_directory(&self.object_directory_path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(io_error(
                    "probe cache object directory",
                    &self.object_directory_path,
                    source,
                ));
            }
        }
        let Some(directory) = open_directory_chain(&self.object_directory_path, false)? else {
            return Ok(None);
        };
        drop(self.object_directory.set(directory));
        Ok(self.object_directory.get())
    }

    #[cfg(feature = "manage")]
    pub(crate) fn lookup_path(&self, path: &Path) -> Result<Option<CacheEntry>> {
        let file = match open_read_only(path) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(io_error("open cache entry", path, source)),
        };
        let metadata = file
            .metadata()
            .map_err(|source| io_error("inspect cache entry", path, source))?;
        self.validate_entry(path, &metadata)?;
        Ok(Some(CacheEntry::new(file, metadata.len())))
    }

    #[cfg(feature = "access")]
    pub(crate) fn base(&self) -> &Path {
        &self.base
    }

    #[cfg(feature = "access")]
    pub(crate) fn xdg_paths(&self) -> Option<(&Path, &Path)> {
        if !self.is_xdg {
            return None;
        }
        let application = self.root.parent()?;
        Some((application.parent()?, application))
    }

    pub(crate) fn ensure_identity(&self) -> Result<&RootIdentity> {
        if let Some(identity) = self.identity.get() {
            return Ok(identity);
        }
        let identity = inspect_root(&self.root)?.ok_or_else(|| Error::InsecureDirectory {
            path: self.root.clone(),
        })?;
        Ok(self.identity.get_or_init(|| identity))
    }

    fn is_trusted_entry(&self, metadata: &Metadata) -> Result<bool> {
        Ok(metadata.is_file() && self.ensure_identity()?.owns(metadata))
    }

    #[cfg(feature = "access")]
    pub(crate) fn validate_entry(&self, path: &Path, metadata: &Metadata) -> Result<()> {
        if !self.is_trusted_entry(metadata)? {
            return Err(Error::UntrustedEntry {
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }

    #[cfg(feature = "access")]
    pub(crate) fn validate_file(&self, path: &Path, file: &File) -> Result<()> {
        let metadata = file
            .metadata()
            .map_err(|source| io_error("inspect cache file", path, source))?;
        self.validate_entry(path, &metadata)
    }

    #[cfg(feature = "access")]
    pub(crate) fn owns_uid(&self, uid: u32) -> Result<bool> {
        Ok(self.ensure_identity()?.uid == uid)
    }
}

fn validated_xdg_paths(root: &Path) -> Result<()> {
    let application = root.parent().ok_or_else(|| Error::InsecureDirectory {
        path: root.to_path_buf(),
    })?;
    let cache_home = application
        .parent()
        .ok_or_else(|| Error::InsecureDirectory {
            path: root.to_path_buf(),
        })?;
    inspect_xdg_anchor(cache_home)?;
    inspect_xdg_anchor_parent(cache_home)?;
    inspect_xdg_anchor(application)?;
    Ok(())
}

fn inspect_xdg_anchor(path: &Path) -> Result<()> {
    let Some(directory) = open_directory_chain(path, false)? else {
        return Ok(());
    };
    validate_xdg_anchor(path, &directory)
}

fn inspect_xdg_anchor_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| Error::InsecureDirectory {
        path: path.to_path_buf(),
    })?;
    let directory =
        open_directory_chain(parent, false)?.ok_or_else(|| Error::InsecureDirectory {
            path: parent.to_path_buf(),
        })?;
    let metadata = directory
        .metadata()
        .map_err(|source| io_error("inspect XDG cache parent", parent, source))?;
    if !is_replace_protected_directory(&metadata, rustix::process::geteuid().as_raw()) {
        return Err(Error::InsecureDirectory {
            path: parent.to_path_buf(),
        });
    }
    Ok(())
}

impl fmt::Debug for Cache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Cache")
            .field("root", &self.root)
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// An opened, immutable cache entry.
#[cfg_attr(docsrs, doc(cfg(feature = "lookup")))]
pub struct CacheEntry {
    file: File,
    len: u64,
}

impl CacheEntry {
    pub(crate) const fn new(file: File, len: u64) -> Self {
        Self { file, len }
    }

    /// Borrows the read-only cached GSYM file.
    #[must_use]
    pub const fn file(&self) -> &File {
        &self.file
    }

    /// Consumes the entry and returns its read-only file.
    #[must_use]
    pub fn into_file(self) -> File {
        self.file
    }

    /// Returns the file length observed when the entry was opened.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Returns whether the entry is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl AsRef<File> for CacheEntry {
    fn as_ref(&self) -> &File {
        self.file()
    }
}

impl std::os::fd::AsFd for CacheEntry {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.file)
    }
}

impl From<CacheEntry> for File {
    fn from(entry: CacheEntry) -> Self {
        entry.into_file()
    }
}

impl fmt::Debug for CacheEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CacheEntry")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RootIdentity {
    uid: u32,
}

impl RootIdentity {
    fn owns(self, metadata: &Metadata) -> bool {
        use std::os::unix::fs::MetadataExt;
        metadata.uid() == self.uid
    }
}

fn inspect_root(path: &Path) -> Result<Option<RootIdentity>> {
    let Some(directory) = open_directory_chain(path, false)? else {
        return Ok(None);
    };
    let metadata = directory
        .metadata()
        .map_err(|source| io_error("inspect cache root", path, source))?;
    if !is_owned_private_directory(&metadata, rustix::process::geteuid().as_raw()) {
        return Err(Error::InsecureDirectory {
            path: path.to_path_buf(),
        });
    }
    Ok(Some(RootIdentity {
        uid: metadata_uid(&metadata),
    }))
}

#[cfg(feature = "access")]
pub(crate) fn ensure_private_directory(path: &Path) -> Result<()> {
    drop(open_private_directory(path)?);
    Ok(())
}

#[cfg(feature = "access")]
pub(crate) fn open_private_directory(path: &Path) -> Result<File> {
    let directory = open_directory_chain(path, true)?.ok_or_else(|| Error::InsecureDirectory {
        path: path.to_path_buf(),
    })?;
    let metadata = directory
        .metadata()
        .map_err(|source| io_error("inspect cache directory", path, source))?;
    if !is_owned_private_directory(&metadata, rustix::process::geteuid().as_raw()) {
        return Err(Error::InsecureDirectory {
            path: path.to_path_buf(),
        });
    }
    Ok(directory)
}

#[cfg(feature = "access")]
pub(crate) fn ensure_xdg_anchor(path: &Path) -> Result<()> {
    let directory = open_directory_chain(path, true)?.ok_or_else(|| Error::InsecureDirectory {
        path: path.to_path_buf(),
    })?;
    validate_xdg_anchor(path, &directory)
}

fn validate_xdg_anchor(path: &Path, directory: &File) -> Result<()> {
    let metadata = directory
        .metadata()
        .map_err(|source| io_error("inspect XDG cache home", path, source))?;
    if !is_owned_secure_parent(&metadata, rustix::process::geteuid().as_raw()) {
        return Err(Error::InsecureDirectory {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn is_owned_private_directory(metadata: &Metadata, uid: u32) -> bool {
    metadata.is_dir() && metadata_uid(metadata) == uid && is_private(metadata)
}

fn is_owned_secure_parent(metadata: &Metadata, uid: u32) -> bool {
    metadata.is_dir() && metadata_uid(metadata) == uid && !is_group_or_other_writable(metadata)
}

fn is_replace_protected_directory(metadata: &Metadata, uid: u32) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    let owner = metadata_uid(metadata);
    is_replace_protected(owner, uid, metadata.permissions().mode(), metadata.is_dir())
}

const fn is_replace_protected(owner: u32, uid: u32, mode: u32, is_directory: bool) -> bool {
    is_directory && (owner == uid || owner == 0) && (mode & 0o022 == 0 || mode & 0o1000 != 0)
}

fn open_directory_chain(path: &Path, create: bool) -> Result<Option<File>> {
    use std::path::Component;

    use rustix::fs::{Mode, OFlags};

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(Error::InsecureDirectory {
            path: path.to_path_buf(),
        });
    }

    let start = if path.is_absolute() { "/" } else { "." };
    let mut directory = rustix::fs::open(
        start,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|source| io_error("open cache path anchor", start, io::Error::from(source)))?;

    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir | Component::CurDir) {
                continue;
            }
            return Err(Error::InsecureDirectory {
                path: path.to_path_buf(),
            });
        };
        loop {
            match rustix::fs::openat(
                &directory,
                name,
                OFlags::RDONLY
                    | OFlags::DIRECTORY
                    | OFlags::CLOEXEC
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK,
                Mode::empty(),
            ) {
                Ok(next) => {
                    directory = File::from(next);
                    break;
                }
                Err(source) if source == rustix::io::Errno::NOENT && !create => {
                    return Ok(None);
                }
                Err(source) if source == rustix::io::Errno::NOENT && create => {
                    match rustix::fs::mkdirat(
                        &directory,
                        name,
                        Mode::RUSR | Mode::WUSR | Mode::XUSR,
                    ) {
                        Ok(()) => directory.sync_all().map_err(|source| {
                            io_error("sync cache directory parent", path, source)
                        })?,
                        Err(source) if source == rustix::io::Errno::EXIST => {}
                        Err(source) => {
                            return Err(io_error(
                                "create cache directory",
                                path,
                                io::Error::from(source),
                            ));
                        }
                    }
                }
                Err(source) => {
                    return Err(io_error(
                        "open cache directory",
                        path,
                        io::Error::from(source),
                    ));
                }
            }
        }
    }
    Ok(Some(directory))
}

#[cfg(feature = "manage")]
pub(crate) fn open_existing_directory(path: &Path) -> Result<File> {
    open_directory_chain(path, false)?.ok_or_else(|| {
        io_error(
            "open cache directory",
            path,
            io::Error::from(io::ErrorKind::NotFound),
        )
    })
}

fn metadata_uid(metadata: &Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.uid()
}

#[expect(
    clippy::verbose_bit_mask,
    reason = "the Unix permission bits are clearer as their conventional octal mask"
)]
fn is_private(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o077 == 0
}

fn is_group_or_other_writable(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o022 != 0
}

#[cfg(feature = "manage")]
pub(crate) fn open_read_only(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

fn open_read_only_at(directory: &File, key: &layout::Key) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, ResolveFlags};

    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let path = key
        .as_c_str()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    match rustix::fs::openat2(
        directory,
        path,
        flags,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    ) {
        Ok(file) => Ok(File::from(file)),
        Err(rustix::io::Errno::NOSYS | rustix::io::Errno::PERM) => {
            let shard_name = key
                .shard()
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
            let filename = key
                .filename()
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
            let shard = rustix::fs::openat(
                directory,
                shard_name,
                OFlags::RDONLY
                    | OFlags::DIRECTORY
                    | OFlags::CLOEXEC
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK,
                Mode::empty(),
            )?;
            rustix::fs::openat(&shard, filename, flags, Mode::empty())
                .map(File::from)
                .map_err(io::Error::from)
        }
        Err(source) => Err(io::Error::from(source)),
    }
}

fn probe_directory(path: &Path) -> io::Result<()> {
    use rustix::fs::{Mode, OFlags};

    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(drop)
    .map_err(io::Error::from)
}

fn xdg_cache_root(
    application: &OsStr,
    xdg_cache_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf> {
    let application_path = Path::new(application);
    let mut components = application_path.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(Error::InvalidApplicationId {
            value: application_path.to_path_buf(),
        });
    }
    let base = xdg_cache_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".cache"))
        })
        .ok_or(Error::CacheHomeUnavailable)?;
    Ok(base.join(application_path).join("gsym"))
}

#[cfg(test)]
mod tests {
    use super::{Cache, CacheEpoch, is_replace_protected, validated_xdg_paths, xdg_cache_root};

    #[test]
    fn xdg_cache_root_uses_absolute_xdg_home_and_home_fallback() {
        assert_eq!(
            xdg_cache_root(
                "chronon".as_ref(),
                Some("/cache".into()),
                Some("/home/u".into())
            )
            .expect("absolute XDG cache home is accepted"),
            std::path::PathBuf::from("/cache/chronon/gsym")
        );
        assert_eq!(
            xdg_cache_root(
                "chronon".as_ref(),
                Some("relative".into()),
                Some("/home/u".into())
            )
            .expect("relative XDG cache home falls back to home"),
            std::path::PathBuf::from("/home/u/.cache/chronon/gsym")
        );
    }

    #[test]
    fn xdg_cache_root_rejects_unsafe_or_unresolved_paths() {
        assert!(xdg_cache_root("../chronon".as_ref(), Some("/cache".into()), None).is_err());
        assert!(xdg_cache_root("chronon".as_ref(), None, Some("relative".into())).is_err());
    }

    #[test]
    fn xdg_cache_anchor_must_not_be_shared_writable() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("temporary directory is created");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("cache parent permissions are changed");
        let anchor = directory.path().join("cache-home");
        std::fs::create_dir(&anchor).expect("cache home is created");
        std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o750))
            .expect("cache home permissions are changed");
        let result = validated_xdg_paths(&anchor.join("app/gsym"));
        assert!(result.is_ok(), "{result:?}");

        std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o770))
            .expect("cache home permissions are changed");
        assert!(validated_xdg_paths(&anchor.join("app/gsym")).is_err());

        std::fs::set_permissions(&anchor, std::fs::Permissions::from_mode(0o700))
            .expect("cache home permissions are changed");
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o777))
            .expect("cache parent permissions are changed");
        assert!(validated_xdg_paths(&anchor.join("app/gsym")).is_err());
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o1777))
            .expect("cache parent permissions are changed");
        let result = validated_xdg_paths(&anchor.join("app/gsym"));
        assert!(result.is_ok(), "{result:?}");

        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .expect("cache parent permissions are changed");
        let application = anchor.join("app");
        std::fs::create_dir(&application).expect("application cache is created");
        std::fs::set_permissions(&application, std::fs::Permissions::from_mode(0o755))
            .expect("application cache permissions are changed");
        let result = validated_xdg_paths(&application.join("gsym"));
        assert!(result.is_ok(), "{result:?}");

        let uid = rustix::process::geteuid().as_raw();
        assert!(!is_replace_protected(
            uid.saturating_add(1),
            uid,
            0o1777,
            true
        ));
    }

    #[test]
    fn cache_root_rejects_parent_components_before_creation() {
        assert!(Cache::open("missing/../cache", CacheEpoch::new(1)).is_err());
    }
}
