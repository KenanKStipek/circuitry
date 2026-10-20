#!/usr/bin/env python3
"""A fake pi in --mode json: streams one session with two slow tool calls. Marker strings test privacy."""
import json, sys, time
if "--version" in sys.argv:
    print("pi 0.0.0-fake"); sys.exit(0)
def out(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
out({"type": "session", "id": "fake-session-1", "cwd": "."})
out({"type": "turn_start"})
out({"type": "message_end", "message": {"role": "assistant", "model": "fake-model-1", "usage": {"input": 1200, "output": 40, "cacheRead": 800, "cacheWrite": 100, "cost": {"total": 0.012}},
     "content": [{"type": "thinking", "thinking": "MARKER_THINKING plan"}, {"type": "toolCall", "id": "c1", "name": "bash", "arguments": {"command": "pytest -k MARKER_COMMAND"}}], "stopReason": "toolUse"}})
out({"type": "tool_execution_start", "toolCallId": "c1", "toolName": "bash", "args": {"command": "pytest -k MARKER_COMMAND"}})
time.sleep(2.5)
out({"type": "tool_execution_end", "toolCallId": "c1", "toolName": "bash", "isError": False, "durationMs": 2500, "result": {"content": [{"type": "text", "text": "MARKER_TOOL_OUTPUT 41 passed"}]}})
out({"type": "turn_end"})
out({"type": "turn_start"})
out({"type": "message_end", "message": {"role": "assistant", "model": "fake-model-1", "usage": {"input": 2400, "output": 60, "cacheRead": 2000, "cacheWrite": 50, "cost": {"total": 0.02}},
     "content": [{"type": "toolCall", "id": "c2", "name": "edit", "arguments": {"path": "src/MARKER_PATH.py", "oldText": "a", "newText": "MARKER_FILE_CONTENT\nline2\n"}}], "stopReason": "toolUse"}})
out({"type": "tool_execution_start", "toolCallId": "c2", "toolName": "edit", "args": {"path": "src/MARKER_PATH.py", "oldText": "a", "newText": "MARKER_FILE_CONTENT\nline2\n"}})
time.sleep(2.5)
out({"type": "tool_execution_end", "toolCallId": "c2", "toolName": "edit", "isError": False, "durationMs": 2500, "result": {"content": [{"type": "text", "text": "MARKER_TOOL_OUTPUT edited"}]}})
out({"type": "turn_end"})
out({"type": "turn_start"})
out({"type": "message_end", "message": {"role": "assistant", "model": "fake-model-1", "usage": {"input": 2600, "output": 30, "cacheRead": 2400, "cacheWrite": 0, "cost": {"total": 0.008}},
     "content": [{"type": "text", "text": "Done. MARKER_REPLY all tests pass."}], "stopReason": "stop"}})
out({"type": "turn_end"})
out({"type": "agent_end"})
