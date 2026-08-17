mod discovery;
mod image;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use object::Object;

use self::discovery::{
    Discovery, DiscoveryCache, discover_dwp, discover_separate_debug, discover_supplementary,
    validated_gnu_debugdata,
};
pub(in crate::convert) use self::image::AddressLayout;
use self::image::{
    cross_check_elf_architecture, cross_check_elf_identity, malformed, parse_elf,
    require_supported_kind,
};
use super::ConversionWarning;
use crate::builder::{BuilderOptions, GsymBuilder};
use crate::{ElfInputKind, Endian, Error, Result, WriterOptions};

/// ELF inputs used to construct a GSYM file.
///
/// Only [`image`](Self::image) is required. Leaving a companion as `None` makes
/// the converter fall back to the image itself for symbols and DWARF, so a
/// single unstripped binary needs nothing else.
///
/// Passing inputs this way skips discovery entirely: nothing is read from the
/// filesystem or the network. Use
/// [`ElfConverter::convert_path`] when companion files should be searched for.
///
/// ```no_run
/// use gsym::convert::ElfInputs;
///
/// let image = std::fs::read("app")?;
/// let debug = std::fs::read("app.debug")?;
/// let inputs = ElfInputs::new(&image).with_debug(&debug);
/// # let _ = inputs;
/// # Ok::<(), gsym::Error>(())
/// ```
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ElfInputs<'data> {
    /// Linked `ET_EXEC`, `ET_DYN`, or relocatable `ET_REL` ELF image.
    pub image: &'data [u8],
    /// Explicit separate DWARF ELF, if different from the image.
    pub debug: Option<&'data [u8]>,
    /// Extra symbol-table ELF read alongside the image's and debug input's own
    /// tables.
    pub symbols: Option<&'data [u8]>,
    /// Supplementary DWARF ELF referenced by the main debug input.
    pub supplementary: Option<&'data [u8]>,
    /// Packaged split-DWARF input.
    pub dwp: Option<&'data [u8]>,
}

impl fmt::Debug for ElfInputs<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ElfInputs")
            .field("image_len", &self.image.len())
            .field("debug_len", &self.debug.map(<[u8]>::len))
            .field("symbols_len", &self.symbols.map(<[u8]>::len))
            .field("supplementary_len", &self.supplementary.map(<[u8]>::len))
            .field("dwp_len", &self.dwp.map(<[u8]>::len))
            .finish()
    }
}

impl<'data> ElfInputs<'data> {
    /// Creates inputs using one ELF for the image, symbols, and DWARF.
    #[must_use]
    pub const fn new(image: &'data [u8]) -> Self {
        Self {
            image,
            debug: None,
            symbols: None,
            supplementary: None,
            dwp: None,
        }
    }

    /// Uses an explicit ELF containing the image's DWARF sections.
    #[must_use]
    pub const fn with_debug(mut self, debug: &'data [u8]) -> Self {
        self.debug = Some(debug);
        self
    }

    /// Adds an explicit ELF symbol table to the ones already imported.
    #[must_use]
    pub const fn with_symbols(mut self, symbols: &'data [u8]) -> Self {
        self.symbols = Some(symbols);
        self
    }

    /// Supplies the supplementary DWARF ELF referenced by the main debug input.
    #[must_use]
    pub const fn with_supplementary(mut self, supplementary: &'data [u8]) -> Self {
        self.supplementary = Some(supplementary);
        self
    }

    /// Supplies a packaged split-DWARF file.
    #[must_use]
    pub const fn with_dwp(mut self, dwp: &'data [u8]) -> Self {
        self.dwp = Some(dwp);
        self
    }
}

/// Controls ELF/DWARF import and optional companion discovery.
///
/// Build from [`Default`] and adjust the fields you care about. The defaults
/// import symbols and DWARF with inline information, search for companion debug
/// files, and read two environment variables: `DEBUGINFOD_URLS` for the
/// servers to try, which is empty unless set, and `DEBUGINFOD_CACHE_PATH` for
/// the download cache. Clear
/// [`debuginfod_urls`](Self::debuginfod_urls) or set
/// [`discovery`](Self::discovery) to [`DiscoveryPolicy::Disabled`] for a
/// conversion that must not touch the network.
///
/// [`writer`](Self::writer) selects the output version, but its byte order,
/// base address, and build ID are overwritten from the image.
///
/// See [`docs::conversion`](crate::docs::conversion) for the discovery order
/// and what each limit bounds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversionOptions {
    /// GSYM writer settings.
    pub writer: WriterOptions,
    /// Import ELF `STT_FUNC` symbols.
    pub include_symbols: bool,
    /// DWARF import settings, or `None` to disable DWARF import.
    pub dwarf: Option<DwarfImportOptions>,
    /// Whether path conversion searches for companion debug files.
    pub discovery: DiscoveryPolicy,
    /// Roots searched for build-ID and mirrored-path debug files.
    pub debug_directories: Vec<PathBuf>,
    /// Debuginfod server URLs tried in order.
    pub debuginfod_urls: Vec<String>,
    /// Local debuginfod cache root.
    pub debuginfod_cache: PathBuf,
    /// Maximum accepted debuginfod response size in bytes.
    pub debuginfod_max_download_size: u64,
    /// Maximum decompressed `.gnu_debugdata` size in bytes.
    pub gnu_debugdata_max_decompressed_size: u64,
}

/// Selects optional DWARF records to import.
///
/// Inline information is on by default because it is what distinguishes GSYM
/// from a symbol table. Call sites are off, matching `llvm-gsymutil`.
///
/// Line rows are always imported when DWARF import is enabled. To skip DWARF
/// altogether, set [`ConversionOptions::dwarf`] to `None`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DwarfImportOptions {
    /// Import inline-call trees.
    pub inline_info: bool,
    /// Import DWARF call-site records.
    pub call_sites: bool,
}

impl Default for DwarfImportOptions {
    fn default() -> Self {
        Self {
            inline_info: true,
            call_sites: false,
        }
    }
}

/// Companion-file discovery policy for path-based conversion.
///
/// Applies to [`ElfConverter::convert_path`] only; [`ElfConverter::convert`]
/// never searches. `Disabled` prevents filesystem and network searches for
/// separate debug files, supplementary files, and split-DWARF data.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum DiscoveryPolicy {
    /// Convert only the requested image path.
    Disabled,
    #[default]
    /// Search separate, supplementary, split-DWARF, and remote debug sources.
    Enabled,
}

/// A potentially slow operation reported during path-based debug discovery.
///
/// Pass an observer to [`ElfConverter::convert_path_with_observer`] to surface
/// network activity in interactive tools.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DiscoveryEvent<'path> {
    /// A debuginfod request is about to be issued.
    DebuginfodRequest {
        /// Kind of companion being requested, such as `debug file`.
        artifact: &'static str,
        /// Hexadecimal ELF build ID.
        build_id: &'path str,
        /// Debuginfod server base URL.
        endpoint: &'path str,
        /// Image or debug file whose companion is being requested.
        related_path: &'path Path,
    },
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            writer: WriterOptions::default(),
            include_symbols: true,
            dwarf: Some(DwarfImportOptions::default()),
            discovery: DiscoveryPolicy::Enabled,
            debug_directories: vec![PathBuf::from("/usr/lib/debug")],
            debuginfod_urls: std::env::var("DEBUGINFOD_URLS")
                .ok()
                .map(|urls| urls.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default(),
            debuginfod_cache: std::env::var_os("DEBUGINFOD_CACHE_PATH").map_or_else(
                || std::env::temp_dir().join("gsym-rs-debuginfod"),
                PathBuf::from,
            ),
            debuginfod_max_download_size: 1 << 30,
            gnu_debugdata_max_decompressed_size: 1 << 30,
        }
    }
}

/// Counts describing one completed conversion.
///
/// `symbol_functions` and `dwarf_functions` overlap: a function present in both
/// the symbol table and the DWARF is counted once in each, and the builder keeps
/// the richer record. `rejected_ranges` includes expected linker tombstones as
/// well as invalid or unrepresentable input ranges.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct ConversionStats {
    /// Distinct functions imported from ELF symbol tables.
    pub symbol_functions: usize,
    /// Functions imported from DWARF DIEs.
    pub dwarf_functions: usize,
    /// Skeleton units whose matching DWO or DWP data was imported.
    pub split_dwarf_units: usize,
    /// Source-line rows attached to imported functions.
    pub line_rows: usize,
    /// Inline nodes attached to imported functions.
    pub inline_nodes: usize,
    /// Dead, invalid, or unrepresentable address ranges rejected.
    ///
    /// Expected zero, `-1`, or `-2` linker tombstones are counted here but do not
    /// produce individual conversion warnings.
    pub rejected_ranges: usize,
}

/// Successful conversion output plus diagnostics and provenance.
///
/// [`builder`](Self::builder) is ready to encode, or to edit first. The
/// `discovered_*` paths record which companion files were selected and are
/// `None` when the input supplied them or none were found, which makes them
/// worth logging when a conversion produces unexpected output.
///
/// A report can carry warnings and still be complete;
/// [`warnings`](Self::warnings) describes records that were skipped, not a
/// failed conversion.
#[non_exhaustive]
pub struct ConversionReport {
    /// Populated builder ready for encoding or further modification.
    pub builder: GsymBuilder,
    /// Counts of imported and rejected records.
    pub stats: ConversionStats,
    /// Non-fatal issues encountered during conversion.
    pub warnings: Vec<ConversionWarning>,
    /// Automatically selected main debug file.
    pub discovered_debug: Option<PathBuf>,
    /// Automatically selected supplementary debug file.
    pub discovered_supplementary: Option<PathBuf>,
    /// Automatically selected DWP package.
    pub discovered_dwp: Option<PathBuf>,
}

impl fmt::Debug for ConversionReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConversionReport")
            .field("builder", &self.builder)
            .field("stats", &self.stats)
            .field("warning_count", &self.warnings.len())
            .field("discovered_debug", &self.discovered_debug)
            .field("discovered_supplementary", &self.discovered_supplementary)
            .field("discovered_dwp", &self.discovered_dwp)
            .finish_non_exhaustive()
    }
}

/// Reusable ELF-to-GSYM converter with immutable options.
///
/// Options are fixed at construction, so one converter can be shared across
/// many images. [`Default`] uses [`ConversionOptions::default`].
///
/// ```no_run
/// use gsym::convert::ElfConverter;
///
/// let report = ElfConverter::default().convert_path("./app")?;
/// for warning in &report.warnings {
///     eprintln!("warning: {warning}");
/// }
/// report.builder.write_to(std::fs::File::create("app.gsym")?)?;
/// # Ok::<(), gsym::Error>(())
/// ```
#[derive(Clone, Debug, Default)]
pub struct ElfConverter {
    options: ConversionOptions,
    discovery_cache: OnceLock<Arc<DiscoveryCache>>,
}

impl PartialEq for ElfConverter {
    fn eq(&self, other: &Self) -> bool {
        self.options == other.options
    }
}

impl Eq for ElfConverter {}

impl ElfConverter {
    /// Creates a converter using `options`.
    #[must_use]
    pub const fn new(options: ConversionOptions) -> Self {
        Self {
            options,
            discovery_cache: OnceLock::new(),
        }
    }

    /// Returns this converter's immutable options.
    #[must_use]
    pub const fn options(&self) -> &ConversionOptions {
        &self.options
    }

    /// Converts linked ELF image/debug inputs into a GSYM builder.
    ///
    /// Uses exactly the inputs given, with no filesystem or network access.
    /// Companions supplied here are cross-checked against the image, so a debug
    /// or symbol file from a different architecture or build is rejected.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed inputs, incompatible companion files,
    /// invalid DWARF, or unrepresentable GSYM records.
    pub fn convert(&self, inputs: ElfInputs<'_>) -> Result<ConversionReport> {
        self.convert_inner(inputs, None)
    }

    fn convert_inner(
        &self,
        inputs: ElfInputs<'_>,
        dwo_base: Option<&Path>,
    ) -> Result<ConversionReport> {
        let image = parse_elf(inputs.image, ElfInputKind::Image)?;
        require_supported_kind(&image)?;

        let layout = AddressLayout::new(&image)?;
        let base_address = layout.ranges.first().map_or(0, |range| range.start);
        if layout.ranges.is_empty() {
            return Err(Error::InvalidModel("ELF image has no executable sections"));
        }

        let build_id = image
            .build_id()
            .map_err(|error| malformed("ELF build ID", error))?
            .unwrap_or_default()
            .to_vec();
        let mut warnings = Vec::new();
        let mini_debug = if inputs.debug.is_none()
            && (self.options.include_symbols || self.options.dwarf.is_some())
        {
            validated_gnu_debugdata(
                &image,
                self.options.gnu_debugdata_max_decompressed_size,
                &mut warnings,
            )
        } else {
            None
        };
        let debug_bytes = inputs
            .debug
            .or(mini_debug.as_deref())
            .unwrap_or(inputs.image);
        let separate_debug = inputs.debug.is_some() || mini_debug.is_some();
        let debug = parse_elf(debug_bytes, ElfInputKind::Debug)?;
        if inputs.debug.is_some() {
            cross_check_elf_identity(&image, &debug, ElfInputKind::Debug)?;
        }
        let supplementary = inputs
            .supplementary
            .map(|bytes| parse_elf(bytes, ElfInputKind::Supplementary))
            .transpose()?;
        let dwp = inputs
            .dwp
            .map(|bytes| parse_elf(bytes, ElfInputKind::Dwp))
            .transpose()?;
        if let Some(dwp) = &dwp {
            cross_check_elf_architecture(&image, dwp, ElfInputKind::Dwp)?;
        }

        let mut writer = self.options.writer.clone();
        writer.endian = if image.is_little_endian() {
            Endian::Little
        } else {
            Endian::Big
        };
        writer.base_address = Some(base_address);
        if writer.build_id.is_empty() {
            writer.build_id = build_id;
        }
        let mut builder = GsymBuilder::with_options(BuilderOptions {
            writer,
            executable_ranges: layout.ranges.clone().into_boxed_slice(),
            ..BuilderOptions::default()
        });
        let mut stats = ConversionStats::default();

        if self.options.include_symbols {
            let symbol_file = inputs
                .symbols
                .map(|bytes| parse_elf(bytes, ElfInputKind::Symbols))
                .transpose()?;
            if let Some(symbol_file) = &symbol_file {
                cross_check_elf_identity(&image, symbol_file, ElfInputKind::Symbols)?;
            }
            let mut sources: Vec<(&[u8], &object::File<'_>)> = Vec::with_capacity(3);
            if let (Some(bytes), Some(file)) = (inputs.symbols, symbol_file.as_ref()) {
                sources.push((bytes, file));
            }
            if separate_debug {
                sources.push((debug_bytes, &debug));
            }
            sources.push((inputs.image, &image));
            stats.symbol_functions = import_distinct_symbols(
                &sources,
                &layout,
                &mut builder,
                &mut stats.rejected_ranges,
            )?;
        }
        if let Some(dwarf_options) = self.options.dwarf {
            super::dwarf::import_dwarf(super::dwarf::DwarfImport {
                file: &debug,
                supplementary: supplementary.as_ref(),
                dwp: dwp.as_ref(),
                dwo_base,
                layout: &layout,
                executable_ranges: &layout.ranges,
                builder: &mut builder,
                include_inlines: dwarf_options.inline_info,
                include_call_sites: dwarf_options.call_sites,
                stats: &mut stats,
                warnings: &mut warnings,
            })?;
        }

        Ok(ConversionReport {
            builder,
            stats,
            warnings,
            discovered_debug: None,
            discovered_supplementary: None,
            discovered_dwp: None,
        })
    }

    /// Converts an ELF path and discovers supported companion debug files.
    ///
    /// Searches for companion debug information unless
    /// [`DiscoveryPolicy::Disabled`] is set. The chosen paths are reported in
    /// [`ConversionReport::discovered_debug`] and its siblings, and problems
    /// encountered while searching appear as warnings rather than errors.
    ///
    /// With debuginfod configured, this may perform network requests. See
    /// [`docs::conversion`](crate::docs::conversion).
    ///
    /// # Errors
    ///
    /// Returns an error when an input cannot be read or converted.
    pub fn convert_path(&self, image_path: impl AsRef<Path>) -> Result<ConversionReport> {
        self.convert_path_with_observer(image_path, |_| {})
    }

    /// Converts an ELF path while reporting potentially slow discovery work.
    ///
    /// The observer is called immediately before each debuginfod request. It
    /// may be called more than once when several servers or companions are
    /// considered.
    ///
    /// # Errors
    ///
    /// Returns an error when an input cannot be read or converted.
    pub fn convert_path_with_observer(
        &self,
        image_path: impl AsRef<Path>,
        mut observer: impl FnMut(DiscoveryEvent<'_>),
    ) -> Result<ConversionReport> {
        let image_path = image_path.as_ref();
        let image_bytes = read_file(image_path, "read ELF image")?;
        let discovery_cache = self.discovery_cache.get_or_init(Arc::default);
        let discovery_enabled = self.options.discovery == DiscoveryPolicy::Enabled;
        let companion_discovery_enabled =
            discovery_enabled && (self.options.include_symbols || self.options.dwarf.is_some());
        let discovery = if companion_discovery_enabled {
            discover_separate_debug(
                image_path,
                &image_bytes,
                &self.options,
                discovery_cache,
                &mut observer,
            )?
        } else {
            Discovery::default()
        };
        let debug_path = discovery
            .artifact
            .as_ref()
            .map_or(image_path, |artifact| artifact.path.as_path());
        let debug_source = discovery
            .artifact
            .as_ref()
            .map_or(image_bytes.as_slice(), |artifact| artifact.bytes.as_slice());
        let dwarf_discovery_enabled = discovery_enabled && self.options.dwarf.is_some();
        let supplementary_discovery = if dwarf_discovery_enabled {
            discover_supplementary(
                debug_path,
                debug_source,
                &self.options,
                discovery_cache,
                &mut observer,
            )?
        } else {
            Discovery::default()
        };
        let dwp = dwarf_discovery_enabled
            .then(|| discover_dwp(image_path, debug_path))
            .flatten();
        let dwo_base = dwarf_discovery_enabled
            .then(|| debug_path.parent())
            .flatten();
        let mut report = self.convert_inner(
            ElfInputs {
                image: &image_bytes,
                debug: discovery
                    .artifact
                    .as_ref()
                    .map(|artifact| artifact.bytes.as_slice()),
                symbols: None,
                supplementary: supplementary_discovery
                    .artifact
                    .as_ref()
                    .map(|artifact| artifact.bytes.as_slice()),
                dwp: dwp.as_ref().map(|artifact| artifact.bytes.as_slice()),
            },
            dwo_base,
        )?;
        report.warnings.extend(discovery.warnings);
        report.warnings.extend(supplementary_discovery.warnings);
        report.discovered_debug = discovery.artifact.map(|artifact| artifact.path);
        report.discovered_supplementary = supplementary_discovery
            .artifact
            .map(|artifact| artifact.path);
        report.discovered_dwp = dwp.map(|artifact| artifact.path);
        Ok(report)
    }
}

fn import_distinct_symbols(
    sources: &[(&[u8], &object::File<'_>)],
    layout: &AddressLayout,
    builder: &mut GsymBuilder,
    rejected: &mut usize,
) -> Result<usize> {
    let mut visited: Vec<&[u8]> = Vec::with_capacity(sources.len());
    let mut unique = Vec::with_capacity(sources.len());
    for &(bytes, file) in sources {
        if !visited.iter().any(|other| std::ptr::eq(*other, bytes)) {
            visited.push(bytes);
            unique.push(file);
        }
    }

    if let [file] = unique.as_slice() {
        let mut imported = 0_usize;
        return image::visit_symbols(file, layout, |function, disposition| {
            match disposition {
                image::SymbolDisposition::Import => {
                    builder.add_function(function)?;
                    imported = imported.saturating_add(1);
                }
                image::SymbolDisposition::Reject => {
                    *rejected = rejected.saturating_add(1);
                }
            }
            Ok(())
        })
        .map(|()| imported);
    }

    let mut accepted = Vec::new();
    let mut skipped = Vec::new();
    for file in unique {
        image::visit_symbols(file, layout, |function, disposition| {
            match disposition {
                image::SymbolDisposition::Import => accepted.push(function),
                image::SymbolDisposition::Reject => skipped.push(function),
            }
            Ok(())
        })?;
    }
    accepted.sort_unstable();
    accepted.dedup();
    skipped.sort_unstable();
    skipped.dedup();
    skipped.retain(|function| accepted.binary_search(function).is_err());

    let imported = accepted.len();
    *rejected = rejected.saturating_add(skipped.len());
    for function in accepted {
        builder.add_function(function)?;
    }
    Ok(imported)
}

fn read_file(path: &Path, description: &'static str) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::IoAtPath {
        operation: description,
        path: path.to_path_buf(),
        source,
    })
}
