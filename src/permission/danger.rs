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
}

impl DangerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DangerKind::Delete => "delete",
            DangerKind::Send => "send",
            DangerKind::ExternalTransmit => "external-transmit",
            DangerKind::Billing => "billing",
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
}
