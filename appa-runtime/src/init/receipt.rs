//! What an activation decided, and how that is shown once.

use std::path::PathBuf;

use super::endpoint::RuntimeOutcome;
use super::paths::friendly_path;
use crate::style::Mark;
pub(super) use crate::style::Style;

/// What an activation decided, before any of it is words.
///
/// Everything the receipt can report is a field here, so what it keeps out of
/// its summary — install paths, deployment digests, the files it wrote — is
/// absent by construction rather than by a rendering step that must remember to
/// leave it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Receipt {
    /// Where the deployment came from, as the user would name it.
    pub(super) adapter: String,
    /// The settings file the session's hook entries were written to.
    pub(super) hooks: PathBuf,
    pub(super) config: PathBuf,
    pub(super) runtime_outcome: RuntimeOutcome,
}

impl Receipt {
    pub(super) fn render(&self, style: Style) -> String {
        let title = style.step(Mark::Done, &style.heading("OpenAPPA activated for Claude Code"));
        let field = |name: &str, value: &str| style.field(name, 9, value);
        let mut receipt = format!(
            "{title}\n\n{}\n{}\n{}\n{}\n{}\n",
            field("Adapter", &self.adapter),
            field("Hooks", &friendly_path(&self.hooks)),
            field("Runtime", self.runtime_outcome.as_str()),
            field("Config", &friendly_path(&self.config)),
            field("Launcher", "clappa"),
        );
        // A session loads its hooks at session start, and the hook wire carries no
        // version, so a session running across an upgrade keeps talking to the
        // runtime it started with.
        receipt.push_str("\nRestart any running `clappa` session to pick this up.\n");
        receipt.push_str("Resume a protected conversation with `clappa --resume`, not `claude --resume`.\n");
        receipt.push_str("Exit and restart any conversation already resumed through plain `claude`.\n");
        receipt.push_str("\nNext: run `clappa`, then `/appa-guide`.\n");
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
            hooks: PathBuf::from("/home/me/.claude/settings.json"),
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
                hooks: PathBuf::from("/home/me/.claude/settings.json"),
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
