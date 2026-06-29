use crate::prompts;
use crate::storage::{CompactionInput, Store};
use serde::{Deserialize, Serialize};

const DEFAULT_MAX_SUMMARY_CHARS: usize = 2000;

#[derive(Clone, Copy, Debug)]
pub struct ToolConfig {
    pub max_summary_chars: usize,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            max_summary_chars: DEFAULT_MAX_SUMMARY_CHARS,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PartialCompactArgs {
    #[serde(default)]
    pub ranges: Vec<PartialCompactRangeArg>,
}

#[derive(Debug, Deserialize)]
pub struct PartialCompactRangeArg {
    pub from_message_id: Option<String>,
    pub to_message_id: Option<String>,
    pub summary: Option<String>,
    pub target_session_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PartialCompactResponse {
    pub ranges_compacted: Vec<CompactedRangeResponse>,
    pub n_ranges_compacted: usize,
    pub n_messages_replaced: i64,
    pub truncated: bool,
    pub active_compactions: i64,
    pub total_known_messages_replaced: i64,
    pub session_id: String,
    pub note: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CompactedRangeResponse {
    pub session_id: String,
    pub from_message_id: String,
    pub to_message_id: String,
    pub n_messages_replaced: i64,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct ToolErrorResponse {
    pub error: String,
}

struct NormalizedRange {
    from_message_id: String,
    to_message_id: String,
    summary: String,
    truncated: bool,
}

pub fn partial_compact_json(
    store: &mut Store,
    current_session_id: &str,
    args_json: &str,
    cfg: ToolConfig,
) -> String {
    let args = match serde_json::from_str::<PartialCompactArgs>(args_json) {
        Ok(args) => args,
        Err(error) => return error_json(format!("invalid partial_compact args: {error}")),
    };
    match partial_compact(store, current_session_id, args, cfg) {
        Ok(response) => serialize_response(&response),
        Err(error) => error_json(error),
    }
}

pub fn partial_compact(
    store: &mut Store,
    current_session_id: &str,
    args: PartialCompactArgs,
    cfg: ToolConfig,
) -> Result<PartialCompactResponse, String> {
    let requested_ranges: Vec<_> = args.ranges.into_iter().filter(range_has_value).collect();
    if requested_ranges.is_empty() {
        return Err("provide ranges with at least one complete range".to_owned());
    }
    if requested_ranges.iter().any(range_missing_required) {
        return Err(
            "each range must include from_message_id, to_message_id, and summary".to_owned(),
        );
    }
    if requested_ranges.iter().any(has_conflicting_session_ids) {
        return Err(
            "target_session_id and legacy session_id must match when both are provided".to_owned(),
        );
    }
    if requested_ranges
        .iter()
        .any(|range| targets_another_session(range, current_session_id))
    {
        return Err(
            "partial_compact can only compact message ranges in the current session; omit session selectors"
                .to_owned(),
        );
    }
    let mut normalized = Vec::with_capacity(requested_ranges.len());
    for range in requested_ranges {
        let from_message_id = range.from_message_id.unwrap_or_default();
        let to_message_id = range.to_message_id.unwrap_or_default();
        let summary = range.summary.unwrap_or_default();
        let (summary, truncated) = truncate_summary(summary, cfg.max_summary_chars);
        normalized.push(NormalizedRange {
            from_message_id,
            to_message_id,
            summary,
            truncated,
        });
    }
    let inputs = normalized
        .iter()
        .map(|range| CompactionInput {
            from_msg_id: range.from_message_id.clone(),
            to_msg_id: range.to_message_id.clone(),
            summary: range.summary.clone(),
        })
        .collect();
    let compactions = store
        .compact_ranges(current_session_id, inputs)
        .map_err(|error| format!("session {current_session_id}: {error}"))?;
    let mut ranges_compacted = Vec::with_capacity(compactions.len());
    let mut n_messages_replaced = 0;
    for range in &normalized {
        let compaction = compactions
            .iter()
            .find(|compaction| {
                compaction.from_msg_id == range.from_message_id
                    && compaction.to_msg_id == range.to_message_id
            })
            .ok_or_else(|| {
                format!(
                    "session {current_session_id}: compacted range {}..{} was not returned by storage",
                    range.from_message_id, range.to_message_id
                )
            })?;
        n_messages_replaced += compaction.n_messages_replaced;
        ranges_compacted.push(CompactedRangeResponse {
            session_id: current_session_id.to_owned(),
            from_message_id: range.from_message_id.clone(),
            to_message_id: range.to_message_id.clone(),
            n_messages_replaced: compaction.n_messages_replaced,
            truncated: range.truncated,
        });
    }
    let stats = store
        .compaction_stats(current_session_id)
        .map_err(|error| format!("session {current_session_id}: {error}"))?;
    Ok(PartialCompactResponse {
        n_ranges_compacted: ranges_compacted.len(),
        truncated: normalized.iter().any(|range| range.truncated),
        ranges_compacted,
        n_messages_replaced,
        active_compactions: stats.active_compactions,
        total_known_messages_replaced: stats.total_known_messages_replaced,
        session_id: current_session_id.to_owned(),
        note: "The compacted ranges are removed from the model-visible view on subsequent calls; the original session log is unchanged.",
    })
}

pub fn current_session_message_ids_tool(store: &Store, current_session_id: &str) -> String {
    match store.current_session_message_id_lines(current_session_id) {
        Ok(lines) => prompts::current_session_message_ids(&lines),
        Err(error) => error_json(format!("session {current_session_id}: {error}")),
    }
}

fn range_missing_required(range: &PartialCompactRangeArg) -> bool {
    !has_value(&range.from_message_id)
        || !has_value(&range.to_message_id)
        || !has_value(&range.summary)
}

fn range_has_value(range: &PartialCompactRangeArg) -> bool {
    has_value(&range.from_message_id)
        || has_value(&range.to_message_id)
        || has_value(&range.summary)
        || has_value(&range.target_session_id)
        || has_value(&range.session_id)
}

fn has_conflicting_session_ids(range: &PartialCompactRangeArg) -> bool {
    match (
        requested_session_id_value(&range.target_session_id),
        requested_session_id_value(&range.session_id),
    ) {
        (Some(target_session_id), Some(session_id)) => target_session_id != session_id,
        _ => false,
    }
}

fn targets_another_session(range: &PartialCompactRangeArg, current_session_id: &str) -> bool {
    match requested_session_id(range) {
        Some(session_id) => session_id != current_session_id,
        None => false,
    }
}

fn requested_session_id(range: &PartialCompactRangeArg) -> Option<&str> {
    requested_session_id_value(&range.target_session_id)
        .or_else(|| requested_session_id_value(&range.session_id))
}

fn requested_session_id_value(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

fn has_value(value: &Option<String>) -> bool {
    requested_session_id_value(value).is_some()
}

fn truncate_summary(summary: String, max_summary_chars: usize) -> (String, bool) {
    if summary.chars().count() <= max_summary_chars {
        return (summary, false);
    }
    let kept = summary.chars().take(max_summary_chars).collect::<String>();
    (format!("{kept}[...truncated]"), true)
}

fn serialize_response<T: Serialize>(response: &T) -> String {
    serde_json::to_string(response).expect("tool response serialization cannot fail")
}

fn error_json(error: String) -> String {
    serialize_response(&ToolErrorResponse { error })
}

#[cfg(test)]
mod tests {
    use super::{current_session_message_ids_tool, partial_compact_json, ToolConfig};
    use crate::storage::{Role, Store};
    use serde_json::Value;
    use tempfile::tempdir;

    fn seeded_store() -> (tempfile::TempDir, Store, String) {
        let temp = tempdir().unwrap();
        let mut store = Store::open(&temp.path().join("pcodx.sqlite3")).unwrap();
        let session = store.create_session(Some("ses-tool"), temp.path()).unwrap();
        for text in ["old setup", "keep this", "old output", "old result"] {
            store
                .record_message(&session, Role::Assistant, text, None)
                .unwrap();
        }
        (temp, store, session)
    }

    #[test]
    fn partial_compact_json_compacts_current_session_ranges() {
        let (_temp, mut store, session) = seeded_store();
        let response = partial_compact_json(
            &mut store,
            &session,
            r#"{"ranges":[{"from_message_id":"msg1","to_message_id":"msg1","summary":"setup"},{"from_message_id":"msg3","to_message_id":"msg4","summary":"output"}]}"#,
            ToolConfig::default(),
        );
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["n_ranges_compacted"], 2);
        assert_eq!(value["n_messages_replaced"], 3);
        assert_eq!(value["active_compactions"], 2);
        assert_eq!(value["total_known_messages_replaced"], 3);
        assert_eq!(value["ranges_compacted"][0]["from_message_id"], "msg1");
        assert_eq!(value["ranges_compacted"][1]["to_message_id"], "msg4");
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec!["cmp1".to_owned(), "msg2".to_owned(), "cmp2".to_owned()]
        );
    }

    #[test]
    fn partial_compact_json_preserves_request_metadata_for_out_of_order_ranges() {
        let (_temp, mut store, session) = seeded_store();
        let response = partial_compact_json(
            &mut store,
            &session,
            r#"{"ranges":[{"from_message_id":"msg3","to_message_id":"msg4","summary":"abcdef"},{"from_message_id":"msg1","to_message_id":"msg1","summary":"x"}]}"#,
            ToolConfig {
                max_summary_chars: 3,
            },
        );
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["ranges_compacted"][0]["from_message_id"], "msg3");
        assert_eq!(value["ranges_compacted"][0]["to_message_id"], "msg4");
        assert_eq!(value["ranges_compacted"][0]["n_messages_replaced"], 2);
        assert_eq!(value["ranges_compacted"][0]["truncated"], true);
        assert_eq!(value["ranges_compacted"][1]["from_message_id"], "msg1");
        assert_eq!(value["ranges_compacted"][1]["to_message_id"], "msg1");
        assert_eq!(value["ranges_compacted"][1]["n_messages_replaced"], 1);
        assert_eq!(value["ranges_compacted"][1]["truncated"], false);
    }

    #[test]
    fn partial_compact_json_accepts_visible_cmp_endpoints_without_error_after_mutation() {
        let (_temp, mut store, session) = seeded_store();
        store
            .compact(&session, "msg1", "msg2", "old setup")
            .unwrap();
        let response = partial_compact_json(
            &mut store,
            &session,
            r#"{"ranges":[{"from_message_id":"cmp1","to_message_id":"msg3","summary":"merged"}]}"#,
            ToolConfig::default(),
        );
        let value: Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_none());
        assert_eq!(value["ranges_compacted"][0]["from_message_id"], "cmp1");
        assert_eq!(value["ranges_compacted"][0]["to_message_id"], "msg3");
        assert_eq!(value["ranges_compacted"][0]["n_messages_replaced"], 3);
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec!["cmp2".to_owned(), "msg4".to_owned()]
        );
    }

    #[test]
    fn partial_compact_json_rejects_cross_session_selectors_without_mutation() {
        let (_temp, mut store, session) = seeded_store();
        let response = partial_compact_json(
            &mut store,
            &session,
            r#"{"ranges":[{"target_session_id":"other","from_message_id":"msg1","to_message_id":"msg1","summary":"bad"}]}"#,
            ToolConfig::default(),
        );
        let value: Value = serde_json::from_str(&response).unwrap();
        assert!(value["error"].as_str().unwrap().contains("current session"));
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec![
                "msg1".to_owned(),
                "msg2".to_owned(),
                "msg3".to_owned(),
                "msg4".to_owned()
            ]
        );
    }

    #[test]
    fn partial_compact_json_reports_missing_required_fields_before_session_checks() {
        let (_temp, mut store, session) = seeded_store();
        let response = partial_compact_json(
            &mut store,
            &session,
            r#"{"ranges":[{"target_session_id":"other"}]}"#,
            ToolConfig::default(),
        );
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["error"],
            "each range must include from_message_id, to_message_id, and summary"
        );
        assert_eq!(
            store.visible_ids(&session).unwrap(),
            vec![
                "msg1".to_owned(),
                "msg2".to_owned(),
                "msg3".to_owned(),
                "msg4".to_owned()
            ]
        );
    }

    #[test]
    fn partial_compact_json_truncates_summary_before_store_write() {
        let (_temp, mut store, session) = seeded_store();
        let response = partial_compact_json(
            &mut store,
            &session,
            r#"{"ranges":[{"session_id":"ses-tool","from_message_id":"msg1","to_message_id":"msg1","summary":"abcdef"}]}"#,
            ToolConfig {
                max_summary_chars: 3,
            },
        );
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["truncated"], true);
        assert_eq!(value["ranges_compacted"][0]["truncated"], true);
        let rendered = store.render_visible_context(&session).unwrap();
        assert!(rendered.contains("abc[...truncated]"));
        assert!(!rendered.contains("abcdef"));
    }

    #[test]
    fn current_session_message_ids_json_uses_shared_helper_text() {
        let (_temp, mut store, session) = seeded_store();
        store.compact(&session, "msg1", "msg2", "old").unwrap();
        let output = current_session_message_ids_tool(&store, &session);
        assert!(output.contains("Current-session message IDs"));
        assert!(output.contains("- cmp1, msg3, msg4"));
    }
}
