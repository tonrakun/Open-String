/// Categories of operations considered irreversible or high-impact. This
/// classification is independent of the active `PermissionLevel`: every
/// level's confirmation policy (4.1) is defined in terms of whether an
/// operation falls into one of these kinds, not the other way around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DangerKind {
    Delete,
    Send,
    ExternalTransmit,
    Billing,
    ConfigEdit,
}

impl DangerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DangerKind::Delete => "delete",
            DangerKind::Send => "send",
            DangerKind::ExternalTransmit => "external-transmit",
            DangerKind::Billing => "billing",
            DangerKind::ConfigEdit => "config-edit",
        }
    }
}

impl std::fmt::Display for DangerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

const DELETE_KEYWORDS: &[&str] = &["delete", "remove", "rm ", "drop ", "uninstall", "削除"];
const SEND_KEYWORDS: &[&str] = &["send", "email", "message", "publish", "post to", "送信"];
const EXTERNAL_TRANSMIT_KEYWORDS: &[&str] =
    &["upload", "webhook", "external api", "transmit", "外部送信"];
const BILLING_KEYWORDS: &[&str] = &[
    "charge",
    "purchase",
    "pay ",
    "payment",
    "billing",
    "subscribe",
    "課金",
];
/// Self-edits to Core's own config/MCP settings (5.4's "Mediator主導による
/// Extension動的導入" still requires user confirmation before such an edit
/// is allowed to slip through unattended).
const CONFIG_EDIT_KEYWORDS: &[&str] = &[
    "edit mcp.json",
    "edit mcp config",
    "edit settings.json",
    "edit core config",
    "modify mcp config",
    "modify settings.json",
    "modify core config",
    "update mcp config",
    "update settings.json",
    "update core config",
    "write to settings.json",
    "write to mcp.json",
    "rewrite mcp config",
    "change mcp config",
    ".open-string/config",
    "claude_desktop_config",
    "mcp設定を変更",
    "mcp設定を編集",
    "コンフィグを編集",
    "コンフィグを変更",
    "設定ファイルを編集",
    "設定ファイルを変更",
];

/// Classifies a free-text operation description (e.g. a planned tool call
/// or shell command) into zero or more dangerous categories. Matching is a
/// deliberately simple keyword scan rather than fuzzy NLP, so callers (the
/// future Mediator pre-check in 4.7) get predictable, auditable results.
pub fn classify(operation: &str) -> Vec<DangerKind> {
    let lower = operation.to_lowercase();
    let mut kinds = Vec::new();
    if contains_any(&lower, DELETE_KEYWORDS) {
        kinds.push(DangerKind::Delete);
    }
    if contains_any(&lower, SEND_KEYWORDS) {
        kinds.push(DangerKind::Send);
    }
    if contains_any(&lower, EXTERNAL_TRANSMIT_KEYWORDS) {
        kinds.push(DangerKind::ExternalTransmit);
    }
    if contains_any(&lower, BILLING_KEYWORDS) {
        kinds.push(DangerKind::Billing);
    }
    if contains_any(&lower, CONFIG_EDIT_KEYWORDS) {
        kinds.push(DangerKind::ConfigEdit);
    }
    kinds
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_delete_operations() {
        assert_eq!(
            classify("delete the staging branch"),
            vec![DangerKind::Delete]
        );
        assert_eq!(classify("rm -rf build/"), vec![DangerKind::Delete]);
    }

    #[test]
    fn classifies_send_operations() {
        assert_eq!(
            classify("send an email to the team"),
            vec![DangerKind::Send]
        );
    }

    #[test]
    fn classifies_external_transmit_operations() {
        assert_eq!(
            classify("upload the report to the external api"),
            vec![DangerKind::ExternalTransmit]
        );
    }

    #[test]
    fn classifies_billing_operations() {
        assert_eq!(
            classify("charge the customer's card"),
            vec![DangerKind::Billing]
        );
    }

    #[test]
    fn matches_multiple_kinds_at_once() {
        let kinds = classify("delete the invoice and charge a refund");
        assert!(kinds.contains(&DangerKind::Delete));
        assert!(kinds.contains(&DangerKind::Billing));
    }

    #[test]
    fn benign_operations_are_not_dangerous() {
        assert!(classify("read the config file").is_empty());
        assert!(classify("list directory contents").is_empty());
    }

    #[test]
    fn classifies_mcp_and_core_config_self_edits() {
        assert_eq!(
            classify("edit mcp.json to add a new server"),
            vec![DangerKind::ConfigEdit]
        );
        assert_eq!(
            classify("update settings.json with the new permission level"),
            vec![DangerKind::ConfigEdit]
        );
        assert_eq!(
            classify("コンフィグを編集して権限を変える"),
            vec![DangerKind::ConfigEdit]
        );
    }

    #[test]
    fn merely_reading_a_config_file_is_not_flagged_as_config_edit() {
        assert!(classify("read the mcp config").is_empty());
        assert!(classify("設定ファイルを読み込む").is_empty());
    }
}
