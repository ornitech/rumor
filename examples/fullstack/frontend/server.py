#!/usr/bin/env python3
"""Tiny stdlib-only static server for the rumor fullstack example.

Serves index.html with a small bit of templating so the page can show:
  - FRONTEND_TITLE (sourced from the env, demonstrating per-service override)
  - the API_URL the page should call (built from API_HOST + API_PORT)
  - the LOG_LEVEL this server saw at boot (which should be 'warn' here,
    because the JSON config's `env` block overrides both env-file layers)
"""
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


HERE = Path(__file__).resolve().parent
INDEX = HERE / "index.html"


def render():
    title = os.environ.get("FRONTEND_TITLE", "Rumor Fullstack Example")
    api_host = os.environ.get("API_HOST", "localhost")
    api_port = os.environ.get("API_PORT", "3000")
    log_level = os.environ.get("LOG_LEVEL", "info")
    api_url = f"http://{api_host}:{api_port}"
    html = INDEX.read_text()
    return (
        html.replace("__TITLE__", title)
        .replace("__API_URL__", api_url)
        .replace("__LOG_LEVEL__", log_level)
    )


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path not in ("/", "/index.html"):
            self.send_response(404)
            self.end_headers()
            return
        body = render().encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        sys.stderr.write("[frontend] " + (fmt % args) + "\n")


def main():
    port = int(os.environ.get("FRONTEND_PORT", "8080"))
    log_level = os.environ.get("LOG_LEVEL", "info")
    title = os.environ.get("FRONTEND_TITLE", "Rumor Fullstack Example")
    print(f"[frontend] starting on port {port}", flush=True)
    print(f"[frontend]   LOG_LEVEL={log_level}  (set by JSON config env block)", flush=True)
    print(f"[frontend]   FRONTEND_TITLE={title}  (set by .env.local override)", flush=True)
    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
