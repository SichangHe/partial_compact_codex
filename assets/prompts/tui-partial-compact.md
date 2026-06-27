{{INSTRUCTION_POINTER}}

The user selected one partial-compaction checkpoint. This slash command is not automatic and does not open a multi-range picker; it gives you one exact range to summarize now.

Selected range:
- from_message_id: {{FROM_MESSAGE_ID}}
- to_message_id: {{TO_MESSAGE_ID}}
- checkpoint: {{CHECKPOINT_TITLE}}

Write a concise replacement summary for exactly this selected range, then call `partial_compact` with one `ranges` item using these exact message IDs. If you independently identify additional disjoint stale ranges while following the instruction, include them as additional items in the same `ranges` call.
