//! Output formatting shared by every CLI command.
//!
//! The CLI reference (MVP spec §7, `feeds-cli` spec "Output format" and
//! "Machine-readable output") defines three renderings of the same data:
//!
//! | Format                     | When                               | Lists                        | Single results            |
//! |----------------------------|------------------------------------|------------------------------|---------------------------|
//! | [`OutputFormat::Human`]    | stdout is a terminal, no `--json`  | one record per line, columns aligned with spaces | a status message line |
//! | [`OutputFormat::Tabular`]  | stdout is piped, no `--json`       | one record per line, fields tab-separated        | a status message line |
//! | [`OutputFormat::Json`]     | `--json`                           | NDJSON: one JSON object per line                 | one JSON object line  |
//!
//! Commands never format by hand: they hand records to [`Output::list`] (a
//! list of serializable records plus a function turning one record into its
//! text columns) or [`Output::outcome`] (one serializable result plus its
//! human message), so every command family gets the same contract for free.
//!
//! # Safety of displayed text
//!
//! Text rendered in the human/tabular formats goes through [`sanitize`],
//! which replaces control characters (including tabs, newlines and the ESC
//! that starts terminal escape sequences) with spaces. This keeps one record
//! on one line, keeps tab-separated output parseable, and implements the
//! spec's "remote text is stripped of terminal control sequences" rule for
//! feed titles shown by later commands. JSON output needs no such step:
//! `serde_json` escapes control characters.
//!
//! # Errors
//!
//! Write failures (closed pipe, full disk) surface as
//! [`MetroError::Generic`], exit code 1.

use std::borrow::Cow;
use std::io::{ErrorKind, Write};

use metro_core::MetroError;
use serde::Serialize;

/// Separator between columns in [`OutputFormat::Human`] output.
const HUMAN_COLUMN_GAP: &str = "  ";

/// How command results are rendered on stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Plain text for a terminal: aligned columns, status messages.
    Human,
    /// Plain text for pipes: tab-separated columns, status messages.
    Tabular,
    /// Machine output: one JSON object per line (NDJSON for lists).
    Json,
}

impl OutputFormat {
    /// Select the format from the global `--json` flag and whether stdout is
    /// a terminal. `--json` always wins.
    #[must_use]
    pub fn select(json: bool, stdout_is_terminal: bool) -> Self {
        match (json, stdout_is_terminal) {
            (true, _) => Self::Json,
            (false, true) => Self::Human,
            (false, false) => Self::Tabular,
        }
    }
}

/// Replace every control character in `text` with a space.
///
/// Borrows when there is nothing to replace, which is the common case.
#[must_use]
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if text.chars().any(char::is_control) {
        Cow::Owned(
            text.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

/// Map a write failure to the core error type (exit code 1).
pub fn write_error(err: &std::io::Error) -> MetroError {
    MetroError::Generic(format!("failed to write output: {err}"))
}

/// Writer adapter that silently discards output once the reader has gone
/// away (`metro folders list | head -1`).
///
/// A closed pipe is the reader's choice, not a failure: like standard Unix
/// tools, the command still exits with its own status and prints no error.
/// Every other write error is passed through.
pub struct IgnoreBrokenPipe<W> {
    inner: W,
}

impl<W: Write> IgnoreBrokenPipe<W> {
    /// Wrap `inner`.
    pub fn new(inner: W) -> Self {
        Self { inner }
    }
}

impl<W: Write> Write for IgnoreBrokenPipe<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self.inner.write(buf) {
            Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(buf.len()),
            other => other,
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.inner.flush() {
            Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
            other => other,
        }
    }
}

/// A stdout writer bound to one [`OutputFormat`].
///
/// Holds `&mut dyn Write` so tests can render into a `Vec<u8>` and the real
/// binary into a locked stdout, without making every command generic.
pub struct Output<'w> {
    writer: &'w mut dyn Write,
    format: OutputFormat,
}

impl<'w> Output<'w> {
    /// Create an output writing to `writer` in `format`.
    pub fn new(writer: &'w mut dyn Write, format: OutputFormat) -> Self {
        Self { writer, format }
    }

    /// Render a list of records.
    ///
    /// - JSON: each record serialized on its own line (NDJSON).
    /// - Human/tabular: `columns(record)` gives the text fields of one
    ///   record; each record becomes one line. An empty list prints nothing
    ///   in every format, so scripts can count lines.
    ///
    /// # Errors
    ///
    /// [`MetroError::Generic`] if serialization or writing fails.
    pub fn list<T, F>(&mut self, records: &[T], columns: F) -> Result<(), MetroError>
    where
        T: Serialize,
        F: Fn(&T) -> Vec<String>,
    {
        match self.format {
            OutputFormat::Json => records.iter().try_for_each(|record| self.json_line(record)),
            OutputFormat::Tabular => records
                .iter()
                .try_for_each(|record| self.tabular_line(&columns(record))),
            OutputFormat::Human => {
                let rows: Vec<Vec<String>> = records.iter().map(columns).collect();
                self.aligned_rows(&rows)
            }
        }
    }

    /// Render the result of a single-record command (add, rename, remove …).
    ///
    /// - JSON: `record` serialized as one line.
    /// - Human/tabular: `message` on one line.
    ///
    /// # Errors
    ///
    /// [`MetroError::Generic`] if serialization or writing fails.
    pub fn outcome<T: Serialize>(&mut self, record: &T, message: &str) -> Result<(), MetroError> {
        match self.format {
            OutputFormat::Json => self.json_line(record),
            OutputFormat::Human | OutputFormat::Tabular => {
                writeln!(self.writer, "{}", sanitize(message)).map_err(|e| write_error(&e))
            }
        }
    }

    /// Write one compact JSON object followed by a newline.
    fn json_line<T: Serialize>(&mut self, record: &T) -> Result<(), MetroError> {
        serde_json::to_writer(&mut *self.writer, record)
            .map_err(|e| MetroError::Generic(format!("failed to serialize output: {e}")))?;
        writeln!(self.writer).map_err(|e| write_error(&e))
    }

    /// Write one record as tab-separated sanitized fields.
    fn tabular_line(&mut self, fields: &[String]) -> Result<(), MetroError> {
        let line: Vec<Cow<'_, str>> = fields.iter().map(|f| sanitize(f)).collect();
        writeln!(self.writer, "{}", line.join("\t")).map_err(|e| write_error(&e))
    }

    /// Write records with every column but the last padded to its widest
    /// value, so columns line up in a terminal.
    fn aligned_rows(&mut self, rows: &[Vec<String>]) -> Result<(), MetroError> {
        let sanitized: Vec<Vec<Cow<'_, str>>> = rows
            .iter()
            .map(|row| row.iter().map(|f| sanitize(f)).collect())
            .collect();

        let column_count = sanitized.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..column_count)
            .map(|col| {
                sanitized
                    .iter()
                    .filter_map(|row| row.get(col))
                    .map(|field| field.chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();

        for row in &sanitized {
            let mut line = String::new();
            for (col, field) in row.iter().enumerate() {
                if col + 1 == row.len() {
                    line.push_str(field);
                } else {
                    let width = widths.get(col).copied().unwrap_or(0);
                    let padding = width.saturating_sub(field.chars().count());
                    line.push_str(field);
                    line.extend(std::iter::repeat_n(' ', padding));
                    line.push_str(HUMAN_COLUMN_GAP);
                }
            }
            writeln!(self.writer, "{line}").map_err(|e| write_error(&e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Row {
        id: u32,
        name: &'static str,
    }

    const ROWS: [Row; 2] = [
        Row { id: 1, name: "a" },
        Row {
            id: 100,
            name: "b\tc",
        },
    ];

    fn columns(row: &Row) -> Vec<String> {
        vec![row.id.to_string(), row.name.to_string()]
    }

    fn render(format: OutputFormat, f: impl FnOnce(&mut Output<'_>)) -> String {
        let mut buf = Vec::new();
        f(&mut Output::new(&mut buf, format));
        String::from_utf8(buf).unwrap()
    }

    /// A writer whose reader has gone away.
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn test_broken_pipe_is_ignored() {
        let mut writer = IgnoreBrokenPipe::new(ClosedPipe);
        let mut output = Output::new(&mut writer, OutputFormat::Tabular);
        assert!(output.list(&ROWS, columns).is_ok());
        assert!(writer.flush().is_ok());
    }

    #[test]
    fn test_select_format() {
        assert_eq!(OutputFormat::select(true, true), OutputFormat::Json);
        assert_eq!(OutputFormat::select(true, false), OutputFormat::Json);
        assert_eq!(OutputFormat::select(false, true), OutputFormat::Human);
        assert_eq!(OutputFormat::select(false, false), OutputFormat::Tabular);
    }

    #[test]
    fn test_sanitize_replaces_control_chars() {
        assert_eq!(sanitize("plain"), "plain");
        assert!(matches!(sanitize("plain"), Cow::Borrowed(_)));
        assert_eq!(sanitize("a\tb\nc\u{1b}[31m"), "a b c [31m");
    }

    #[test]
    fn test_list_json_is_ndjson() {
        let text = render(OutputFormat::Json, |o| o.list(&ROWS, columns).unwrap());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(value.is_object());
        }
        assert!(text.contains(r#""name":"b\tc""#));
    }

    #[test]
    fn test_list_tabular_is_tab_separated_and_sanitized() {
        let text = render(OutputFormat::Tabular, |o| o.list(&ROWS, columns).unwrap());
        assert_eq!(text, "1\ta\n100\tb c\n");
    }

    #[test]
    fn test_list_human_aligns_columns() {
        let text = render(OutputFormat::Human, |o| o.list(&ROWS, columns).unwrap());
        assert_eq!(text, "1    a\n100  b c\n");
    }

    #[test]
    fn test_list_empty_prints_nothing() {
        for format in [
            OutputFormat::Human,
            OutputFormat::Tabular,
            OutputFormat::Json,
        ] {
            let text = render(format, |o| o.list::<Row, _>(&[], columns).unwrap());
            assert!(text.is_empty(), "{format:?} printed {text:?}");
        }
    }

    #[test]
    fn test_outcome_message_vs_json() {
        let row = Row { id: 7, name: "x" };
        let human = render(OutputFormat::Human, |o| o.outcome(&row, "done\n").unwrap());
        assert_eq!(human, "done \n");
        let json = render(OutputFormat::Json, |o| o.outcome(&row, "done").unwrap());
        assert_eq!(json, "{\"id\":7,\"name\":\"x\"}\n");
    }
}
