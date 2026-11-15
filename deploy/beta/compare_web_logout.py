"""Check Flask-compatible web session deletion using the closed beta."""
from argparse import ArgumentParser
from http.client import HTTPConnection, HTTPSConnection
from http.cookies import SimpleCookie
import json
import socket

p = ArgumentParser()
p.add_argument("server")
p.add_argument("port", type=int)
p.add_argument("--host", default="beta.wiring.club")
p.add_argument("--tls", action="store_true")
p.add_argument("--beta", action="store_true")
args = p.parse_args()
flask = ("127.0.0.1", 5070)
rust = (args.server, args.port)


def fetch(address, path, method, headers, data=None):
    conn = HTTPSConnection(args.host, address[1], timeout=8) if args.tls and address == rust else HTTPConnection(*address, timeout=8)
    if args.tls and address == rust:
        conn._create_connection = lambda remote, *params: socket.create_connection((address[0], remote[1]), *params)
    try:
        conn.request(method, path, data, {"Host": args.host, "X-Forwarded-Proto": "https", **headers})
        res = conn.getresponse()
        body = res.read()
        if body and res.getheader("Content-Type") == "application/json":
            body = json.loads(body)
        cookies = SimpleCookie()
        for item in res.headers.get_all("Set-Cookie", []):
            cookies.load(item)
        return res.status, body, cookies
    finally:
        conn.close()


for lang in ("ru", "en"):
    for method, status in (("POST", 200), ("OPTIONS", 200), ("GET", 405)):
        for headers in ({}, {"Authorization": "Bearer invalid"}):
            headers = {"X-Wiring-Language": lang, **headers}
            old, old_body, old_cookie = fetch(flask, "/api/logout", method, headers)
            new, new_body, new_cookie = fetch(rust, "/api/logout", method, headers)
            assert (old, old_body) == (new, new_body), (method, old, new)
            assert old_cookie == new_cookie, (method, list(old_cookie), list(new_cookie))
            assert new == (401 if "Authorization" in headers else status)
if args.beta:
    status, _, login = fetch(flask, "/api/login", "POST", {"Content-Type": "application/json"},
                             json.dumps({"email": "dev@wiring.test", "password": "wiring-dev"}))
    assert status == 200
    cookie = login["wiring_session"].value
    status, payload, cookies = fetch(rust, "/api/logout", "POST", {"Cookie": "wiring_session="+cookie})
    assert status == 200 and payload == {"ok": True}
    assert cookies["wiring_session"].value == "" and cookies["wiring_session"]["max-age"] == "0"
    assert cookies["wiring_session"]["secure"] and cookies["wiring_session"]["httponly"]
    status, inbox, _ = fetch(flask, "/api/inbox", "GET", {"Cookie": "wiring_session="})
    assert status == 401 and not inbox["ok"]
print("Flask and Rust web logout matched.")
