use std::fmt;
use std::io::{self, IsTerminal as _, Write};

use anstream::{AutoStream, ColorChoice};
use anstyle::{AnsiColor, Effects, Style};

pub(crate) const ACCENT: Style = AnsiColor::Cyan.on_default().effects(Effects::BOLD);
pub(crate) const MUTED: Style = AnsiColor::BrightBlack.on_default();
pub(crate) const NAME: Style = AnsiColor::Green.on_default();
const SUCCESS: Style = AnsiColor::Green.on_default().effects(Effects::BOLD);
const WARNING: Style = AnsiColor::Yellow.on_default().effects(Effects::BOLD);
const FAILURE: Style = AnsiColor::Red.on_default().effects(Effects::BOLD);

#[derive(Clone, Copy, Eq, PartialEq)]
enum OutputMode {
    Normal,
    Quiet,
    Verbose,
}

pub(crate) struct Terminal {
    stdout: AutoStream<io::StdoutLock<'static>>,
    stderr: AutoStream<io::StderrLock<'static>>,
    mode: OutputMode,
    interactive: bool,
    progress_active: bool,
}

impl Terminal {
    pub(crate) fn new(choice: ColorChoice, quiet: bool, verbose: bool) -> Self {
        let interactive = io::stderr().is_terminal();
        let mode = if quiet {
            OutputMode::Quiet
        } else if verbose {
            OutputMode::Verbose
        } else {
            OutputMode::Normal
        };
        Self {
            stdout: AutoStream::new(io::stdout(), choice).lock(),
            stderr: AutoStream::new(io::stderr(), choice).lock(),
            mode,
            interactive,
            progress_active: false,
        }
    }

    pub(crate) const fn stdout(&mut self) -> &mut AutoStream<io::StdoutLock<'static>> {
        &mut self.stdout
    }

    pub(crate) const fn is_verbose(&self) -> bool {
        matches!(self.mode, OutputMode::Verbose)
    }

    pub(crate) const fn is_quiet(&self) -> bool {
        matches!(self.mode, OutputMode::Quiet)
    }

    pub(crate) fn summary(&mut self) -> Option<&mut AutoStream<io::StderrLock<'static>>> {
        self.clear_progress();
        (self.mode != OutputMode::Quiet).then_some(&mut self.stderr)
    }

    pub(crate) fn status(&mut self, message: fmt::Arguments<'_>) -> io::Result<()> {
        if self.mode != OutputMode::Quiet {
            self.clear_progress();
            writeln!(
                self.stderr,
                "{ACCENT}info:{} {message}",
                ACCENT.render_reset()
            )?;
        }
        Ok(())
    }

    /// Shows replaceable batch progress on a terminal without polluting logs.
    pub(crate) fn progress(&mut self, message: fmt::Arguments<'_>) -> io::Result<()> {
        if self.mode != OutputMode::Quiet && self.interactive {
            write!(
                self.stderr,
                "\r\x1b[2K{MUTED}{message}{}",
                MUTED.render_reset()
            )?;
            self.stderr.flush()?;
            self.progress_active = true;
        }
        Ok(())
    }

    /// Keeps routine activity on the live line unless verbose output was asked for.
    pub(crate) fn activity(&mut self, message: fmt::Arguments<'_>) -> io::Result<()> {
        if self.mode == OutputMode::Verbose {
            self.status(message)
        } else {
            self.progress(message)
        }
    }

    fn clear_progress(&mut self) {
        if self.progress_active {
            drop(write!(self.stderr, "\r\x1b[2K"));
            self.progress_active = false;
        }
    }

    pub(crate) fn success(&mut self, message: fmt::Arguments<'_>) -> io::Result<()> {
        if self.mode != OutputMode::Quiet {
            self.clear_progress();
            writeln!(
                self.stderr,
                "{SUCCESS}ok:{} {message}",
                SUCCESS.render_reset()
            )?;
        }
        Ok(())
    }

    pub(crate) fn warning(&mut self, message: fmt::Arguments<'_>) -> io::Result<()> {
        self.clear_progress();
        writeln!(
            self.stderr,
            "{WARNING}warning:{} {message}",
            WARNING.render_reset()
        )
    }

    pub(crate) fn error(&mut self, error: &anyhow::Error) -> io::Result<()> {
        self.clear_progress();
        writeln!(
            self.stderr,
            "{FAILURE}error:{} {error}",
            FAILURE.render_reset()
        )?;
        for cause in error.chain().skip(1) {
            writeln!(
                self.stderr,
                "  {MUTED}caused by:{} {cause}",
                MUTED.render_reset()
            )?;
        }
        Ok(())
    }
}
