//! Terminal input and restoration for interactive approval.

use std::error::Error;
use std::fmt;
use std::io::{IsTerminal, Read, Write};
use std::os::fd::AsFd;
use std::path::Path;

use anyhow::{Result, anyhow};
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcsetattr};

use super::output::{AnsiStyle, HumanOutput, relative_path};
use crate::models::TargetKind;
use crate::update_run::{UpdateReview, UpdateSelectionIdentity};

pub(super) struct InteractiveApproval<'a> {
    input: Box<dyn ApprovalInput + 'a>,
}

impl<'a> InteractiveApproval<'a> {
    pub(super) fn classified<R>(input: R) -> Self
    where
        R: Read + AsFd + IsTerminal + 'a,
    {
        let terminal = input.is_terminal();
        Self {
            input: Box::new(ClassifiedInput { input, terminal }),
        }
    }

    pub(super) fn plain<R: Read + 'a>(input: R) -> Self {
        Self {
            input: Box::new(PlainInput(input)),
        }
    }

    pub(super) fn review<E: Write>(
        mut self,
        updates: &[UpdateReview<'_>],
        output: &mut E,
        repo_root: &Path,
        human_output: HumanOutput,
    ) -> Result<Vec<UpdateSelectionIdentity>> {
        let original = self.input.enter_raw_mode()?;
        let review_result = self.review_updates(updates, output, repo_root, human_output);
        let restoration_result = original
            .as_ref()
            .map_or(Ok(()), |settings| self.input.restore(settings));
        match (review_result, restoration_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(primary), Err(restoration)) => Err(ApprovalAndRestorationError {
                primary,
                restoration,
            }
            .into()),
        }
    }

    fn review_updates<E: Write>(
        &mut self,
        updates: &[UpdateReview<'_>],
        output: &mut E,
        repo_root: &Path,
        human_output: HumanOutput,
    ) -> Result<Vec<UpdateSelectionIdentity>> {
        let mut approved = Vec::new();
        for update in updates {
            write!(output, "{}", render_prompt(update, repo_root, human_output))?;
            output.flush()?;
            let mut buffer = [0; 1];
            let count = self.input.read(&mut buffer)?;
            if count != 0 && buffer[0] == 0x03 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "prompt interrupted",
                )
                .into());
            }
            let choice = count.checked_sub(1).map_or('\n', |_| char::from(buffer[0]));
            let displayed = if matches!(choice, 'y' | 'Y') {
                'y'
            } else {
                'n'
            };
            if self.input.is_terminal() {
                write!(output, "{displayed}\r\n")?;
            } else {
                writeln!(output, "{displayed}")?;
            }
            if matches!(choice, 'y' | 'Y') {
                approved.push(update.identity().clone());
            }
        }
        Ok(approved)
    }
}

trait ApprovalInput: Read {
    fn is_terminal(&self) -> bool;
    fn enter_raw_mode(&self) -> Result<Option<Termios>>;
    fn restore(&self, settings: &Termios) -> Result<()>;
}

struct PlainInput<R>(R);

impl<R: Read> Read for PlainInput<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer)
    }
}

impl<R: Read> ApprovalInput for PlainInput<R> {
    fn is_terminal(&self) -> bool {
        false
    }

    fn enter_raw_mode(&self) -> Result<Option<Termios>> {
        Ok(None)
    }

    fn restore(&self, _settings: &Termios) -> Result<()> {
        Ok(())
    }
}

struct ClassifiedInput<R> {
    input: R,
    terminal: bool,
}

impl<R: Read> Read for ClassifiedInput<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.input.read(buffer)
    }
}

impl<R: Read + AsFd> ApprovalInput for ClassifiedInput<R> {
    fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn enter_raw_mode(&self) -> Result<Option<Termios>> {
        if !self.terminal {
            return Ok(None);
        }
        let original = tcgetattr(&self.input)?;
        let mut raw = original.clone();
        raw.make_raw();
        tcsetattr(&self.input, OptionalActions::Now, &raw)?;
        Ok(Some(original))
    }

    fn restore(&self, settings: &Termios) -> Result<()> {
        tcsetattr(&self.input, OptionalActions::Now, settings).map_err(|error| anyhow!(error))
    }
}

#[derive(Debug)]
struct ApprovalAndRestorationError {
    primary: anyhow::Error,
    restoration: anyhow::Error,
}

impl fmt::Display for ApprovalAndRestorationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:#}; additionally, terminal restoration failed: {:#}",
            self.primary, self.restoration
        )
    }
}

impl Error for ApprovalAndRestorationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.primary.as_ref())
    }
}

fn render_prompt(update: &UpdateReview<'_>, repo_root: &Path, human_output: HumanOutput) -> String {
    let path = human_output.styled(&relative_path(update.path(), repo_root), AnsiStyle::Cyan);
    let document = update.document_index() + 1;
    let details = match update.kind() {
        TargetKind::HelmRelease => {
            let current = human_output.styled(update.current_version(), AnsiStyle::Yellow);
            let latest = human_output.styled(update.latest_version(), AnsiStyle::Green);
            format!(
                "HelmRelease {}, {}, chart {}/{} {current} -> {latest}",
                update.target_name(),
                update.yaml_path(),
                update.repo_name().unwrap_or_default(),
                update.chart_name().unwrap_or_default()
            )
        }
        TargetKind::ImageBinding => {
            let current = human_output.styled(
                update.current_image().unwrap_or_default(),
                AnsiStyle::Yellow,
            );
            let latest =
                human_output.styled(update.latest_image().unwrap_or_default(), AnsiStyle::Green);
            format!(
                "{} {}, {}, image {current} -> {latest}",
                update.resource_kind(),
                update.target_name(),
                update.yaml_path()
            )
        }
    };
    format!("Update {path} (document {document}, {details})? [y/N] ")
}
