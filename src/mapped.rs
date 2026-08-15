use std::fs::File;
use std::path::Path;

use memmap2::{Mmap, MmapOptions};

use crate::{Error, Gsym, Result};

/// GSYM reader backed directly by a read-only memory map.
///
/// This is the same reader as [`Gsym`], with `memmap2::Mmap` selected as its
/// byte storage, so it has the same query API. Mapping suits a large file with
/// sparse lookups, where reading the whole file into memory would cost more
/// than the lookups do.
///
/// Both constructors are `unsafe` because a memory map is not a snapshot. If
/// any process truncates or rewrites the file while it is mapped, results
/// borrowed from it can observe changed bytes or become invalid.
/// [`Gsym::open`] reads an owned snapshot instead and carries no such
/// requirement.
///
/// ```no_run
/// use gsym::MappedGsym;
///
/// // SAFETY: this process controls the file and keeps it immutable while mapped.
/// let gsym = unsafe { MappedGsym::map("app.gsym")? };
/// if let Some(symbol) = gsym.lookup(0x401000)? {
///     println!("{}", String::from_utf8_lossy(symbol.frames[0].name));
/// }
/// # Ok::<(), gsym::Error>(())
/// ```
///
/// A mapped reader and an owned one parse the same file identically:
///
/// ```no_run
/// use gsym::{Gsym, MappedGsym};
///
/// let snapshot = Gsym::open("app.gsym")?;
///
/// // SAFETY: this application owns the file and keeps it immutable while mapped.
/// let mapped = unsafe { MappedGsym::map("app.gsym")? };
/// let file = std::fs::File::open("app.gsym")?;
/// // SAFETY: the same file-stability guarantee applies to this mapping.
/// let mapped_file = unsafe { MappedGsym::map_file(&file)? };
///
/// assert_eq!(snapshot.header(), mapped.header());
/// assert_eq!(mapped.header(), mapped_file.header());
/// # Ok::<(), gsym::Error>(())
/// ```
pub type MappedGsym = Gsym<Mmap>;

impl Gsym<Mmap> {
    /// Opens and validates a GSYM file through a read-only memory map.
    ///
    /// # Safety
    ///
    /// The mapped file must not be modified or truncated by any process for
    /// the lifetime of the returned mapping. Use [`Gsym::open`] to read owned
    /// bytes when that cannot be guaranteed.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the file cannot be opened or mapped, or a
    /// format error when its GSYM metadata is invalid.
    pub unsafe fn map(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| Error::IoAtPath {
            operation: "open GSYM file for mapping",
            path: path.to_path_buf(),
            source,
        })?;
        // SAFETY: the caller accepted the file-stability requirement.
        let mapping =
            unsafe { MmapOptions::new().map(&file) }.map_err(|source| Error::IoAtPath {
                operation: "memory-map GSYM file",
                path: path.to_path_buf(),
                source,
            })?;
        Self::parse(mapping)
    }

    /// Maps and validates an already-open file.
    ///
    /// Use this when the file was opened elsewhere, for instance through a
    /// descriptor passed in or a handle kept for locking. The mapping does not
    /// keep the `File` alive, so it may be closed once this returns.
    ///
    /// # Safety
    ///
    /// `file` must not be modified or truncated by any process for the
    /// lifetime of the returned mapping.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when mapping fails or a format error for invalid
    /// GSYM metadata.
    pub unsafe fn map_file(file: &File) -> Result<Self> {
        // SAFETY: guaranteed by this function's caller.
        let mapping = unsafe { MmapOptions::new().map(file)? };
        Self::parse(mapping)
    }
}
