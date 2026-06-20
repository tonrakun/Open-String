mod aggregate;
mod bundled_extensions;
mod claude_executor;
mod confirmation;
mod conversation;
mod ctx_agent;
mod executor;
mod mcp_memory;
mod mcp_tools;
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
pub use bundled_extensions::auto_register_t0k3n;
pub use claude_executor::ClaudeTaskExecutor;
pub use confirmation::{CliConfirmationPrompt, ConfirmationPrompt};
pub use conversation::{MediatorTurn, ProposedExtension, plan};
pub use ctx_agent::{
    CtxAgentConfig, FileMemoryStore, MemoryStore, clear_stale_tool_results, compact,
    ctx_agent_config_path, estimate_history_tokens, is_phase_boundary, load_ctx_agent_config,
    save_ctx_agent_config, should_compact,
};
pub use executor::TaskExecutor;
pub use mcp_memory::connect_for_state_management;
pub use mcp_tools::connect_workspace_tools;
pub use mediator::{DispatchError, Mediator, MediatorConfig};
pub use progress::{FileProgressMemoStore, ProgressMemoStore};
pub use respond::{natural_language_response, render_report};
pub use result::{TaskOutcome, TaskResult};
pub use scope::TaskScope;
pub use sub_agent::SubAgent;
pub use system_prompt::{SystemPromptBuilder, load_connected_extensions, register_extension};
pub use task::Task;
