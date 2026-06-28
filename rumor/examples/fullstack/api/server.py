#!/usr/bin/env python3
"""Tiny stdlib-only HTTP API for the rumor fullstack example.

Prints which env values it observed at startup, then serves:
  GET /         -> JSON dump of the relevant env vars
  GET /healthz  -> "ok"

The point of this server is not to be a real API. It exists to make the env
layering visible: hit `/` and check that LOG_LEVEL is "debug" (proving the
per-service .env.local overrode the central .env).
"""
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def env_snapshot():
    keys = [
        "LOG_LEVEL",
        "POSTGRES_HOST",
        "POSTGRES_PORT",
        "POSTGRES_USER",
        "POSTGRES_DB",
        "REDIS_HOST",
        "REDIS_PORT",
        "API_HOST",
        "API_PORT",
    ]
    return {k: os.environ.get(k, "<unset>") for k in keys}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/healthz":
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Access-Control-Allow-Origin", "*")
            self.end_headers()
            self.wfile.write(b"ok\n")
            return

        body = json.dumps(
            {
                "service": "api",
                "env": env_snapshot(),
                "message": "If LOG_LEVEL is 'debug', the api/.env.local override worked.",
            },
            indent=2,
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        sys.stderr.write("[api] " + (fmt % args) + "\n")


def main():
    port = int(os.environ.get("API_PORT", "3000"))
    snap = env_snapshot()
    print(f"[api] starting on port {port}", flush=True)
    for k, v in snap.items():
        print(f"[api]   {k}={v}", flush=True)
    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
