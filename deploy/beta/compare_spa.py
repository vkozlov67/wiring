"""Compare web-shell HTML. OAuth-cookie cases use only the closed beta secret."""
from argparse import ArgumentParser
from http.client import HTTPConnection, HTTPSConnection
from http.cookies import SimpleCookie
import os
import socket

PATHS = (
    "/", "/sign-in", "/sign-up", "/login", "/register", "/forgot", "/reset", "/verify",
    "/feed", "/for-you", "/likes", "/chats", "/chats/123", "/me", "/me/edit", "/consents",
    "/plus", "/premium", "/onboard", "/delete-account", "/notifications", "/archive", "/p/123", "/r/beta-smoke",
)

parser = ArgumentParser()
parser.add_argument("server")
parser.add_argument("port", type=int)
parser.add_argument("--beta-cookie", action="store_true")
parser.add_argument("--host", default="beta.wiring.club")
parser.add_argument("--proxy-tls", action="store_true")
args = parser.parse_args()
flask, rust = ("127.0.0.1", 5070), (args.server, args.port)


def fetch(addr, path, method="GET", extra=None):
    headers = {"Host": args.host, "X-Forwarded-Proto": "https", **(extra or {})}
    if args.proxy_tls and addr == rust:
        conn = HTTPSConnection(args.host, addr[1], timeout=8)
        conn._create_connection = lambda destination, *params: socket.create_connection(
            (addr[0], destination[1]), *params,
        )
    else:
        conn = HTTPConnection(*addr, timeout=8)
    try:
        conn.request(method, path, headers=headers)
        res = conn.getresponse()
        return res.status, res.headers, res.read().strip()
    finally:
        conn.close()


for path in PATHS:
    for method, suffix in (("GET", ""), ("GET", "?lang=en"), ("HEAD", ""), ("OPTIONS", ""), ("POST", "")):
        expected, original, old_body = fetch(flask, path + suffix, method)
        status, headers, body = fetch(rust, path + suffix, method)
        assert status == expected, (path, method, status, expected)
        assert body == old_body, (path, method, suffix)
        for header in ("Content-Type", "Cache-Control", "Content-Language"):
            assert headers.get(header) == original.get(header), (path, method, header)
        vary = lambda values: {
            item.strip().lower() for line in values.get_all("Vary", []) for item in line.split(",")
        }
        actual_vary = vary(headers)
        if args.proxy_tls:
            actual_vary.discard("accept-encoding")  # nginx may add this for gzip.
        assert actual_vary == vary(original), (path, method, actual_vary, vary(original))
        assert set((headers.get("Allow") or "").split(", ")) == set((original.get("Allow") or "").split(", "))
print(f"{5 * len(PATHS)} SPA comparisons passed.")

if args.beta_cookie:
    from flask import Flask
    from flask.sessions import SecureCookieSessionInterface
    from itsdangerous import URLSafeTimedSerializer
    app = Flask(__name__)
    app.secret_key = os.environ["APP_SECRET_KEY"]
    signer = SecureCookieSessionInterface().get_signing_serializer(app)
    receipt = URLSafeTimedSerializer(app.secret_key, salt="wiring-registration-v1").dumps(42)
    token = signer.dumps({"_permanent": True, "registration_created": True, "registration_receipt": receipt})
    for address in (flask, rust):
        status, headers, body = fetch(address, "/", extra={
            "Cookie": f"wiring_session={token}", "User-Agent": "WIRING beta SPA contract test",
        })
        assert status == 200
        assert b'data-registration-created="true"' in body and b'data-registration-managed="true"' in body
        assert receipt.encode() not in body and headers.get("Cache-Control") == "no-store"
        cookies = SimpleCookie()
        for value in headers.get_all("Set-Cookie"):
            cookies.load(value)
        assert cookies["wiring-registration-receipt"].value == receipt
        assert cookies["wiring-registration-receipt"]["path"] == "/api/analytics"
        assert signer.loads(cookies["wiring_session"].value) == {"_permanent": True}
        renewed = cookies["wiring_session"].value
        status, _, body = fetch(address, "/", extra={"Cookie": f"wiring_session={renewed}"})
        assert status == 200 and b'data-registration-created="false"' in body and b'data-registration-managed="false"' in body
    print("OAuth registration marker and receipt were preserved and consumed once.")
