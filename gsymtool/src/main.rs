//! Command-line tools for the `gsym-rs` library.

#![warn(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

mod bulk;
mod cli;
mod terminal;

#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::borrow::Cow;
use std::fmt;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use clap::CommandFactory;
use gsym::convert::{
    ConversionOptions, ConversionWarning, DiscoveryPolicy, DwarfImportOptions, ElfConverter,
    ElfInputs,
};
use gsym::{AddressRange, Gsym, GsymBuilder, LookupOptions, LookupScratch, TranscodeOptions};

use crate::cli::{
    Cli, CliEndian, CliVersion, Command, ConvertArgs, DumpArgs, DwarfToggles, HumanBytes,
    InputArgs, LookupArgs, SegmentArgs, SourceToggles, TranscodeArgs,
};
use crate::terminal::{ACCENT, MUTED, NAME, Terminal};

fn main() -> ExitCode {
    let Cli {
        color,
        quiet,
        verbose,
        command,
    } = Cli::parse_styled();
    let mut terminal = Terminal::new(color.into(), quiet, verbose);
    match run(command, &mut terminal) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            drop(terminal.error(&error));
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command, terminal: &mut Terminal) -> Result<()> {
    match command {
        Command::Convert(arguments) => convert(arguments, terminal),
        Command::Transcode(arguments) => transcode(&arguments, terminal),
        Command::Segment(arguments) => segment(&arguments, terminal),
        Command::Lookup(arguments) => lookup(arguments, terminal),
        Command::Dump(arguments) => dump(&arguments, terminal),
        Command::Verify(arguments) => verify(&arguments, terminal),
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "gsymtool", terminal.stdout());
            Ok(())
        }
    }
}

fn convert(arguments: ConvertArgs, terminal: &mut Terminal) -> Result<()> {
    let ConvertArgs {
        inputs,
        output,
        output_dir,
        recursive,
        jobs,
        debug,
        symbols,
        supplementary,
        dwp,
        version,
        sources,
        dwarf,
    } = arguments;
    let converter = ElfConverter::new(conversion_options(version, &sources, &dwarf));
    if let Some(directory) = output_dir {
        return bulk::run(
            &converter,
            bulk::Batch {
                inputs: &inputs,
                output_dir: &directory,
                recursive,
                jobs,
            },
            terminal,
        );
    }
    let output = output.context("either --output or --output-dir is required")?;
    let [input] = inputs.as_slice() else {
        bail!("--output converts a single ELF file; use --output-dir to convert several");
    };
    let companions = Companions {
        debug: debug.as_deref(),
        symbols: symbols.as_deref(),
        supplementary: supplementary.as_deref(),
        dwp: dwp.as_deref(),
    };
    convert_single(&converter, input, &output, companions, terminal)
}

fn conversion_options(
    version: CliVersion,
    sources: &SourceToggles,
    dwarf: &DwarfToggles,
) -> ConversionOptions {
    let mut options = ConversionOptions::default();
    options.writer.version = version.into();
    options.include_symbols = !sources.no_symbols;
    options.dwarf = (!sources.no_dwarf).then_some(DwarfImportOptions {
        inline_info: !dwarf.no_inline,
        call_sites: dwarf.call_sites,
    });
    apply_discovery_toggles(&mut options, &sources.discovery);
    options
}

fn apply_discovery_toggles(options: &mut ConversionOptions, toggles: &cli::DiscoveryToggles) {
    if toggles.no_discovery {
        options.discovery = DiscoveryPolicy::Disabled;
        options.debuginfod_urls.clear();
    } else if toggles.no_debuginfod {
        options.debuginfod_urls.clear();
    }
}

fn convert_single(
    converter: &ElfConverter,
    input: &Path,
    output: &Path,
    companions: Companions<'_>,
    terminal: &mut Terminal,
) -> Result<()> {
    let started = Instant::now();
    if input.is_dir() {
        bail!(
            "{} is a directory; use --output-dir to convert the ELF files it holds",
            input.display()
        );
    }
    let report = run_conversion(converter, input, companions)?;
    let stats = report.stats;
    let format = report.builder.options().writer.version;
    let endian = report.builder.options().writer.endian;
    let candidate_functions = report.builder.functions().len();
    let source_files = report.builder.files().len().saturating_sub(1);
    let warning_count = report.warnings.len();
    print_conversion_warnings(&report.warnings, terminal)?;

    let output_size = persist_builder(report.builder, output)?;
    terminal.success(format_args!("wrote {}", output.display()))?;
    if let Some(stderr) = terminal.summary() {
        writeln!(
            stderr,
            "  output       {} | GSYM {} | {} endian",
            HumanBytes(output_size),
            format,
            endian_name(endian)
        )?;
        if let Ok(metadata) = std::fs::metadata(input) {
            writeln!(
                stderr,
                "  source       {} ELF -> {}",
                HumanBytes(metadata.len()),
                SizeRatio {
                    output: output_size,
                    input: metadata.len(),
                }
            )?;
        }
        writeln!(
            stderr,
            "  functions    {} candidates | {} symbols | {} DWARF",
            Count(candidate_functions),
            Count(stats.symbol_functions),
            Count(stats.dwarf_functions)
        )?;
        if source_files != 0
            || stats.line_rows != 0
            || stats.inline_nodes != 0
            || stats.split_dwarf_units != 0
        {
            writeln!(
                stderr,
                "  debug info   {} source files | {} line rows | {} inline calls | {} split units",
                Count(source_files),
                Count(stats.line_rows),
                Count(stats.inline_nodes),
                Count(stats.split_dwarf_units)
            )?;
        }
        if stats.rejected_ranges != 0 {
            writeln!(
                stderr,
                "  filtered     {} dead, invalid, or unrepresentable ranges",
                Count(stats.rejected_ranges)
            )?;
        }
        if warning_count != 0 {
            writeln!(stderr, "  diagnostics  {} warnings", Count(warning_count))?;
        }
        if let Some(path) = report.discovered_debug.as_deref().or(companions.debug) {
            writeln!(stderr, "  debug file   {}", path.display())?;
        }
        if let Some(path) = report
            .discovered_supplementary
            .as_deref()
            .or(companions.supplementary)
        {
            writeln!(stderr, "  supplement   {}", path.display())?;
        }
        if let Some(path) = report.discovered_dwp.as_deref().or(companions.dwp) {
            writeln!(stderr, "  DWP package  {}", path.display())?;
        }
        writeln!(stderr, "  elapsed      {}", Elapsed(started.elapsed()))?;
    }
    Ok(())
}

const REJECTED_RANGE_EXAMPLES: usize = 3;

#[derive(Default)]
struct RejectedRangeSummary {
    count: usize,
    examples: Vec<AddressRange>,
}

impl RejectedRangeSummary {
    fn collect(warnings: &[ConversionWarning]) -> Self {
        let mut summary = Self::default();
        for warning in warnings {
            let ConversionWarning::RejectedRange { range } = warning else {
                continue;
            };
            summary.count = summary.count.saturating_add(1);
            if summary.examples.len() < REJECTED_RANGE_EXAMPLES && !summary.examples.contains(range)
            {
                summary.examples.push(*range);
            }
        }
        summary
    }
}

impl fmt::Display for RejectedRangeSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "skipping {} non-live or invalid DWARF range{}",
            Count(self.count),
            if self.count == 1 { "" } else { "s" }
        )?;
        if !self.examples.is_empty() {
            formatter.write_str("; examples: ")?;
            for (index, range) in self.examples.iter().enumerate() {
                if index != 0 {
                    formatter.write_str(", ")?;
                }
                write!(formatter, "{:#x}..{:#x}", range.start, range.end)?;
            }
        }
        formatter.write_str("; pass --verbose to show each unexpected rejected range")
    }
}

fn print_conversion_warnings(
    warnings: &[ConversionWarning],
    terminal: &mut Terminal,
) -> std::io::Result<()> {
    if terminal.is_quiet() {
        return Ok(());
    }
    if terminal.is_verbose() {
        for warning in warnings {
            terminal.warning(format_args!("{warning}"))?;
        }
        return Ok(());
    }

    for warning in warnings {
        if !matches!(warning, ConversionWarning::RejectedRange { .. }) {
            terminal.warning(format_args!("{warning}"))?;
        }
    }
    let rejected = RejectedRangeSummary::collect(warnings);
    if rejected.count != 0 {
        terminal.warning(format_args!("{rejected}"))?;
    }
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct Companions<'a> {
    debug: Option<&'a Path>,
    symbols: Option<&'a Path>,
    supplementary: Option<&'a Path>,
    dwp: Option<&'a Path>,
}

fn run_conversion(
    converter: &ElfConverter,
    input: &Path,
    companions: Companions<'_>,
) -> Result<gsym::convert::ConversionReport> {
    let Companions {
        debug,
        symbols,
        supplementary,
        dwp,
    } = companions;
    if debug.is_none() && symbols.is_none() && supplementary.is_none() && dwp.is_none() {
        return Ok(converter.convert_path(input)?);
    }
    let image = read_file(input)?;
    let debug_data = read_optional(debug)?;
    let symbol_data = read_optional(symbols)?;
    let supplementary_data = read_optional(supplementary)?;
    let dwp_data = read_optional(dwp)?;
    Ok(converter.convert(ElfInputs {
        image: &image,
        debug: debug_data.as_deref(),
        symbols: symbol_data.as_deref(),
        supplementary: supplementary_data.as_deref(),
        dwp: dwp_data.as_deref(),
    })?)
}

fn transform_options(version: Option<CliVersion>, endian: Option<CliEndian>) -> TranscodeOptions {
    TranscodeOptions {
        version: version.map(Into::into),
        endian: endian.map(Into::into),
    }
}

fn transcode(arguments: &TranscodeArgs, terminal: &mut Terminal) -> Result<()> {
    let options = transform_options(arguments.version, arguments.endian);
    let source = read_file(&arguments.input)?;
    let bytes = Gsym::parse(&source)?.transcode(options)?;
    persist_bytes(&arguments.output, &bytes)?;
    terminal.success(format_args!(
        "wrote {} ({})",
        arguments.output.display(),
        HumanBytes(bytes.len() as u64)
    ))?;
    Ok(())
}

fn segment(arguments: &SegmentArgs, terminal: &mut Terminal) -> Result<()> {
    let options = transform_options(arguments.version, arguments.endian);
    let source = read_file(&arguments.input)?;
    let decoded = Gsym::parse(&source)?.decode_all()?;
    let segments = decoded.segments(arguments.size.get(), options)?;
    for segment in &segments {
        let path = shard_path(&arguments.output, segment.first_address);
        persist_bytes(&path, segment.bytes())?;
        terminal.success(format_args!(
            "wrote {} ({} functions, {})",
            path.display(),
            segment.function_count,
            HumanBytes(segment.bytes().len() as u64)
        ))?;
    }
    Ok(())
}

fn shard_path(prefix: &Path, first_address: u64) -> PathBuf {
    let mut name = prefix.as_os_str().to_os_string();
    name.push(format!("-{first_address:#x}"));
    PathBuf::from(name)
}

fn persist_builder(builder: GsymBuilder, path: &Path) -> Result<u64> {
    let mut temporary = create_output(path)?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        builder.write_to(&mut writer)?;
        writer.flush()?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(std::fs::metadata(path)?.len())
}

fn persist_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut temporary = create_output(path)?;
    temporary.as_file_mut().write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn lookup(arguments: LookupArgs, terminal: &mut Terminal) -> Result<()> {
    let input = read_file(&arguments.input)?;
    let gsym = Gsym::parse(&input)?;
    let options = LookupOptions {
        line_information: !arguments.no_lines,
        inline_frames: !arguments.no_inline,
        call_sites: !arguments.no_call_sites,
    };
    let mut scratch = LookupScratch::default();
    let stdout = terminal.stdout();
    for address in arguments.addresses {
        match gsym.lookup_with_options(address, options, &mut scratch)? {
            Some(result) => {
                writeln!(stdout, "{ACCENT}{address:#018x}:{}", ACCENT.render_reset())?;
                for frame in result.frames() {
                    write!(
                        stdout,
                        "  {NAME}{}{}",
                        bytes(frame.name),
                        NAME.render_reset()
                    )?;
                    if !frame.basename.is_empty() {
                        write!(stdout, " {MUTED}at{} ", MUTED.render_reset())?;
                        if !frame.directory.is_empty() {
                            write!(stdout, "{}/", bytes(frame.directory))?;
                        }
                        write!(stdout, "{}:{}", bytes(frame.basename), frame.line)?;
                    }
                    if frame.inlined {
                        write!(stdout, " {MUTED}[inlined]{}", MUTED.render_reset())?;
                    }
                    writeln!(stdout)?;
                }
                if !result.call_site_patterns().is_empty() {
                    write!(stdout, "  {MUTED}possible callees:{}", MUTED.render_reset())?;
                    for pattern in result.call_site_patterns() {
                        write!(stdout, " {}", bytes(pattern))?;
                    }
                    writeln!(stdout)?;
                }
            }
            None => writeln!(
                stdout,
                "{ACCENT}{address:#018x}:{} {MUTED}<no match>{}",
                ACCENT.render_reset(),
                MUTED.render_reset()
            )?,
        }
    }
    Ok(())
}

fn dump(arguments: &DumpArgs, terminal: &mut Terminal) -> Result<()> {
    let input = read_file(&arguments.input)?;
    let gsym = Gsym::parse(&input)?;
    let header = gsym.header();
    let stdout = terminal.stdout();
    writeln!(
        stdout,
        "{ACCENT}GSYM {}{} | {} endian",
        header.version,
        ACCENT.render_reset(),
        match header.endian {
            gsym::Endian::Little => "little",
            gsym::Endian::Big => "big",
        }
    )?;
    writeln!(
        stdout,
        "  file size       {}",
        HumanBytes(input.len() as u64)
    )?;
    writeln!(stdout, "  base address    {:#018x}", header.base_address)?;
    writeln!(stdout, "  functions       {}", header.address_count)?;
    writeln!(stdout, "  address width   {} B", header.address_offset_size)?;
    if header.build_id.is_empty() {
        writeln!(
            stdout,
            "  build ID        {MUTED}<none>{}",
            MUTED.render_reset()
        )?;
    } else {
        writeln!(stdout, "  build ID        {}", hex::encode(header.build_id))?;
    }
    if arguments.functions {
        writeln!(stdout)?;
        writeln!(stdout, "{ACCENT}Functions{}", ACCENT.render_reset())?;
        let limit = arguments.limit.unwrap_or(usize::MAX);
        for (index, function) in gsym.functions().take(limit).enumerate() {
            let function = function?;
            let range = function.range();
            writeln!(
                stdout,
                "  {index:>6}  {:#018x}..{:#018x}  {NAME}{}{}",
                range.start,
                range.end,
                bytes(function.name()),
                NAME.render_reset()
            )?;
        }
    }
    Ok(())
}

fn verify(arguments: &InputArgs, terminal: &mut Terminal) -> Result<()> {
    let bytes = read_file(&arguments.input)?;
    let report = Gsym::parse(&bytes)?.verify()?;
    let stdout = terminal.stdout();
    writeln!(
        stdout,
        "{NAME}valid:{} {}",
        NAME.render_reset(),
        arguments.input.display()
    )?;
    writeln!(stdout, "  functions       {}", report.functions)?;
    writeln!(stdout, "  source files    {}", report.files)?;
    writeln!(stdout, "  strings         {}", report.strings)?;
    writeln!(
        stdout,
        "  function data   {}",
        HumanBytes(report.function_info_bytes as u64)
    )?;
    Ok(())
}

fn bytes(value: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(value)
}

struct Count(usize);

impl fmt::Display for Count {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_count(formatter, self.0)
    }
}

fn write_count(formatter: &mut fmt::Formatter<'_>, value: usize) -> fmt::Result {
    if value < 1_000 {
        write!(formatter, "{value}")
    } else {
        write_count(formatter, value / 1_000)?;
        write!(formatter, ",{:03}", value % 1_000)
    }
}

struct SizeRatio {
    output: u64,
    input: u64,
}

impl SizeRatio {
    fn tenths(&self) -> Option<u128> {
        let input = u128::from(self.input);
        u128::from(self.output)
            .checked_mul(1000)?
            .checked_add(input.checked_div(2)?)?
            .checked_div(input)
    }
}

impl fmt::Display for SizeRatio {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(tenths) = self.tenths() else {
            return formatter.write_str("unknown ratio");
        };
        write!(
            formatter,
            "{}.{}% of input",
            tenths.checked_div(10).unwrap_or(0),
            tenths.checked_rem(10).unwrap_or(0)
        )
    }
}

struct Elapsed(Duration);

impl fmt::Display for Elapsed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 < Duration::from_secs(1) {
            write!(formatter, "{} ms", self.0.as_millis())
        } else if self.0 < Duration::from_secs(60) {
            write!(formatter, "{:.2} s", self.0.as_secs_f64())
        } else {
            write!(formatter, "{:.1} min", self.0.as_secs_f64() / 60.0)
        }
    }
}

const fn endian_name(endian: gsym::Endian) -> &'static str {
    match endian {
        gsym::Endian::Little => "little",
        gsym::Endian::Big => "big",
    }
}

fn read_optional(path: Option<&Path>) -> Result<Option<Vec<u8>>> {
    path.map(read_file).transpose()
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}

fn create_output(path: &Path) -> Result<tempfile::NamedTempFile> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    tempfile::NamedTempFile::new_in(directory)
        .with_context(|| format!("failed to create output beside {}", path.display()))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn discovery_toggles_update_the_conversion_policy() {
        let mut options = ConversionOptions {
            debuginfod_urls: vec!["https://debuginfod.example".to_owned()],
            ..ConversionOptions::default()
        };
        apply_discovery_toggles(
            &mut options,
            &cli::DiscoveryToggles {
                no_discovery: false,
                no_debuginfod: false,
            },
        );
        assert_eq!(options.debuginfod_urls.len(), 1);

        apply_discovery_toggles(
            &mut options,
            &cli::DiscoveryToggles {
                no_discovery: false,
                no_debuginfod: true,
            },
        );
        assert!(options.debuginfod_urls.is_empty());

        options.debuginfod_urls = vec!["https://debuginfod.example".to_owned()];
        apply_discovery_toggles(
            &mut options,
            &cli::DiscoveryToggles {
                no_discovery: true,
                no_debuginfod: false,
            },
        );
        assert_eq!(options.discovery, DiscoveryPolicy::Disabled);
        assert!(options.debuginfod_urls.is_empty());
    }

    #[test]
    fn renders_binary_names_and_build_ids_without_intermediate_strings() {
        assert_eq!(bytes(b"hello"), "hello");
        assert_eq!(bytes(&[0xff]), "\u{fffd}");
        assert_eq!(hex::encode([0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

    #[test]
    fn rejected_range_summary_counts_all_ranges_and_keeps_distinct_examples() {
        let rejected = |start, end| ConversionWarning::RejectedRange {
            range: AddressRange::new(start, end),
        };
        let warnings = [
            rejected(0, 0x1f),
            rejected(0, 0x1f),
            rejected(0, 2),
            ConversionWarning::MalformedRanges {
                stopped: false,
                reason: "bad range list".into(),
            },
            rejected(0, 0x32),
            rejected(0, 0x40),
        ];

        let summary = RejectedRangeSummary::collect(&warnings);

        assert_eq!(
            summary.to_string(),
            "skipping 5 non-live or invalid DWARF ranges; examples: 0x0..0x1f, 0x0..0x2, 0x0..0x32; pass --verbose to show each unexpected rejected range"
        );
    }

    #[test]
    fn size_ratio_rounds_to_the_nearest_tenth_without_saturating() {
        assert_eq!(
            SizeRatio {
                output: 1999,
                input: 10_000,
            }
            .to_string(),
            "20.0% of input"
        );
        assert_eq!(
            SizeRatio {
                output: 1994,
                input: 10_000,
            }
            .to_string(),
            "19.9% of input"
        );
        assert_eq!(
            SizeRatio {
                output: 20_000_000_000_000_000,
                input: 40_000_000_000_000_000,
            }
            .to_string(),
            "50.0% of input"
        );
        assert_eq!(
            SizeRatio {
                output: u64::MAX,
                input: u64::MAX,
            }
            .to_string(),
            "100.0% of input"
        );
        assert_eq!(
            SizeRatio {
                output: 4096,
                input: 0,
            }
            .to_string(),
            "unknown ratio"
        );
    }

    #[test]
    #[cfg(unix)]
    fn segment_shard_paths_keep_every_prefix_byte() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let prefix = PathBuf::from(OsStr::from_bytes(b"out/sh\xffard.gsym"));
        let path = shard_path(&prefix, 0x1000);
        assert_eq!(path.as_os_str().as_bytes(), b"out/sh\xffard.gsym-0x1000");
        assert_eq!(
            shard_path(Path::new("app.gsym"), 0x40_1120)
                .to_str()
                .unwrap(),
            "app.gsym-0x401120"
        );
    }

    #[test]
    fn command_line_accepts_separate_debug_dwp_and_v2() {
        let Ok(cli) = Cli::try_parse_from([
            "gsymtool",
            "convert",
            "app",
            "--output",
            "app.gsym",
            "--debug",
            "app.debug",
            "--dwp",
            "app.dwp",
            "--version",
            "v2",
        ]) else {
            panic!("valid conversion command was rejected");
        };
        let Command::Convert(arguments) = cli.command else {
            panic!("conversion command parsed as a different subcommand");
        };
        assert_eq!(arguments.inputs, [PathBuf::from("app")]);
        assert_eq!(arguments.output, Some(PathBuf::from("app.gsym")));
        assert_eq!(arguments.debug, Some(PathBuf::from("app.debug")));
        assert_eq!(arguments.dwp, Some(PathBuf::from("app.dwp")));
        assert!(matches!(arguments.version, CliVersion::V2));
    }
}
