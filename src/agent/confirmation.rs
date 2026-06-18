/// Asks the user to confirm an operation the active permission level
/// requires confirmation for. Abstracted so the Mediator's decision logic
/// doesn't depend on a particular front end (CLI today; TUI/GUI/chat
/// gateway later, 4.3/4.4).
pub trait ConfirmationPrompt {
    fn confirm(&self, summary: &str) -> bool;
}

/// Confirms over stdin/stderr, the same y/n flow used for god mode
/// reconfirmation (`crate::prompt::yes_no`).
pub struct CliConfirmationPrompt;

impl ConfirmationPrompt for CliConfirmationPrompt {
    fn confirm(&self, summary: &str) -> bool {
        eprintln!("This operation requires confirmation: {summary}");
        crate::prompt::yes_no("Proceed? [y/N]: ").unwrap_or(false)
    }
}
