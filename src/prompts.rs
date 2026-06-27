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

#[cfg(test)]
mod tests {
    use super::PROMPTS;
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
}
