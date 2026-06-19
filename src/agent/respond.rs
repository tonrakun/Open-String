use super::TaskOutcome;
use super::aggregate::AggregatedReport;
use crate::llm::{ClaudeClient, ClaudeError, ContentBlock, Message};

/// Phrases an `AggregatedReport` for the user (4.7.1). The Mediator is the
/// only component that talks to the user in natural language; it calls the
/// model directly here (no tools, no Sub Agent) purely to rewrite the
/// already-computed structured report into a concise reply. It must not
/// invent information beyond what the report contains.
const MEDIATOR_RESPONSE_SYSTEM_PROMPT: &str = "You are the Mediator in the Open String \
system, the only component that speaks to the user in natural language. You are given a \
structured report aggregating one or more Sub Agents' task results. Rewrite it as a concise, \
natural-language reply to the user: summarize what succeeded and what failed, call out any \
conflicting results and how they were resolved, and mention anything that was denied and why. \
Do not invent information beyond what the report contains, and do not describe the internal \
aggregation process itself.";

pub fn natural_language_response(
    client: &ClaudeClient,
    report: &AggregatedReport,
) -> Result<String, ClaudeError> {
    if report.items.is_empty() && report.conflicts.is_empty() && report.denied.is_empty() {
        return Ok("There were no tasks to report on.".to_string());
    }

    let rendered = render_report(report);
    let response = client.send(
        MEDIATOR_RESPONSE_SYSTEM_PROMPT,
        &[Message::user_text(rendered)],
        &[],
    )?;

    Ok(response
        .blocks
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Deterministic plain-text rendering of a report, used as the model input
/// above. Kept separate (and public) so it doubles as a non-LLM fallback.
pub fn render_report(report: &AggregatedReport) -> String {
    let mut sections = Vec::new();

    if !report.items.is_empty() {
        let mut lines = vec!["Completed tasks:".to_string()];
        for item in &report.items {
            let agreement = if item.duplicate_count > 1 {
                format!(" ({} sub agents agreed)", item.duplicate_count)
            } else {
                String::new()
            };
            lines.push(format!(
                "- [{}] {}: {}{agreement}",
                outcome_label(item.outcome),
                item.description,
                item.summary
            ));
        }
        sections.push(lines.join("\n"));
    }

    if !report.conflicts.is_empty() {
        let mut lines = vec!["Conflicting results (resolved by majority vote):".to_string()];
        for conflict in &report.conflicts {
            lines.push(format!(
                "- {} -> resolved as {}",
                conflict.description,
                outcome_label(conflict.resolved_outcome)
            ));
            for (outcome, summary) in &conflict.results {
                lines.push(format!("    - [{}] {}", outcome_label(*outcome), summary));
            }
        }
        sections.push(lines.join("\n"));
    }

    if !report.denied.is_empty() {
        let mut lines = vec!["Denied before execution:".to_string()];
        for denied in &report.denied {
            lines.push(format!("- {}: {}", denied.description, denied.reason));
        }
        sections.push(lines.join("\n"));
    }

    sections.join("\n\n")
}

fn outcome_label(outcome: TaskOutcome) -> &'static str {
    match outcome {
        TaskOutcome::Success => "success",
        TaskOutcome::Failure => "failure",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::aggregate::{AggregatedItem, Conflict, DeniedTask};
    use httpmock::Method::POST;
    use httpmock::MockServer;

    #[test]
    fn empty_report_short_circuits_without_calling_the_api() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200);
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let report = AggregatedReport::default();

        let response = natural_language_response(&client, &report).unwrap();

        assert!(response.contains("no tasks"));
        mock.assert_hits(0);
    }

    #[test]
    fn render_report_includes_items_conflicts_and_denials() {
        let report = AggregatedReport {
            items: vec![AggregatedItem {
                description: "read config".to_string(),
                outcome: TaskOutcome::Success,
                summary: "config is valid".to_string(),
                duplicate_count: 2,
            }],
            conflicts: vec![Conflict {
                description: "deploy service".to_string(),
                resolved_outcome: TaskOutcome::Failure,
                results: vec![
                    (TaskOutcome::Success, "deployed".to_string()),
                    (TaskOutcome::Failure, "timed out".to_string()),
                ],
            }],
            denied: vec![DeniedTask {
                description: "delete database".to_string(),
                reason: "requires confirmation".to_string(),
            }],
        };

        let rendered = render_report(&report);

        assert!(rendered.contains("read config"));
        assert!(rendered.contains("2 sub agents agreed"));
        assert!(rendered.contains("deploy service"));
        assert!(rendered.contains("timed out"));
        assert!(rendered.contains("delete database"));
        assert!(rendered.contains("requires confirmation"));
    }

    #[test]
    fn sends_rendered_report_and_returns_the_models_text() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(200).json_body(serde_json::json!({
                "content": [{"type": "text", "text": "All good: config checked out fine."}],
                "stop_reason": "end_turn"
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let report = AggregatedReport {
            items: vec![AggregatedItem {
                description: "read config".to_string(),
                outcome: TaskOutcome::Success,
                summary: "config is valid".to_string(),
                duplicate_count: 1,
            }],
            conflicts: vec![],
            denied: vec![],
        };

        let response = natural_language_response(&client, &report).unwrap();

        mock.assert();
        assert_eq!(response, "All good: config checked out fine.");
    }

    #[test]
    fn api_error_is_propagated() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/v1/messages");
            then.status(500).json_body(serde_json::json!({
                "error": {"message": "internal error"}
            }));
        });

        let client = ClaudeClient::new("sk-ant-test").with_base_url(server.base_url());
        let report = AggregatedReport {
            items: vec![AggregatedItem {
                description: "read config".to_string(),
                outcome: TaskOutcome::Success,
                summary: "config is valid".to_string(),
                duplicate_count: 1,
            }],
            conflicts: vec![],
            denied: vec![],
        };

        let result = natural_language_response(&client, &report);

        assert!(result.is_err());
    }
}
