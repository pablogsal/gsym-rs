//! Batch conversion across a pool of worker threads.
//!
//! Each file uses the single-file conversion path, so worker count does not
//! affect output bytes.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::DirEntry;
use std::io::{Read as _, Write as _};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use gsym::convert::ElfConverter;

use crate::cli::HumanBytes;
use crate::terminal::{MUTED, Terminal};
use crate::{Count, Elapsed, persist_builder};

const ELF_MAGIC: [u8; 4] = *b"\x7fELF";

/// What one `convert --output-dir` invocation asked for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Batch<'a> {
    /// ELF files and directories named on the command line.
    pub(crate) inputs: &'a [PathBuf],
    /// Root the scanned directories are mirrored under.
    pub(crate) output_dir: &'a Path,
    /// Whether directory inputs are descended into.
    pub(crate) recursive: bool,
    /// Requested worker count, or `None` for the available parallelism.
    pub(crate) jobs: Option<NonZeroUsize>,
}

#[derive(Debug)]
struct Job {
    input: PathBuf,
    output: PathBuf,
    size: u64,
}

#[derive(Debug, Default)]
struct Plan {
    jobs: Vec<Job>,
    skipped: usize,
}

struct Converted {
    bytes: u64,
    functions: usize,
    warnings: usize,
    elapsed: Duration,
}

struct Outcome<'jobs> {
    job: &'jobs Job,
    result: Result<Option<Converted>>,
}

#[derive(Debug, Default)]
struct Totals {
    converted: usize,
    empty: usize,
    failed: usize,
    warnings: usize,
    bytes: u64,
}

/// Converts every ELF the batch names, several files at a time.
///
/// # Errors
///
/// Returns an error when the batch is empty, when two inputs claim the same
/// output path, or when any conversion failed. Individual failures do not stop
/// the batch: they are reported as they happen and counted in the summary.
pub(crate) fn run(
    converter: &ElfConverter,
    batch: Batch<'_>,
    terminal: &mut Terminal,
) -> Result<()> {
    let started = Instant::now();
    let plan = plan(batch)?;
    if plan.jobs.is_empty() {
        bail!("found no ELF files to convert");
    }
    create_output_directories(&plan.jobs)?;
    let workers = batch
        .jobs
        .unwrap_or_else(available_jobs)
        .get()
        .min(plan.jobs.len());
    let totals = execute(converter, &plan.jobs, workers, terminal)?;
    report_totals(&plan, &totals, started.elapsed(), terminal)?;
    if totals.failed != 0 {
        bail!(
            "{} of {} conversions failed",
            Count(totals.failed),
            Count(plan.jobs.len())
        );
    }
    Ok(())
}

fn available_jobs() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

fn plan(batch: Batch<'_>) -> Result<Plan> {
    let mut plan = Plan::default();
    for input in batch.inputs {
        let metadata = std::fs::metadata(input)
            .with_context(|| format!("failed to read {}", input.display()))?;
        if metadata.is_dir() {
            scan(input, batch.output_dir, batch.recursive, &mut plan)?;
        } else {
            let name = input
                .file_name()
                .with_context(|| format!("{} has no file name", input.display()))?;
            plan.jobs.push(Job {
                input: input.clone(),
                output: batch.output_dir.join(gsym_name(name)),
                size: metadata.len(),
            });
        }
    }
    reject_collisions(&plan.jobs)?;
    plan.jobs.sort_by_key(|job| Reverse(job.size));
    Ok(plan)
}

/// Directory entries are sorted by name. Symlinks are excluded to avoid cycles
/// and duplicate conversions of versioned libraries.
fn scan(source: &Path, destination: &Path, recursive: bool, plan: &mut Plan) -> Result<()> {
    let mut entries = std::fs::read_dir(source)
        .with_context(|| format!("failed to read {}", source.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to read {}", source.display()))?;
    entries.sort_by_key(DirEntry::file_name);
    for entry in entries {
        let kind = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        if kind.is_dir() {
            if recursive {
                scan(
                    &entry.path(),
                    &destination.join(entry.file_name()),
                    recursive,
                    plan,
                )?;
            }
        } else if let Some(size) = elf_size(&entry, kind.is_symlink())? {
            plan.jobs.push(Job {
                output: destination.join(gsym_name(&entry.file_name())),
                input: entry.path(),
                size,
            });
        } else {
            plan.skipped = plan.skipped.saturating_add(1);
        }
    }
    Ok(())
}

/// Unreadable files in scanned trees are skipped rather than failing the batch.
fn elf_size(entry: &DirEntry, symlink: bool) -> Result<Option<u64>> {
    let metadata = entry
        .metadata()
        .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
    if symlink || !metadata.is_file() {
        return Ok(None);
    }
    let mut magic = [0_u8; 4];
    let elf = std::fs::File::open(entry.path())
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok_and(|()| magic == ELF_MAGIC);
    Ok(elf.then_some(metadata.len()))
}

/// Keeps the full ELF name so versioned libraries map to distinct outputs.
fn gsym_name(name: &OsStr) -> OsString {
    let mut output = name.to_os_string();
    output.push(".gsym");
    output
}

fn reject_collisions(jobs: &[Job]) -> Result<()> {
    let mut claimed = HashMap::with_capacity(jobs.len());
    for job in jobs {
        if let Some(previous) = claimed.insert(job.output.as_path(), job.input.as_path()) {
            bail!(
                "{} and {} would both be written to {}",
                previous.display(),
                job.input.display(),
                job.output.display()
            );
        }
    }
    Ok(())
}

fn create_output_directories(jobs: &[Job]) -> Result<()> {
    let mut created = HashSet::new();
    for job in jobs {
        let Some(parent) = job
            .output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        else {
            continue;
        };
        if created.insert(parent) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    Ok(())
}

fn execute(
    converter: &ElfConverter,
    jobs: &[Job],
    workers: usize,
    terminal: &mut Terminal,
) -> Result<Totals> {
    let cursor = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| -> Result<Totals> {
        for _ in 0..workers {
            let sender = sender.clone();
            let cursor = &cursor;
            scope.spawn(move || work(converter, jobs, cursor, &sender));
        }
        drop(sender);
        let mut totals = Totals::default();
        for outcome in receiver {
            report(&outcome, &mut totals, terminal)?;
        }
        Ok(totals)
    })
}

fn work<'jobs>(
    converter: &ElfConverter,
    jobs: &'jobs [Job],
    cursor: &AtomicUsize,
    sender: &Sender<Outcome<'jobs>>,
) {
    loop {
        let index = cursor.fetch_add(1, Ordering::Relaxed);
        let Some(job) = jobs.get(index) else { break };
        let outcome = Outcome {
            job,
            result: convert_one(converter, job)
                .with_context(|| format!("failed to convert {}", job.input.display())),
        };
        if sender.send(outcome).is_err() {
            break;
        }
    }
}

/// Inputs rejected as empty or producing no functions are counted as skipped.
fn convert_one(converter: &ElfConverter, job: &Job) -> Result<Option<Converted>> {
    let started = Instant::now();
    let report = match converter.convert_path(&job.input) {
        Ok(report) => report,
        Err(error) if holds_nothing_to_convert(&error) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let functions = report.builder.functions().len();
    let warnings = report.warnings.len();
    if functions == 0 {
        return Ok(None);
    }
    let bytes = persist_builder(report.builder, &job.output)?;
    Ok(Some(Converted {
        bytes,
        functions,
        warnings,
        elapsed: started.elapsed(),
    }))
}

const fn holds_nothing_to_convert(error: &gsym::Error) -> bool {
    matches!(error, gsym::Error::InvalidModel(_))
}

/// Per-file warnings are counted rather than printed to keep failures visible.
fn report(outcome: &Outcome<'_>, totals: &mut Totals, terminal: &mut Terminal) -> Result<()> {
    match &outcome.result {
        Ok(None) => totals.empty = totals.empty.saturating_add(1),
        Ok(Some(converted)) => {
            totals.converted = totals.converted.saturating_add(1);
            totals.warnings = totals.warnings.saturating_add(converted.warnings);
            totals.bytes = totals.bytes.saturating_add(converted.bytes);
            terminal.success(format_args!(
                "{} {MUTED}->{} {} ({}, {}{}, {})",
                outcome.job.input.display(),
                MUTED.render_reset(),
                outcome.job.output.display(),
                HumanBytes(converted.bytes),
                Plural(converted.functions, "function"),
                Warnings(converted.warnings),
                Elapsed(converted.elapsed),
            ))?;
        }
        Err(error) => {
            totals.failed = totals.failed.saturating_add(1);
            terminal.error(error)?;
        }
    }
    Ok(())
}

fn report_totals(
    plan: &Plan,
    totals: &Totals,
    elapsed: Duration,
    terminal: &mut Terminal,
) -> Result<()> {
    let Some(stderr) = terminal.summary() else {
        return Ok(());
    };
    writeln!(
        stderr,
        "  converted    {} of {} files | {} written",
        Count(totals.converted),
        Count(plan.jobs.len()),
        HumanBytes(totals.bytes)
    )?;
    if totals.failed != 0 {
        writeln!(
            stderr,
            "  failed       {}",
            Plural(totals.failed, "conversion")
        )?;
    }
    match (plan.skipped, totals.empty) {
        (0, 0) => {}
        (0, empty) => writeln!(
            stderr,
            "  skipped      {} without functions",
            Plural(empty, "ELF file")
        )?,
        (skipped, 0) => writeln!(stderr, "  skipped      {}", Plural(skipped, "non-ELF path"))?,
        (skipped, empty) => writeln!(
            stderr,
            "  skipped      {} | {} without functions",
            Plural(skipped, "non-ELF path"),
            Plural(empty, "ELF file")
        )?,
    }
    if totals.warnings != 0 {
        writeln!(
            stderr,
            "  diagnostics  {}",
            Plural(totals.warnings, "warning")
        )?;
    }
    writeln!(stderr, "  elapsed      {}", Elapsed(elapsed))?;
    Ok(())
}

struct Plural(usize, &'static str);

impl fmt::Display for Plural {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", Count(self.0), self.1)?;
        if self.0 == 1 {
            return Ok(());
        }
        formatter.write_str("s")
    }
}

struct Warnings(usize);

impl fmt::Display for Warnings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == 0 {
            return Ok(());
        }
        write!(formatter, ", {}", Plural(self.0, "warning"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_rather_than_replaces_the_elf_extension() {
        assert_eq!(gsym_name(OsStr::new("libc.so.6")), "libc.so.6.gsym");
        assert_eq!(gsym_name(OsStr::new("app")), "app.gsym");
    }

    #[test]
    fn renders_a_warning_count_only_when_there_is_one() {
        assert_eq!(Warnings(0).to_string(), "");
        assert_eq!(Warnings(1).to_string(), ", 1 warning");
        assert_eq!(Warnings(2_500).to_string(), ", 2,500 warnings");
    }

    #[test]
    fn pluralizes_every_count_but_one() {
        assert_eq!(Plural(0, "function").to_string(), "0 functions");
        assert_eq!(Plural(1, "function").to_string(), "1 function");
        assert_eq!(Plural(12_000, "function").to_string(), "12,000 functions");
    }

    #[test]
    fn rejects_two_inputs_claiming_one_output() {
        let jobs = vec![
            Job {
                input: PathBuf::from("first/app"),
                output: PathBuf::from("out/app.gsym"),
                size: 1,
            },
            Job {
                input: PathBuf::from("second/app"),
                output: PathBuf::from("out/app.gsym"),
                size: 1,
            },
        ];
        let error = reject_collisions(&jobs).unwrap_err().to_string();
        assert!(error.contains("first/app"), "{error}");
        assert!(error.contains("second/app"), "{error}");
        assert!(error.contains("out/app.gsym"), "{error}");

        let mirrored = vec![
            Job {
                input: PathBuf::from("first/app"),
                output: PathBuf::from("out/first/app.gsym"),
                size: 1,
            },
            Job {
                input: PathBuf::from("second/app"),
                output: PathBuf::from("out/second/app.gsym"),
                size: 1,
            },
        ];
        assert!(reject_collisions(&mirrored).is_ok());
    }
}
