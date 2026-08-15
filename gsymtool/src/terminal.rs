use std::fmt;
use std::io::{self, Write};

use anstream::{AutoStream, ColorChoice};
use anstyle::{AnsiColor, Effects, Style};

pub(crate) const ACCENT: Style = AnsiColor::Cyan.on_default().effects(Effects::BOLD);
pub(crate) const MUTED: Style = AnsiColor::BrightBlack.on_default();
pub(crate) const NAME: Style = AnsiColor::Green.on_default();
const SUCCESS: Style = AnsiColor::Green.on_default().effects(Effects::BOLD);
const WARNING: Style = AnsiColor::Yellow.on_default().effects(Effects::BOLD);
const FAILURE: Style = AnsiColor::Red.on_default().effects(Effects::BOLD);

pub(crate) struct Terminal {
    stdout: AutoStream<io::StdoutLock<'static>>,
    stderr: AutoStream<io::StderrLock<'static>>,
    quiet: bool,
}

impl Terminal {
    pub(crate) fn new(choice: ColorChoice, quiet: bool) -> Self {
        Self {
            stdout: AutoStream::new(io::stdout(), choice).lock(),
            stderr: AutoStream::new(io::stderr(), choice).lock(),
            quiet,
        }
    }

    pub(crate) const fn stdout(&mut self) -> &mut AutoStream<io::StdoutLock<'static>> {
        &mut self.stdout
    }

    pub(crate) fn summary(&mut self) -> Option<&mut AutoStream<io::StderrLock<'static>>> {
        (!self.quiet).then_some(&mut self.stderr)
    }

    pub(crate) fn success(&mut self, message: fmt::Arguments<'_>) -> io::Result<()> {
        if !self.quiet {
            writeln!(
                self.stderr,
                "{SUCCESS}ok:{} {message}",
                SUCCESS.render_reset()
            )?;
        }
        Ok(())
    }

    pub(crate) fn warning(&mut self, message: fmt::Arguments<'_>) -> io::Result<()> {
        writeln!(
            self.stderr,
            "{WARNING}warning:{} {message}",
            WARNING.render_reset()
        )
    }

    pub(crate) fn error(&mut self, error: &anyhow::Error) -> io::Result<()> {
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
