"""Compare health, catalog and mobile configuration against the Flask routes."""
from argparse import ArgumentParser
from http.client import HTTPConnection, HTTPSConnection
import json
import socket

p = ArgumentParser()
p.add_argument("server")
p.add_argument("port", type=int)
p.add_argument("--host", default="beta.wiring.club")
p.add_argument("--tls", action="store_true")
p.add_argument("--beta", action="store_true")
args = p.parse_args()


def fetch(host, port, path, method="GET", extra=None, payload=None, tls=False):
    if tls:
        conn = HTTPSConnection(args.host, port, timeout=10)
        conn._create_connection = lambda address, *params: socket.create_connection((host, address[1]), *params)
    else:
        conn = HTTPConnection(host, port, timeout=10)
    headers = {"Host": args.host, "X-Forwarded-Proto": "https", **(extra or {})}
    try:
        conn.request(method, path, body=payload, headers=headers)
        res = conn.getresponse()
        body = res.read()
        if body and res.getheader("Content-Type") == "application/json":
            body = json.loads(body)
        return res.status, res.headers, body
    finally:
        conn.close()


def compare(path, method, headers=None):
    expected, old, old_body = fetch("127.0.0.1", 5070, path, method, headers)
    status, new, body = fetch(args.server, args.port, path, method, headers, tls=args.tls)
    assert (status, body) == (expected, old_body), (path, method, status, expected)
    for field in ("Content-Type", "Content-Language"):
        assert old.get(field) == new.get(field), (path, method, field)
    allow = lambda h: set((h.get("Allow") or "").split(", "))
    vary = lambda h: {part.strip().lower() for field in h.get_all("Vary", []) for part in field.split(",")}
    new_vary = vary(new)
    if args.tls:
        new_vary.discard("accept-encoding")
    assert allow(old) == allow(new) and vary(old) == new_vary, (path, method, "allow/vary")
    assert bool(old.get("Set-Cookie")) == bool(new.get("Set-Cookie")), (path, method, "cookie")


paths = ("/health", "/api/catalog", "/api/app-config")
for path in paths:
    for method, suffix, headers in (
        ("GET", "", {}),
        ("GET", "?lang=en", {}),
        ("GET", "?lang=ru", {"X-Wiring-Language": "en"}),
        ("HEAD", "", {}),
        ("OPTIONS", "", {}),
        ("POST", "", {}),
        ("GET", "", {"Authorization": "Bearer invalid"}),
    ):
        compare(path, method, headers)
if args.beta:
    status, _, session = fetch("127.0.0.1", 5070, "/api/auth/token", "POST",
                               {"Content-Type": "application/json"},
                               json.dumps({"email": "dev@wiring.test", "password": "wiring-dev"}))
    assert status == 200
    for path in paths[1:]:
        compare(path, "GET", {"Authorization": f"Bearer {session['access_token']}", "User-Agent": "WIRING beta service-info"})
print("Health, catalog and app-config HTTP contracts passed.")
