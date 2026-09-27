//! How terminal output is dressed.
//!
//! One decision for a whole run, taken from the stream that run writes to,
//! and the small vocabulary the installer and the activation receipt both
//! draw on. Only [`Style`] decides whether escapes are emitted: the words and
//! the glyphs are the same either way, so an install piped to a log reads as
//! the terminal shows it.
//!
//! This is the parent's own output. The activation child's stderr is a
//! separate surface that `installation::native` parses for the one line that
//! says why a run failed, and nothing here writes to it.

use std::env;
use std::io::IsTerminal;

/// The column prose wraps at. Fixed rather than read from the terminal, so
/// one run renders the same way wherever it is read back.
const WIDTH: usize = 72;

/// Whether output carries terminal escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    Plain,
    Colored,
}

/// How a step of a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    /// Named without an outcome: the step a run is on.
    Doing,
    Done,
    Failed,
    Warned,
}

impl Mark {
    fn glyph(self) -> char {
        match self {
            Mark::Doing => '·',
            Mark::Done => '✓',
            Mark::Failed => '✗',
            Mark::Warned => '!',
        }
    }

    fn escape(self) -> &'static str {
        match self {
            Mark::Doing => "2",
            Mark::Done => "1;32",
            Mark::Failed => "1;31",
            Mark::Warned => "1;33",
        }
    }
}

impl Style {
    pub(crate) fn of_stdout() -> Self {
        Self::of(std::io::stdout().is_terminal())
    }

    pub(crate) fn of_stderr() -> Self {
        Self::of(std::io::stderr().is_terminal())
    }

    /// Escapes only for a terminal that has not asked to go without them.
    fn of(is_terminal: bool) -> Self {
        let refused =
            env::var_os("NO_COLOR").is_some() || env::var_os("TERM").is_some_and(|term| term == "dumb");
        match is_terminal && !refused {
            true => Style::Colored,
            false => Style::Plain,
        }
    }

    fn paint(self, escape: &str, text: &str) -> String {
        match self {
            Style::Colored => format!("\u{1b}[{escape}m{text}\u{1b}[0m"),
            Style::Plain => text.to_owned(),
        }
    }

    /// The line a run opens with: what is being installed, and into what.
    pub(crate) fn heading(self, text: &str) -> String {
        self.paint("1", text)
    }

    /// One step of a run: its mark in the left margin, then what it did.
    pub(crate) fn step(self, mark: Mark, text: &str) -> String {
        let glyph = mark.glyph().to_string();
        format!("  {} {text}", self.paint(mark.escape(), &glyph))
    }

    /// Prose under the step it belongs to, wrapped and indented past its mark.
    pub(crate) fn detail(self, text: &str) -> String {
        wrap(text, WIDTH - 4)
            .iter()
            .map(|line| format!("    {}", self.paint("2", line)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Commands to type, set apart so they can be copied without the prose
    /// around them. Never wrapped: a broken command line is not copyable.
    pub(crate) fn commands(self, lines: &[String]) -> String {
        lines
            .iter()
            .map(|line| format!("      {}", self.paint("1", line)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// One `name value` row of a receipt, with the names in one column.
    pub(crate) fn field(self, name: &str, width: usize, value: &str) -> String {
        format!("  {} {value}", self.paint("1;36", &format!("{name:<width$}")))
    }
}

/// Greedy word wrap. A run of non-space characters longer than the column is
/// left whole, so a long path or flag in the middle of prose is never broken
/// across lines. Whitespace *inside* a path — `Application Support` on macOS —
/// is a break like any other, which is why [`Style::commands`] does not wrap.
fn wrap(text: &str, columns: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > columns {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the whole module rests on: style changes the escapes and
    /// nothing else, so a piped log and a terminal carry the same words.
    #[test]
    fn style_changes_only_the_escapes() {
        let rendered = |style: Style| {
            [
                style.heading("OpenAPPA 0.25.0 → Claude Code"),
                style.step(Mark::Done, "fetched and verified artifacts"),
                style.step(Mark::Failed, "activation"),
                style.step(Mark::Doing, "resolving the published version"),
                style.step(Mark::Warned, "the config includes no battery"),
                style.detail("The launcher at ~/.local/bin/clappa is from an older OpenAPPA."),
                style.commands(&["appa plugin remove claude-code --purge".to_owned()]),
                style.field("Runtime", 9, "verified"),
            ]
            .join("\n")
        };
        let plain = rendered(Style::Plain);
        let colored = rendered(Style::Colored);

        assert!(!plain.contains('\u{1b}'));
        assert!(colored.contains('\u{1b}'));
        let stripped = strip_escapes(&colored);
        assert_eq!(stripped, plain, "colored output says something plain output does not");
    }

    /// Every mark is told apart by its glyph alone, so a plain log reports the
    /// outcome of a step as clearly as a terminal colours it.
    #[test]
    fn each_mark_has_its_own_glyph() {
        let mut seen = std::collections::HashSet::new();
        for mark in [Mark::Doing, Mark::Done, Mark::Failed, Mark::Warned] {
            assert!(seen.insert(mark.glyph()), "{mark:?} reuses another mark's glyph");
        }
    }

    /// The column is a soft bound: it is never broken except by a single run of
    /// non-space characters, which is kept whole so it stays copyable.
    #[test]
    fn prose_wraps_and_long_words_stay_whole() {
        let path = "/Users/someone/.local/share/appa/deployments/e116beff/plugin/bin";
        let wrapped = wrap(&format!("The launcher {path} is from an older OpenAPPA."), 40);

        assert!(wrapped.len() > 1, "nothing wrapped at all");
        assert!(
            wrapped.iter().any(|line| line.contains(path)),
            "the path was broken across lines",
        );
        for line in &wrapped {
            assert!(
                line.chars().count() <= 40 || line.split_whitespace().count() == 1,
                "{line:?} is over the column without being one word",
            );
        }
    }

    /// Commands are never wrapped, whatever their length: a command broken over
    /// two lines cannot be copied into a shell.
    #[test]
    fn commands_are_never_wrapped() {
        let long = "appa plugin install claude-code --config /Users/someone/Library/Application Support/appa/appa.toml".to_owned();

        let rendered = Style::Plain.commands(std::slice::from_ref(&long));

        assert_eq!(rendered.lines().count(), 1);
        assert!(rendered.contains(&long));
    }

    /// A receipt's names line up, so the values read as a column.
    #[test]
    fn fields_align_their_names() {
        let style = Style::Plain;
        let runtime = style.field("Runtime", 9, "verified");
        let config = style.field("Config", 9, "~/.appa");

        assert_eq!(
            runtime.find("verified"),
            config.find("~/.appa"),
            "the values start in different columns",
        );
    }

    fn strip_escapes(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}

