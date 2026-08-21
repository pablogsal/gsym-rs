use std::ffi::CStr;
use std::path::{Path, PathBuf};

use std::os::unix::ffi::OsStrExt as _;

use crate::build_id::MAX_BUILD_ID_LEN;
use crate::{BuildId, CacheEpoch};

pub(crate) const OBJECTS: &str = "objects";
pub(crate) const OBJECT_EXTENSION: &str = ".gsym";
#[cfg(feature = "access")]
pub(crate) const ACCESS: &str = "access";
#[cfg(feature = "access")]
pub(crate) const ACCESS_EXTENSION: &str = ".lru";
#[cfg(feature = "manage")]
pub(crate) const LOCKS: &str = "locks";
#[cfg(feature = "manage")]
pub(crate) const NEGATIVE: &str = "negative";
#[cfg(feature = "manage")]
pub(crate) const NEGATIVE_EXTENSION: &str = ".neg";
#[cfg(feature = "manage")]
pub(crate) const TEMPORARY: &str = "tmp";
#[cfg(feature = "manage")]
pub(crate) const TEMPORARY_EXTENSION: &str = ".tmp";
pub(crate) const BUILD_ID: &str = ".build-id";
const HEX: &[u8; 16] = b"0123456789abcdef";
const MAX_FILENAME_LEN: usize = MAX_BUILD_ID_LEN.saturating_sub(1).saturating_mul(2) + 5;
const MAX_KEY_LEN: usize = 2 + 1 + MAX_FILENAME_LEN;

pub(crate) struct Key {
    bytes: [u8; MAX_KEY_LEN + 1],
    shard: [u8; 3],
    len: usize,
}

impl Key {
    pub(crate) fn as_c_str(&self) -> Option<&CStr> {
        self.bytes
            .get(..=self.len)
            .and_then(|bytes| CStr::from_bytes_with_nul(bytes).ok())
    }

    pub(crate) fn shard(&self) -> Option<&CStr> {
        CStr::from_bytes_with_nul(&self.shard).ok()
    }

    pub(crate) fn filename(&self) -> Option<&CStr> {
        self.bytes
            .get(3..=self.len)
            .and_then(|bytes| CStr::from_bytes_with_nul(bytes).ok())
    }
}

pub(crate) fn namespace(root: &Path, epoch: CacheEpoch) -> PathBuf {
    let mut path = PathBuf::with_capacity(root.as_os_str().as_bytes().len().saturating_add(16));
    path.push(root);
    path.push("v1");
    path.push(format!("e{}", epoch.get()));
    path
}

pub(crate) fn object(base: &Path, build_id: &BuildId) -> PathBuf {
    keyed_path(base, OBJECTS, build_id.as_bytes(), OBJECT_EXTENSION)
}

pub(crate) fn object_key(build_id: &BuildId) -> Key {
    keyed_name(build_id.as_bytes(), OBJECT_EXTENSION)
}

#[cfg(feature = "access")]
pub(crate) fn access(base: &Path, build_id: &BuildId) -> PathBuf {
    access_bytes(base, build_id.as_bytes())
}

#[cfg(feature = "access")]
pub(crate) fn access_bytes(base: &Path, build_id: &[u8]) -> PathBuf {
    keyed_path(base, ACCESS, build_id, ACCESS_EXTENSION)
}

#[cfg(feature = "access")]
pub(crate) fn access_key(build_id: &BuildId) -> Key {
    access_key_bytes(build_id.as_bytes())
}

#[cfg(feature = "access")]
pub(crate) fn access_key_bytes(build_id: &[u8]) -> Key {
    keyed_name(build_id, ACCESS_EXTENSION)
}

#[cfg(feature = "manage")]
pub(crate) fn lock(base: &Path, build_id: &BuildId) -> PathBuf {
    // A fixed 4096-slot table bounds persistent lock-file growth. Collisions
    // may delay an unrelated population attempt but cannot mix entries.
    let slot = lock_slot(build_id.as_bytes());
    let shard = [hex_digit(((slot >> 8) & 0x0f) as u8)];
    let filename = [
        hex_digit(((slot >> 4) & 0x0f) as u8),
        hex_digit((slot & 0x0f) as u8),
        b'.',
        b'l',
        b'o',
        b'c',
        b'k',
    ];
    let mut path = PathBuf::with_capacity(
        base.as_os_str()
            .as_bytes()
            .len()
            .saturating_add(LOCKS.len())
            .saturating_add(12),
    );
    path.push(base);
    path.push(LOCKS);
    path.push(std::ffi::OsStr::from_bytes(&shard));
    path.push(std::ffi::OsStr::from_bytes(&filename));
    path
}

#[cfg(feature = "manage")]
pub(crate) fn lock_slot(bytes: &[u8]) -> u16 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let hash = bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    ((hash ^ (hash >> 32)) & 0x0fff) as u16
}

#[cfg(feature = "manage")]
pub(crate) fn negative(base: &Path, build_id: &BuildId) -> PathBuf {
    keyed_path(base, NEGATIVE, build_id.as_bytes(), NEGATIVE_EXTENSION)
}

#[cfg(feature = "manage")]
pub(crate) fn temporary(base: &Path, build_id: &BuildId) -> PathBuf {
    keyed_path(base, TEMPORARY, build_id.as_bytes(), TEMPORARY_EXTENSION)
}

fn keyed_path(base: &Path, kind: &str, bytes: &[u8], suffix: &str) -> PathBuf {
    let key = keyed_name(bytes, suffix);
    let bytes = key.as_c_str().map(CStr::to_bytes).unwrap_or_default();
    let directory = bytes.get(..2).unwrap_or_default();
    let filename = bytes.get(3..).unwrap_or_default();

    let mut path = PathBuf::with_capacity(
        base.as_os_str()
            .as_bytes()
            .len()
            .saturating_add(kind.len())
            .saturating_add(key.len)
            .saturating_add(16),
    );
    path.push(base);
    path.push(kind);
    path.push(BUILD_ID);
    path.push(std::ffi::OsStr::from_bytes(directory));
    path.push(std::ffi::OsStr::from_bytes(filename));
    path
}

fn keyed_name(bytes: &[u8], suffix: &str) -> Key {
    let first = bytes.first().copied().unwrap_or_default();
    let mut key = Key {
        bytes: [0; MAX_KEY_LEN + 1],
        shard: [0; 3],
        len: 0,
    };
    key.bytes[0] = hex_digit(first >> 4);
    key.bytes[1] = hex_digit(first & 0x0f);
    key.bytes[2] = b'/';
    key.shard[0] = key.bytes[0];
    key.shard[1] = key.bytes[1];
    let rest = bytes.get(1..).unwrap_or_default();
    let hex_len = rest.len().saturating_mul(2);
    let (pairs, _) = key.bytes[3..].as_chunks_mut::<2>();
    for (byte, [high, low]) in rest.iter().zip(pairs) {
        *high = hex_digit(*byte >> 4);
        *low = hex_digit(*byte & 0x0f);
    }
    let suffix_start = 3_usize.saturating_add(hex_len);
    let filename_end = suffix_start.saturating_add(suffix.len());
    if let Some(destination) = key.bytes.get_mut(suffix_start..filename_end) {
        destination.copy_from_slice(suffix.as_bytes());
    }
    key.len = filename_end;
    key
}

fn hex_digit(nibble: u8) -> u8 {
    HEX.get(usize::from(nibble)).copied().unwrap_or_default()
}

#[cfg(all(test, feature = "manage"))]
mod tests {
    use super::lock;
    use crate::BuildId;

    #[test]
    fn lock_slot_uses_the_complete_build_identifier() {
        let first = BuildId::new([0x12, 0x30, 0x01]).expect("build ID is valid");
        let second = BuildId::new([0x12, 0x30, 0x02]).expect("build ID is valid");

        assert_ne!(
            lock(std::path::Path::new("cache"), &first),
            lock(std::path::Path::new("cache"), &second)
        );
    }
}
