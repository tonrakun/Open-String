mod aggregate;
mod claude_executor;
mod confirmation;
mod executor;
mod mediator;
mod result;
mod sub_agent;
mod task;
mod tools;

pub use aggregate::{AggregatedItem, AggregatedReport, Conflict};
pub use claude_executor::ClaudeTaskExecutor;
pub use confirmation::{CliConfirmationPrompt, ConfirmationPrompt};
pub use executor::TaskExecutor;
pub use mediator::{DispatchError, Mediator, MediatorConfig};
pub use result::{TaskOutcome, TaskResult};
pub use sub_agent::SubAgent;
pub use task::Task;
