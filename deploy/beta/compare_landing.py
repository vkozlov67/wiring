"""Check the mobile landing, theme, download links and localized PWA manifest."""
from argparse import ArgumentParser
from http.client import HTTPConnection, HTTPSConnection
import json
import socket

p = ArgumentParser()
p.add_argument("server")
p.add_argument("port", type=int)
p.add_argument("--host", default="beta.wiring.club")
p.add_argument("--tls", action="store_true")
args = p.parse_args()


def fetch(host, port, path, method, theme, use_tls=False):
    conn = HTTPSConnection(args.host, port, timeout=8) if use_tls else HTTPConnection(host, port, timeout=8)
    if use_tls:
        conn._create_connection = lambda address, *params: socket.create_connection((host, address[1]), *params)
    try:
        conn.request(method, path, headers={"Host": args.host, "X-Forwarded-Proto": "https",
            **({"Cookie": "wiring-theme=" + theme} if theme else {})})
        res = conn.getresponse()
        body = res.read().strip()
        if body and res.getheader("Content-Type") == "application/manifest+json":
            body = json.loads(body)
        return res.status, res.headers, body
    finally:
        conn.close()


checks = 0
for path in ("/m", "/public/manifest.webmanifest"):
    for theme in ("", "pastel", "mist", "dusk", "night", "slate", "invalid"):
        for language in ("ru", "en"):
            for method in ("GET", "HEAD", "OPTIONS", "POST"):
                old, headers, expected = fetch("127.0.0.1", 5070, path+"?lang="+language, method, theme)
                code, actual, body = fetch(args.server, args.port, path+"?lang="+language, method, theme, args.tls)
                assert code == old and body == expected, (path, method, language, theme, code, old)
                for name in ("Cache-Control", "Content-Type", "Content-Language"):
                    assert headers.get(name) == actual.get(name), (path, method, name)
                assert bool(headers.get("Set-Cookie")) == bool(actual.get("Set-Cookie")), (path, method, "cookie")
                checks += 1
print(f"{checks} landing and manifest comparisons passed.")
