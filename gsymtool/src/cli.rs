use std::fmt;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::str::FromStr;

use anstyle::{AnsiColor, Effects};
use clap::builder::Styles;
use clap::{
    ArgGroup, Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum, ValueHint,
};
use clap_complete::Shell;

use gsym::{Endian, GsymVersion};

const HELP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Yellow.on_default());

#[derive(Debug, Parser)]
#[command(
    name = "gsymtool",
    version,
    about = "Create, inspect, and query LLVM GSYM files",
    long_about = "A pure-Rust command-line tool for creating, transforming, inspecting, and querying LLVM GSYM symbol files.",
    arg_required_else_help = true,
    styles = HELP_STYLES,
    max_term_width = 100
)]
pub(crate) struct Cli {
    /// Control colors in diagnostics and command output.
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t = ColorMode::Auto,
        help_heading = "Global options"
    )]
    pub(crate) color: ColorMode,

    /// Suppress successful write summaries.
    #[arg(short, long, global = true, help_heading = "Global options")]
    pub(crate) quiet: bool,

    #[command(subcommand)]
    pub(crate) command: Command,
}

impl Cli {
    pub(crate) fn parse_styled() -> Self {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        let color = requested_help_color(&arguments);
        let matches = Self::command().color(color).get_matches_from(arguments);
        Self::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
    }
}

fn requested_help_color(arguments: &[std::ffi::OsString]) -> clap::ColorChoice {
    let mut choice = clap::ColorChoice::Auto;
    let mut arguments = arguments.iter().skip(1);
    while let Some(argument) = arguments.next().and_then(|value| value.to_str()) {
        if argument == "--" {
            break;
        }
        let value = argument.strip_prefix("--color=").or_else(|| {
            (argument == "--color")
                .then(|| arguments.next()?.to_str())
                .flatten()
        });
        choice = match value {
            Some("always") => clap::ColorChoice::Always,
            Some("never") => clap::ColorChoice::Never,
            Some("auto") => clap::ColorChoice::Auto,
            _ => choice,
        };
    }
    choice
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Convert Linux ELF symbols and DWARF to GSYM.
    #[command(
        verbatim_doc_comment,
        after_help = "Examples:\n  gsymtool convert ./app -o ./app.gsym\n  gsymtool convert ./app -o ./app.gsym --version v2 --dwp ./app.dwp\n  gsymtool convert ./bin --output-dir ./gsym --recursive\n  gsymtool convert ./a.so ./b.so --output-dir ./gsym --jobs 8"
    )]
    Convert(ConvertArgs),

    /// Re-encode a GSYM file without returning to ELF/DWARF.
    #[command(
        visible_alias = "rewrite",
        after_help = "Example:\n  gsymtool transcode app.gsym -o app-v2.gsym --version v2 --endian big"
    )]
    Transcode(TranscodeArgs),

    /// Split a GSYM file into independently readable address shards.
    #[command(
        after_help = "SIZE accepts B, KiB, MiB, GiB, KB, MB, or GB.\n\nExample:\n  gsymtool segment app.gsym -o app-shard.gsym --size 64MiB"
    )]
    Segment(SegmentArgs),

    /// Symbolize one or more unslid virtual addresses.
    #[command(
        visible_alias = "symbolize",
        after_help = "Example:\n  gsymtool lookup app.gsym 0x401120 0x40113a"
    )]
    Lookup(LookupArgs),

    /// Display GSYM metadata and optionally its function index.
    #[command(
        visible_alias = "info",
        after_help = "Example:\n  gsymtool dump app.gsym --functions --limit 20"
    )]
    Dump(DumpArgs),

    /// Fully validate every record in a standalone GSYM file.
    #[command(visible_alias = "check")]
    Verify(InputArgs),

    /// Generate shell completion scripts.
    #[command(
        after_help = "Examples:\n  gsymtool completions zsh > ~/.zfunc/_gsymtool\n  gsymtool completions bash > gsymtool.bash"
    )]
    Completions {
        /// Shell whose completion syntax should be generated.
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Debug, Args)]
#[command(group = ArgGroup::new("destination").args(["output", "output_dir"]).required(true))]
pub(crate) struct ConvertArgs {
    /// `ET_EXEC`, `ET_DYN`, or `ET_REL` Linux ELF inputs, or directories of them.
    #[arg(required = true, value_name = "PATH", value_hint = ValueHint::AnyPath)]
    pub(crate) inputs: Vec<PathBuf>,

    /// Destination GSYM file for one ELF input, replaced atomically.
    #[arg(short, long, value_name = "GSYM", value_hint = ValueHint::FilePath)]
    pub(crate) output: Option<PathBuf>,

    /// Destination root for a batch, mirroring every directory scanned.
    ///
    /// Each converted ELF keeps its name with `.gsym` appended, so `bin/sub/app`
    /// becomes `DIR/sub/app.gsym`. Files that are not ELF images are skipped.
    #[arg(
        long,
        value_name = "DIR",
        value_hint = ValueHint::DirPath,
        conflicts_with_all = ["debug", "symbols", "supplementary", "dwp"]
    )]
    pub(crate) output_dir: Option<PathBuf>,

    /// Descend into subdirectories of directory inputs.
    #[arg(short, long, conflicts_with = "output")]
    pub(crate) recursive: bool,

    /// Conversions to run at once; defaults to the available parallelism.
    ///
    /// Each job holds one image and its DWARF in memory, so lowering this bounds
    /// peak memory when converting large binaries.
    #[arg(short, long, value_name = "N", conflicts_with = "output")]
    pub(crate) jobs: Option<NonZeroUsize>,

    /// Separate ELF debug file.
    #[arg(long, value_name = "ELF", value_hint = ValueHint::FilePath)]
    pub(crate) debug: Option<PathBuf>,

    /// ELF file from which to load the symbol table.
    #[arg(long, value_name = "ELF", value_hint = ValueHint::FilePath)]
    pub(crate) symbols: Option<PathBuf>,

    /// Supplementary DWARF object referenced by the main debug file.
    #[arg(long, value_name = "ELF", value_hint = ValueHint::FilePath)]
    pub(crate) supplementary: Option<PathBuf>,

    /// Packaged split-DWARF input.
    #[arg(long, value_name = "DWP", value_hint = ValueHint::FilePath)]
    pub(crate) dwp: Option<PathBuf>,

    /// GSYM wire-format version.
    #[arg(long, value_enum, default_value_t = CliVersion::V1)]
    pub(crate) version: CliVersion,

    #[command(flatten)]
    pub(crate) sources: SourceToggles,

    #[command(flatten)]
    pub(crate) dwarf: DwarfToggles,
}

/// Which of the available inputs the conversion imports.
#[derive(Debug, Args)]
pub(crate) struct SourceToggles {
    /// Do not import ELF symbol-table functions.
    #[arg(long)]
    pub(crate) no_symbols: bool,

    /// Do not import DWARF functions, source lines, or inline records.
    #[arg(
        long,
        conflicts_with_all = ["debug", "supplementary", "dwp", "no_inline", "call_sites"]
    )]
    pub(crate) no_dwarf: bool,

    /// Do not search debug links, build-ID roots, DWO/DWP paths, or debuginfod.
    #[arg(long)]
    pub(crate) no_discovery: bool,
}

/// How much per-function detail the DWARF import keeps.
#[derive(Debug, Args)]
pub(crate) struct DwarfToggles {
    /// Omit DWARF inline-call trees.
    #[arg(long)]
    pub(crate) no_inline: bool,

    /// Import DWARF call-site records.
    #[arg(long)]
    pub(crate) call_sites: bool,
}

#[derive(Debug, Args)]
pub(crate) struct TranscodeArgs {
    /// Existing standalone GSYM file.
    #[arg(value_name = "INPUT", value_hint = ValueHint::FilePath)]
    pub(crate) input: PathBuf,

    /// Re-encoded GSYM destination, replaced atomically.
    #[arg(short, long, value_name = "OUTPUT", value_hint = ValueHint::FilePath)]
    pub(crate) output: PathBuf,

    /// Output version; preserves the input version when omitted.
    #[arg(long, value_enum)]
    pub(crate) version: Option<CliVersion>,

    /// Output byte order; preserves the input order when omitted.
    #[arg(long, value_enum)]
    pub(crate) endian: Option<CliEndian>,
}

#[derive(Debug, Args)]
pub(crate) struct SegmentArgs {
    /// Existing standalone GSYM file.
    #[arg(value_name = "INPUT", value_hint = ValueHint::FilePath)]
    pub(crate) input: PathBuf,

    /// Output prefix; each filename receives `-0x<first-address>`.
    #[arg(short, long, value_name = "PREFIX", value_hint = ValueHint::FilePath)]
    pub(crate) output: PathBuf,

    /// Approximate maximum shard size.
    #[arg(long, value_name = "SIZE", default_value = "64MiB")]
    pub(crate) size: ByteSize,

    /// Output version; preserves the input version when omitted.
    #[arg(long, value_enum)]
    pub(crate) version: Option<CliVersion>,

    /// Output byte order; preserves the input order when omitted.
    #[arg(long, value_enum)]
    pub(crate) endian: Option<CliEndian>,
}

#[derive(Debug, Args)]
pub(crate) struct LookupArgs {
    /// Standalone GSYM file.
    #[arg(value_name = "GSYM", value_hint = ValueHint::FilePath)]
    pub(crate) input: PathBuf,

    /// Decimal or `0x`-prefixed unslid virtual addresses.
    #[arg(required = true, value_name = "ADDRESS", value_parser = parse_address)]
    pub(crate) addresses: Vec<u64>,

    /// Omit source paths and line numbers.
    #[arg(long)]
    pub(crate) no_lines: bool,

    /// Omit inline-call frames.
    #[arg(long)]
    pub(crate) no_inline: bool,

    /// Omit matching call-site patterns.
    #[arg(long)]
    pub(crate) no_call_sites: bool,
}

#[derive(Debug, Args)]
pub(crate) struct DumpArgs {
    /// Standalone GSYM file.
    #[arg(value_name = "GSYM", value_hint = ValueHint::FilePath)]
    pub(crate) input: PathBuf,

    /// Include the function address index.
    #[arg(short, long)]
    pub(crate) functions: bool,

    /// Maximum number of functions to print.
    #[arg(long, value_name = "COUNT", requires = "functions")]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct InputArgs {
    /// Standalone GSYM file.
    #[arg(value_name = "GSYM", value_hint = ValueHint::FilePath)]
    pub(crate) input: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum ColorMode {
    /// Color only when writing to a capable terminal.
    #[default]
    Auto,
    /// Always emit ANSI colors.
    Always,
    /// Never emit ANSI colors.
    Never,
}

impl From<ColorMode> for anstream::ColorChoice {
    fn from(value: ColorMode) -> Self {
        match value {
            ColorMode::Auto => Self::Auto,
            ColorMode::Always => Self::Always,
            ColorMode::Never => Self::Never,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub(crate) enum CliVersion {
    #[default]
    V1,
    V2,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum CliEndian {
    Little,
    Big,
}

impl From<CliEndian> for Endian {
    fn from(value: CliEndian) -> Self {
        match value {
            CliEndian::Little => Self::Little,
            CliEndian::Big => Self::Big,
        }
    }
}

impl From<CliVersion> for GsymVersion {
    fn from(value: CliVersion) -> Self {
        match value {
            CliVersion::V1 => Self::V1,
            CliVersion::V2 => Self::V2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ByteSize(usize);

impl ByteSize {
    pub(crate) const fn get(self) -> usize {
        self.0
    }
}

impl FromStr for ByteSize {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let compact = value.replace('_', "");
        let split = compact
            .find(|character: char| !character.is_ascii_digit())
            .unwrap_or(compact.len());
        let (digits, suffix) = compact.split_at(split);
        let count = digits
            .parse::<u128>()
            .map_err(|_| format!("invalid byte size `{value}`"))?;
        if count == 0 {
            return Err("byte size must be greater than zero".to_owned());
        }
        let multiplier = match suffix.to_ascii_lowercase().as_str() {
            "" | "b" => 1_u128,
            "k" | "kb" => 1_000,
            "ki" | "kib" => 1 << 10,
            "m" | "mb" => 1_000_000,
            "mi" | "mib" => 1 << 20,
            "g" | "gb" => 1_000_000_000,
            "gi" | "gib" => 1 << 30,
            _ => {
                return Err(format!(
                    "invalid byte unit in `{value}`; use B, KiB, MiB, GiB, KB, MB, or GB"
                ));
            }
        };
        let bytes = count
            .checked_mul(multiplier)
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or_else(|| format!("byte size `{value}` does not fit this platform"))?;
        Ok(Self(bytes))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_byte_size(formatter, self.0 as u64)
    }
}

pub(crate) struct HumanBytes(pub(crate) u64);

impl fmt::Display for HumanBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_byte_size(formatter, self.0)
    }
}

fn write_byte_size(formatter: &mut fmt::Formatter<'_>, bytes: u64) -> fmt::Result {
    const UNITS: [(&str, u64); 4] = [
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
        ("B", 1),
    ];
    let &(unit, divisor) = UNITS
        .iter()
        .find(|(_, divisor)| bytes >= *divisor)
        .unwrap_or(&UNITS[3]);
    let whole = bytes.checked_div(divisor).unwrap_or(0);
    if divisor == 1 || bytes.is_multiple_of(divisor) {
        write!(formatter, "{whole} {unit}")
    } else {
        let remainder = bytes.checked_rem(divisor).unwrap_or(0);
        let tenths = remainder
            .saturating_mul(10)
            .checked_div(divisor)
            .unwrap_or(0);
        write!(formatter, "{whole}.{tenths} {unit}")
    }
}

pub(crate) fn parse_address(value: &str) -> Result<u64, String> {
    let compact: String = value
        .chars()
        .filter(|character| *character != '_')
        .collect();
    let (digits, radix) = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
        .map_or((compact.as_str(), 10), |digits| (digits, 16));
    if digits.is_empty() {
        return Err(format!("invalid address `{value}`"));
    }
    u64::from_str_radix(digits, radix).map_err(|_| format!("invalid address `{value}`"))
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;
    use clap::{CommandFactory, Parser};

    use super::*;

    #[test]
    fn parses_decimal_and_hex_addresses() {
        assert_eq!(parse_address("4096"), Ok(0x1000));
        assert_eq!(parse_address("0x1000"), Ok(0x1000));
        assert_eq!(parse_address("0Xffff_ffff"), Ok(u64::from(u32::MAX)));
    }

    #[test]
    fn rejects_empty_and_out_of_range_addresses() {
        assert_eq!(parse_address(""), Err("invalid address ``".to_owned()));
        assert_eq!(parse_address("0x"), Err("invalid address `0x`".to_owned()));
        assert_eq!(
            parse_address("0x1_0000_0000_0000_0000"),
            Err("invalid address `0x1_0000_0000_0000_0000`".to_owned())
        );
    }

    #[test]
    fn parses_and_renders_human_byte_sizes() {
        assert_eq!("64MiB".parse(), Ok(ByteSize(64 << 20)));
        assert_eq!("32_768".parse(), Ok(ByteSize(32_768)));
        assert_eq!("4MB".parse(), Ok(ByteSize(4_000_000)));
        assert_eq!(ByteSize(64 << 20).to_string(), "64 MiB");
        assert_eq!(ByteSize(1536).to_string(), "1.5 KiB");
    }

    #[test]
    fn rejects_zero_unknown_and_overflowing_byte_sizes() {
        assert_eq!(
            "0".parse::<ByteSize>(),
            Err("byte size must be greater than zero".to_owned())
        );
        assert_eq!(
            "4watts".parse::<ByteSize>(),
            Err("invalid byte unit in `4watts`; use B, KiB, MiB, GiB, KB, MB, or GB".to_owned())
        );
        assert_eq!(
            "999999999999999999999999GiB".parse::<ByteSize>(),
            Err("byte size `999999999999999999999999GiB` does not fit this platform".to_owned())
        );
    }

    #[test]
    fn top_level_help_exposes_global_controls_and_aliases() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("--color"));
        assert!(help.contains("--quiet"));
        assert!(help.contains("symbolize"));
        assert!(help.contains("completions"));
    }

    #[test]
    fn lookup_requires_an_address() {
        let error = Cli::try_parse_from(["gsymtool", "lookup", "input.gsym"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn conversion_requires_exactly_one_destination() {
        let missing = Cli::try_parse_from(["gsymtool", "convert", "app"]).unwrap_err();
        assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);

        let both = Cli::try_parse_from([
            "gsymtool",
            "convert",
            "app",
            "-o",
            "app.gsym",
            "--output-dir",
            "out",
        ])
        .unwrap_err();
        assert_eq!(both.kind(), ErrorKind::ArgumentConflict);

        let no_input =
            Cli::try_parse_from(["gsymtool", "convert", "--output-dir", "out"]).unwrap_err();
        assert_eq!(no_input.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn batch_options_require_a_directory_and_exclude_companions() {
        let recursive = Cli::try_parse_from(["gsymtool", "convert", "bin", "-o", "app.gsym", "-r"])
            .unwrap_err();
        assert_eq!(recursive.kind(), ErrorKind::ArgumentConflict);

        let jobs = Cli::try_parse_from(["gsymtool", "convert", "bin", "-o", "app.gsym", "-j", "4"])
            .unwrap_err();
        assert_eq!(jobs.kind(), ErrorKind::ArgumentConflict);

        let companion = Cli::try_parse_from([
            "gsymtool",
            "convert",
            "bin",
            "--output-dir",
            "out",
            "--debug",
            "app.debug",
        ])
        .unwrap_err();
        assert_eq!(companion.kind(), ErrorKind::ArgumentConflict);

        let zero_jobs = Cli::try_parse_from([
            "gsymtool",
            "convert",
            "bin",
            "--output-dir",
            "out",
            "--jobs",
            "0",
        ])
        .unwrap_err();
        assert_eq!(zero_jobs.kind(), ErrorKind::ValueValidation);
    }

    #[test]
    fn batch_conversion_accepts_several_inputs_and_a_job_count() {
        let Ok(cli) = Cli::try_parse_from([
            "gsymtool",
            "convert",
            "a.so",
            "bin",
            "--output-dir",
            "out",
            "--recursive",
            "--jobs",
            "8",
        ]) else {
            panic!("valid batch command was rejected");
        };
        let Command::Convert(arguments) = cli.command else {
            panic!("conversion command parsed as a different subcommand");
        };
        assert_eq!(
            arguments.inputs,
            [PathBuf::from("a.so"), PathBuf::from("bin")]
        );
        assert_eq!(arguments.output_dir, Some(PathBuf::from("out")));
        assert_eq!(arguments.jobs, NonZeroUsize::new(8));
        assert!(arguments.recursive);
        assert!(arguments.output.is_none());
    }
}
