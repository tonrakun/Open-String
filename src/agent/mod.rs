mod aggregate;
mod claude_executor;
mod confirmation;
mod conversation;
mod ctx_agent;
mod executor;
mod mediator;
mod progress;
mod respond;
mod result;
mod scope;
mod sub_agent;
mod system_prompt;
mod task;
mod tools;

pub use aggregate::AggregatedReport;
pub use claude_executor::ClaudeTaskExecutor;
pub use confirmation::{CliConfirmationPrompt, ConfirmationPrompt};
pub use conversation::{MediatorTurn, plan};
pub use ctx_agent::{
    CtxAgentConfig, FileMemoryStore, MemoryStore, clear_stale_tool_results, compact,
    is_phase_boundary, should_compact,
};
pub use executor::TaskExecutor;
pub use mediator::{DispatchError, Mediator, MediatorConfig};
pub use progress::{FileProgressMemoStore, ProgressMemoStore};
pub use respond::{natural_language_response, render_report};
pub use result::{TaskOutcome, TaskResult};
pub use scope::TaskScope;
pub use sub_agent::SubAgent;
pub use system_prompt::{SystemPromptBuilder, load_connected_extensions};
pub use task::Task;
