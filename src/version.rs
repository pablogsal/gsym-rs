/// GSYM encoding version.
///
/// [`V1`](Self::V1) is the default and the version current tooling reads.
/// [`V2`](Self::V2) raises v1's 4 GiB size limit and its 20-byte build-ID
/// limit, at the cost of requiring LLVM 23 or newer to read it.
///
/// Select a version with
/// [`GsymBuilder::version`](crate::GsymBuilder::version), or convert an existing
/// file with [`Gsym::transcode`](crate::Gsym::transcode). Writing never upgrades
/// a file on its own: a model that exceeds a v1 limit fails with
/// [`Error::V1LimitExceeded`](crate::Error::V1LimitExceeded).
///
/// See [`docs::format`](crate::docs::format) for what differs on disk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum GsymVersion {
    /// Widely deployed format, readable by every current tool.
    #[default]
    V1,
    /// Format without v1's size limits, readable by LLVM 23 and newer.
    V2,
}

impl GsymVersion {
    /// Returns the version number stored in a GSYM header.
    #[must_use]
    pub const fn number(self) -> u16 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
}

impl std::fmt::Display for GsymVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "v{}", self.number())
    }
}
