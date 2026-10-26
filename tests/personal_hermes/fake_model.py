"""A deterministic OpenAI-compatible model for the end-to-end run.

A user message `run: <command>` is answered with one `terminal` tool call for
that command; once the tool result is in the conversation, with a final
message quoting the tool result. Anything else gets a plain reply. Requests
are recorded only as their path, the SHA-256 of their bearer token and the
model they asked for, so a test can prove which key and setting arrived without the key
ever being exposed.

Standard library only: it runs in the Hermes image with its Python.
"""

import hashlib
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MODEL = "fake-model"
_seen = []
_lock = threading.Lock()


def _reply(messages):
    """The assistant turn for this conversation: a tool call or text."""
    last_user = next(
        (m for m in reversed(messages) if m.get("role") == "user"), {"content": ""}
    )
    content = last_user.get("content") or ""
    if isinstance(content, list):
        content = " ".join(
            part.get("text", "") for part in content if isinstance(part, dict)
        )
    answered = any(m.get("role") == "tool" for m in messages)
    # A scheduled run wraps the saved prompt, so `run:` may follow other text.
    marker = content.rfind("run:")
    if marker >= 0 and not answered:
        command = content[marker + len("run:") :].strip().splitlines()[0].strip()
        return {
            "tool_calls": [
                {
                    "id": "call_e2e",
                    "type": "function",
                    "function": {
                        "name": "terminal",
                        "arguments": json.dumps({"command": command}),
                    },
                }
            ]
        }
    tool = next((m for m in reversed(messages) if m.get("role") == "tool"), None)
    text = "done: " + str(tool.get("content", ""))[:200] if tool else "hello from e2e"
    return {"content": text}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _json(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path.rstrip("/").endswith("/models"):
            self._json(
                200, {"object": "list", "data": [{"id": MODEL, "object": "model"}]}
            )
        elif self.path == "/seen":
            with _lock:
                self._json(200, {"requests": list(_seen)})
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) or b"{}")
        bearer = (self.headers.get("Authorization") or "").removeprefix("Bearer ")
        with _lock:
            _seen.append(
                {
                    "path": self.path,
                    "key": hashlib.sha256(bearer.encode()).hexdigest(),
                    "model": body.get("model"),
                }
            )
        if not self.path.rstrip("/").endswith("/chat/completions"):
            self._json(404, {"error": "not found"})
            return
        message = _reply(body.get("messages", []))
        finish = "tool_calls" if "tool_calls" in message else "stop"
        created = int(time.time())
        if not body.get("stream"):
            self._json(
                200,
                {
                    "id": "chatcmpl-e2e",
                    "object": "chat.completion",
                    "created": created,
                    "model": MODEL,
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", **message},
                            "finish_reason": finish,
                        }
                    ],
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1,
                        "total_tokens": 2,
                    },
                },
            )
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        delta = {"role": "assistant", **message}
        if "tool_calls" in delta:
            delta["tool_calls"] = [dict(call, index=0) for call in delta["tool_calls"]]
        for chunk in (
            {"delta": delta, "finish_reason": None},
            {"delta": {}, "finish_reason": finish},
        ):
            event = {
                "id": "chatcmpl-e2e",
                "object": "chat.completion.chunk",
                "created": created,
                "model": MODEL,
                "choices": [{"index": 0, **chunk}],
            }
            self.wfile.write(f"data: {json.dumps(event)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
