<instruction name="opencode-partial-compact">
Partial compaction replaces no-longer-needed messages from the agent's context window with agent-provided summaries.

Strongly consider partial compaction for any stale context not very likely to be useful soon: tool output with much shorter takeaways, resolved detours, repeated investigation, obsolete edits, or anything irrelevant to the current task.

Each summary should say what was removed, why it is safe to forget, and any durable facts needed later, in short. Include old message IDs iff they are likely to be useful for precise recovery.

MUST preserve instead of replace:
- active system, developer, tool, and user instructions;
- user prompts, except bulky pasted logs, traces, generated output, or other replaceable bulk;
- key decisions, assumptions, unresolved questions, blockers, and risks;
- information needed for the current task or immediately foreseeable follow-up work.

Call `partial_compact` with `ranges: [{ from_message_id, to_message_id, summary }]`. Each object compacts only the selected message range. Batch all message ranges into a single call. Example: `partial_compact({ranges: [{from_message_id:"msg1",to_message_id:"msg4",summary:"msg1, msg 2 did blah."},{from_message_id:"msg11",to_message_id:"msg18",summary:"Finished blah thru iterations including msg15, msg16, msg17, msg18."}]})`.
If you do not remember message IDs, you may use `partial_compact_current_session_message_ids` to list the current visible message IDs.

Tail compaction: regardless of current context size, aggressively summarize the newest unneeded messages to keep the working context lean while preserving KV cache.

Full-session compaction: on higher context usage, compact stale context more aggressively, starting with recent ranges and moving backward as needed.

Try to keep your context window under 50%.

Original messages remain in the session log. If you need them later, use message search/read tools for the current session history: `session_search` and `session_read`.
</instruction>
