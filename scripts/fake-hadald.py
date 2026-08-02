#!/usr/bin/env python3
"""A stand-in for hadald, for testing the broker without a language model.

Speaks enough of Ollama's HTTP API for hadal-brokerd: GET /api/tags for
readiness, POST /api/generate for a streamed NDJSON reply.

The reply is read from a file and emitted in small chunks, deliberately
splitting the ```hadal-action fences across chunk boundaries. That is the
failure mode a naive scanner has, and it is exactly what a real model does —
so the fake must do it too or the test is easier than reality.

    fake-hadald.py --reply-file scenario.txt [--port 11434] [--chunk 7]
"""

from __future__ import annotations

import argparse
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

REPLY = ""
CHUNK = 7


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # quiet
        pass

    def do_GET(self):
        if self.path.startswith("/api/tags"):
            body = json.dumps(
                {"models": [{"name": "hadal-mini:latest"}, {"name": "hadal:latest"}]}
            ).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)

    def do_POST(self):
        if not self.path.startswith("/api/generate"):
            self.send_error(404)
            return

        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)

        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

        def emit(obj):
            line = (json.dumps(obj) + "\n").encode()
            self.wfile.write(f"{len(line):X}\r\n".encode() + line + b"\r\n")
            self.wfile.flush()

        for i in range(0, len(REPLY), CHUNK):
            emit({"response": REPLY[i : i + CHUNK], "done": False})
        emit({"response": "", "done": True})
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()


def main() -> int:
    global REPLY, CHUNK
    ap = argparse.ArgumentParser()
    ap.add_argument("--reply-file", required=True)
    ap.add_argument("--port", type=int, default=11434)
    ap.add_argument("--chunk", type=int, default=7)
    args = ap.parse_args()

    with open(args.reply_file, encoding="utf-8") as fh:
        REPLY = fh.read()
    CHUNK = args.chunk

    server = HTTPServer(("127.0.0.1", args.port), Handler)
    print(f"fake-hadald on 127.0.0.1:{args.port}, {len(REPLY)} chars in {CHUNK}-char chunks",
          file=sys.stderr, flush=True)
    server.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
