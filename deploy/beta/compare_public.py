"""Compare the Rust public document contract against Flask before switching the proxy."""
import argparse
from http.client import HTTPConnection
import json
from urllib.parse import urlsplit

EDITORIAL = ("/account-deletion", "/child-safety", "/privacy", "/legal", "/rules", "/marketing", "/glossary")
METADATA = ("/robots.txt", "/sitemap.xml", "/.well-known/apple-app-site-association", "/.well-known/assetlinks.json")


def fetch(origin, host, method, path, extra):
    parsed = urlsplit(origin)
    conn = HTTPConnection(parsed.hostname, parsed.port, timeout=12)
    try:
        conn.request(method, path, headers={"Host": host, "X-Forwarded-Proto": "https", **extra})
        res = conn.getresponse()
        return res.status, {key.lower(): val for key, val in res.getheaders()}, res.read()
    finally:
        conn.close()


def compare(expected, actual, host, browser=False):
    checks = 0
    for path in EDITORIAL + METADATA:
        cases = (
            ("GET", "", {}),
            ("GET", "?lang=en", {}),
            ("GET", "?lang=ru", {"X-Wiring-Language": "en", "Cookie": "wiring-language=ru"}),
            ("GET", "?lang=invalid", {"Cookie": "wiring-language=en"}),
            ("HEAD", "?lang=en", {}),
            ("OPTIONS", "", {}),
            ("POST", "", {}),
        )
        if browser:
            cases += (("GET", "", {"User-Agent": "WIRING-HTTP-contract/1.0"}),)
        for method, suffix, headers in cases:
            old_status, old_headers, old_body = fetch(expected, host, method, path + suffix, headers)
            status, new_headers, body = fetch(actual, host, method, path + suffix, headers)
            case = method, path, suffix
            assert status == old_status, (case, status, old_status)
            for name in ("content-language", "content-type", "allow", "vary", "set-cookie"):
                old, new = old_headers.get(name), new_headers.get(name)
                if name == "set-cookie":
                    old, new = (value.split(";")[0:1] + value.split(";")[2:] if value else value
                                for value in (old, new))
                if name in ("allow", "vary"):
                    old, new = (set(value.lower().replace(" ", "").split(",")) if value else value
                                for value in (old, new))
                assert new == old, (case, name, new, old)
            if method == "GET" and old_headers["content-type"] == "application/json":
                assert json.loads(body) == json.loads(old_body), case
            elif method == "GET" and path in EDITORIAL:
                assert body.strip() == old_body.strip(), case
            else:
                assert body == old_body, case
            checks += 1
    print(f"{checks} HTTP contract comparisons passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("flask")
    parser.add_argument("rust")
    parser.add_argument("--host", required=True)
    parser.add_argument("--browser", action="store_true", help="include a request with a User-Agent on beta")
    args = parser.parse_args()
    compare(args.flask, args.rust, args.host, args.browser)
