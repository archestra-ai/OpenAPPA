//! What an activation decided, and how that is shown once.

use std::env;
use std::io::IsTerminal;
use std::path::PathBuf;

use super::PLUGIN;
use super::endpoint::RuntimeOutcome;
use super::paths::friendly_path;

/// Whether the receipt carries terminal escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Style {
    Plain,
    Colored,
}

impl Style {
    pub(super) fn of_stdout() -> Self {
        if std::io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none() {
            Style::Colored
        } else {
            Style::Plain
        }
    }
}

/// What an activation decided, before any of it is words.
///
/// Everything the receipt can report is a field here, so what it keeps out of
/// its summary — install paths, deployment digests, the files it wrote — is
/// absent by construction rather than by a rendering step that must remember to
/// leave it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Receipt {
    /// Where the plugin came from, as the user would name it.
    pub(super) adapter: String,
    pub(super) config: PathBuf,
    pub(super) runtime_outcome: RuntimeOutcome,
}

impl Receipt {
    pub(super) fn render(&self, style: Style) -> String {
        let colored = style == Style::Colored;
        let title = if colored {
            "\u{1b}[1;32m✓ OpenAPPA activated\u{1b}[0m \u{1b}[2mfor Claude Code\u{1b}[0m"
        } else {
            "OpenAPPA activated for Claude Code"
        };
        let label = |name: &str| {
            if colored {
                format!("\u{1b}[1;36m{name:<9}\u{1b}[0m")
            } else {
                format!("{name:<9}")
            }
        };
        let mut receipt = format!(
            "{title}\n\n  {} {}\n  {} {PLUGIN}\n  {} {}\n  {} {}\n  {} clappa\n",
            label("Adapter"),
            self.adapter,
            label("Plugin"),
            label("Runtime"),
            self.runtime_outcome.as_str(),
            label("Config"),
            friendly_path(&self.config),
            label("Launcher"),
        );
        // A session loads its hooks at session start, and the hook wire carries no
        // version, so a session running across an upgrade keeps talking to the
        // runtime it started with.
        receipt.push_str("\nRestart any running `clappa` session to pick this up.\n");
        receipt.push_str("\nNext: run `clappa`, then `/appa-guide init`.\n");
        receipt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Style` is the only thing that decides whether escapes are emitted, and it
    /// decides it for the whole receipt rather than per line.
    #[test]
    fn only_a_colored_style_puts_escapes_in_a_receipt() {
        let receipt = Receipt {
            adapter: "current checkout".to_owned(),
            config: PathBuf::from("/etc/appa/appa.toml"),
            runtime_outcome: RuntimeOutcome::Healthy,
        };

        assert!(!receipt.render(Style::Plain).contains('\u{1b}'));
        assert!(receipt.render(Style::Colored).contains('\u{1b}'));
    }

    /// Every outcome a run can end in renders, and no two of them render the
    /// same receipt: a user cannot be shown "healthy" for a runtime that was reloaded.
    #[test]
    fn each_outcome_renders_a_distinct_receipt() {
        let mut seen = std::collections::HashSet::new();
        for runtime_outcome in [RuntimeOutcome::Healthy, RuntimeOutcome::Reloaded] {
            let receipt = Receipt {
                adapter: "current checkout".to_owned(),
                config: PathBuf::from("/etc/appa/appa.toml"),
                runtime_outcome,
            };
            assert!(
                seen.insert(receipt.render(Style::Plain)),
                "{runtime_outcome:?} renders as another outcome does",
            );
        }
    }
}
