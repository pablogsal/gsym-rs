use std::borrow::Borrow;
use std::fmt;
use std::str::FromStr;

/// Longest build identifier accepted by the conventional `.build-id` layout.
///
/// At 126 bytes, the second path component remains within the common 255-byte
/// filesystem name limit after hexadecimal encoding and adding `.gsym`.
pub(crate) const MAX_BUILD_ID_LEN: usize = 126;

/// A validated, owned binary build identifier.
///
/// Identifiers can be constructed from raw bytes or parsed from hexadecimal:
///
/// ```
/// use gsym_cache::BuildId;
///
/// let build_id: BuildId = "0123abcdef".parse()?;
/// assert_eq!(build_id.as_bytes(), &[0x01, 0x23, 0xab, 0xcd, 0xef]);
/// assert_eq!(build_id.to_string(), "0123abcdef");
/// # Ok::<(), gsym_cache::BuildIdError>(())
/// ```
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg_attr(docsrs, doc(cfg(feature = "lookup")))]
pub struct BuildId(Box<[u8]>);

impl BuildId {
    /// Validates and copies a binary build identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty identifier or one too long for the cache
    /// layout.
    pub fn new(bytes: impl AsRef<[u8]>) -> Result<Self, BuildIdError> {
        let bytes = bytes.as_ref();
        validate(bytes)?;
        Ok(Self(bytes.into()))
    }

    fn from_boxed(bytes: Box<[u8]>) -> Result<Self, BuildIdError> {
        validate(&bytes)?;
        Ok(Self(bytes))
    }

    /// Returns the raw build-identifier bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

const fn validate(bytes: &[u8]) -> Result<(), BuildIdError> {
    if bytes.is_empty() {
        return Err(BuildIdError::Empty);
    }
    if bytes.len() > MAX_BUILD_ID_LEN {
        return Err(BuildIdError::TooLong {
            length: bytes.len(),
            maximum: MAX_BUILD_ID_LEN,
        });
    }
    Ok(())
}

impl AsRef<[u8]> for BuildId {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Borrow<[u8]> for BuildId {
    fn borrow(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl TryFrom<&[u8]> for BuildId {
    type Error = BuildIdError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::new(bytes)
    }
}

impl TryFrom<Vec<u8>> for BuildId {
    type Error = BuildIdError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Self::from_boxed(bytes.into_boxed_slice())
    }
}

impl TryFrom<Box<[u8]>> for BuildId {
    type Error = BuildIdError;

    fn try_from(bytes: Box<[u8]>) -> Result<Self, Self::Error> {
        Self::from_boxed(bytes)
    }
}

impl fmt::Display for BuildId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for BuildId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BuildId(")?;
        fmt::Display::fmt(self, formatter)?;
        formatter.write_str(")")
    }
}

impl FromStr for BuildId {
    type Err = BuildIdError;

    fn from_str(encoded: &str) -> Result<Self, Self::Err> {
        if !encoded.len().is_multiple_of(2) {
            return Err(BuildIdError::OddHexLength {
                length: encoded.len(),
            });
        }
        let byte_length = encoded.len() / 2;
        if byte_length > MAX_BUILD_ID_LEN {
            return Err(BuildIdError::TooLong {
                length: byte_length,
                maximum: MAX_BUILD_ID_LEN,
            });
        }
        let mut bytes = Vec::with_capacity(byte_length);
        let (pairs, _) = encoded.as_bytes().as_chunks::<2>();
        for (pair_index, [high, low]) in pairs.iter().enumerate() {
            let high_index = pair_index.saturating_mul(2);
            let high = hex_nibble(*high).ok_or(BuildIdError::InvalidHex {
                index: high_index,
                byte: *high,
            })?;
            let low_index = high_index.saturating_add(1);
            let low = hex_nibble(*low).ok_or(BuildIdError::InvalidHex {
                index: low_index,
                byte: *low,
            })?;
            bytes.push((high << 4) | low);
        }
        Self::try_from(bytes)
    }
}

pub(crate) const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(byte.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Some(byte.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

/// Build identifier validation error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
#[cfg_attr(docsrs, doc(cfg(feature = "lookup")))]
pub enum BuildIdError {
    /// Build identifiers used as cache keys may not be empty.
    #[error("build identifier is empty")]
    Empty,
    /// The encoded identifier would exceed the filesystem component limit.
    #[error("build identifier is {length} bytes; maximum is {maximum}")]
    TooLong {
        /// Supplied byte length.
        length: usize,
        /// Maximum accepted byte length.
        maximum: usize,
    },
    /// A hexadecimal representation contains an incomplete byte.
    #[error("hexadecimal build identifier has odd length {length}")]
    OddHexLength {
        /// Supplied string length in bytes.
        length: usize,
    },
    /// A hexadecimal representation contains a non-hexadecimal byte.
    #[error("invalid hexadecimal byte {byte:#04x} at index {index}")]
    InvalidHex {
        /// Byte index in the supplied string.
        index: usize,
        /// Rejected byte.
        byte: u8,
    },
}
