#!/usr/bin/env python3
"""A fake Claude Code in stream-json mode: one API call split over two lines sharing message.id, a subagent line,
and slow tool calls, so the live summary can be sampled mid-session. Expected live values are printed by the probe."""
import json, sys, time
if "--version" in sys.argv:
    print("2.1.300 (Claude Code fake)"); sys.exit(0)
def out(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
u1 = {"input_tokens": 1000, "output_tokens": 1, "cache_read_input_tokens": 500, "cache_creation_input_tokens": 100}
out({"type": "system", "subtype": "init", "session_id": "fake-claude-1", "model": "claude-fake"})
# one API call (msg_1) printed as two lines: a text block, then a tool_use block, both with the same id and usage
out({"type": "assistant", "message": {"id": "msg_1", "usage": u1, "content": [{"type": "text", "text": "MARKER_REPLY looking"}]}})
out({"type": "assistant", "message": {"id": "msg_1", "usage": u1, "content": [{"type": "tool_use", "id": "tu_1", "name": "Task", "input": {"prompt": "MARKER_COMMAND sub"}}]}})
# a subagent's own API call: its usage must stay out, its tool call counts
out({"type": "assistant", "parent_tool_use_id": "tu_1", "message": {"id": "msg_sub", "usage": {"input_tokens": 9000, "output_tokens": 1, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0},
     "content": [{"type": "tool_use", "id": "tu_sub", "name": "Read", "input": {"file_path": "MARKER_PATH.md"}}]}})
time.sleep(2.5)   # sample here: turns 1, tool_calls 2, tokens_sent 1600
out({"type": "user", "parent_tool_use_id": "tu_1", "message": {"content": [{"type": "tool_result", "tool_use_id": "tu_sub", "content": "MARKER_TOOL_OUTPUT", "is_error": False}]}})
out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "MARKER_TOOL_OUTPUT done", "is_error": False}]}})
u2 = {"input_tokens": 1200, "output_tokens": 1, "cache_read_input_tokens": 1500, "cache_creation_input_tokens": 0}
out({"type": "assistant", "message": {"id": "msg_2", "usage": u2, "content": [{"type": "tool_use", "id": "tu_2", "name": "Edit", "input": {"file_path": "src/MARKER_PATH.py", "old_string": "a", "new_string": "MARKER_FILE_CONTENT\nb\n"}}]}})
time.sleep(2.5)   # sample here: turns 2, tool_calls 3, tokens_sent 1600 + 2700 = 4300
out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "tu_2", "content": "MARKER_TOOL_OUTPUT ok", "is_error": False}]}})
u3 = {"input_tokens": 1300, "output_tokens": 50, "cache_read_input_tokens": 2700, "cache_creation_input_tokens": 0}
out({"type": "assistant", "message": {"id": "msg_3", "usage": u3, "content": [{"type": "text", "text": "Done. MARKER_REPLY"}]}})
out({"type": "result", "subtype": "success", "is_error": False, "result": "Done. MARKER_REPLY", "session_id": "fake-claude-1", "num_turns": 3,
     "total_cost_usd": 0.05, "usage": {"input_tokens": 3500, "output_tokens": 120, "cache_read_input_tokens": 4700, "cache_creation_input_tokens": 100},
     "modelUsage": {"claude-fake": {"contextWindow": 200000, "maxOutputTokens": 8192}}})
