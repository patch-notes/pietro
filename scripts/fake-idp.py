#!/usr/bin/env python3
"""Tiny dev-only OIDC stub for smoke-testing `pietro serve` without a real IdP.

Serves the bare minimum so `CoreProviderMetadata::discover_async` succeeds:
  * GET /.well-known/openid-configuration
  * GET /jwks

This is NOT a real OIDC provider — it does not mint tokens. It only exists so
that `pietro serve` can boot and we can hit `/healthz` and unauthenticated
endpoints. For real callback testing, run against Keycloak per pietro.md §19.
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 19000
BASE = f"http://127.0.0.1:{PORT}/"

DISCOVERY = {
    "issuer": BASE,
    "authorization_endpoint": f"{BASE}authorize",
    "token_endpoint": f"{BASE}token",
    "jwks_uri": f"{BASE}jwks",
    "response_types_supported": ["code"],
    "subject_types_supported": ["public"],
    "id_token_signing_alg_values_supported": ["RS256"],
    "scopes_supported": ["openid", "profile", "email"],
}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/.well-known/openid-configuration":
            body = json.dumps(DISCOVERY).encode()
        elif self.path == "/jwks":
            body = json.dumps({"keys": []}).encode()
        else:
            self.send_response(404)
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):  # quiet
        pass


if __name__ == "__main__":
    print(f"fake-idp listening on {BASE}", flush=True)
    HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
