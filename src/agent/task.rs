/// A unit of work the Mediator may delegate to a single-use Sub Agent.
/// `description` doubles as the input to the permission danger classifier
/// (`permission::classify_danger`), so it should read like an instruction
/// (e.g. "delete the staging branch") rather than a vague label.
#[derive(Debug, Clone)]
pub struct Task {
    pub description: String,
    pub read_only: bool,
}

impl Task {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            read_only: false,
        }
    }

    pub fn read_only(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            read_only: true,
        }
    }
}
