pub struct Prompt {
    pub name: &'static str,
    pub text: &'static str,
}

pub const PROMPTS: &[Prompt] = &[
    Prompt {
        name: "current-session-message-ids-tool-description.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/current-session-message-ids-tool-description.md"),
    },
    Prompt {
        name: "current-session-message-ids.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/current-session-message-ids.md"),
    },
    Prompt {
        name: "partial-compact-arg-ranges.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/partial-compact-arg-ranges.md"),
    },
    Prompt {
        name: "partial-compact-instruction-pointer.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/partial-compact-instruction-pointer.md"),
    },
    Prompt {
        name: "partial-compact-instruction-tool-description.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/partial-compact-instruction-tool-description.md"),
    },
    Prompt {
        name: "partial-compact-instruction.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/partial-compact-instruction.md"),
    },
    Prompt {
        name: "partial-compact-range-from-message-id.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/partial-compact-range-from-message-id.md"),
    },
    Prompt {
        name: "partial-compact-range-summary.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/partial-compact-range-summary.md"),
    },
    Prompt {
        name: "partial-compact-range-to-message-id.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/partial-compact-range-to-message-id.md"),
    },
    Prompt {
        name: "partial-compact-reminder.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/partial-compact-reminder.md"),
    },
    Prompt {
        name: "partial-compact-tool-description.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/partial-compact-tool-description.md"),
    },
    Prompt {
        name: "tui-partial-compact.md",
        text: include_str!("../vendor/agent_partial_compact_common/prompts/tui-partial-compact.md"),
    },
];

pub fn get(name: &str) -> Option<&'static str> {
    PROMPTS
        .iter()
        .find(|prompt| prompt.name == name)
        .map(|prompt| prompt.text)
}

pub fn current_session_message_ids(message_id_lines: &str) -> String {
    render(
        get("current-session-message-ids.md").expect("current-session-message-ids.md is embedded"),
        message_id_lines,
    )
}

pub fn partial_compact_developer_instructions() -> String {
    [
        "PCODX partial compaction is available in this Codex session through dynamic tools.",
        get("partial-compact-instruction.md").expect("partial-compact-instruction.md is embedded"),
        "Use visible `msg...` message ids or `cmp...` compacted-range ids in `partial_compact` ranges; call `partial_compact_current_session_message_ids` if you need to refresh the current visible id list.",
        "`cmp...` ids refer to already-compacted ranges and can be used as range endpoints when merging or replacing older summaries.",
    ]
    .join("\n")
}

pub fn partial_compact_tool_description() -> String {
    get("partial-compact-tool-description.md")
        .expect("partial-compact-tool-description.md is embedded")
        .replace(
            "{{INSTRUCTION_POINTER}}",
            get("partial-compact-instruction-pointer.md")
                .expect("partial-compact-instruction-pointer.md is embedded")
                .trim(),
        )
}

fn render(template: &str, message_id_lines: &str) -> String {
    template.replace("{{MESSAGE_ID_LINES}}", message_id_lines)
}

#[cfg(test)]
mod tests {
    use super::{current_session_message_ids, partial_compact_tool_description, PROMPTS};
    use std::fs;
    use std::path::Path;

    #[test]
    fn vendored_prompts_match_opencode_source_when_present() {
        let source_dir = Path::new("vendor/agent_partial_compact_common/prompts");
        if !source_dir.exists() {
            return;
        }
        for prompt in PROMPTS {
            let source =
                fs::read_to_string(source_dir.join(prompt.name)).expect("source prompt readable");
            assert_eq!(prompt.text, source, "prompt changed: {}", prompt.name);
        }
    }

    #[test]
    fn renders_current_session_message_id_lines() {
        let rendered = current_session_message_ids("- msg1, cmp1");
        assert!(rendered.contains("Current-session message IDs"));
        assert!(rendered.contains("- msg1, cmp1"));
        assert!(!rendered.contains("{{MESSAGE_ID_LINES}}"));
    }

    #[test]
    fn renders_partial_compact_tool_description_pointer() {
        let rendered = partial_compact_tool_description();
        assert!(rendered.contains("Before using `partial_compact`"));
        assert!(!rendered.contains("{{INSTRUCTION_POINTER}}"));
    }
}
