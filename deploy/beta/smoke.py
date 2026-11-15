"""Verify the isolated beta and capture API responses for Flask/Rust comparison."""

import argparse
from http.cookiejar import CookieJar, DefaultCookiePolicy
import json
from pathlib import Path
from urllib.error import HTTPError
from urllib.parse import urlencode, urlsplit
from urllib.request import HTTPCookieProcessor, HTTPRedirectHandler, ProxyHandler, Request, build_opener


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("origin")
    parser.add_argument("--backend", choices=["flask", "rust"], required=True)
    parser.add_argument("--loopback-proxy", action="store_true", help="exercise the private Flask port before DNS is ready")
    parser.add_argument("--write", type=Path)
    parser.add_argument("--compare", type=Path)
    args = parser.parse_args()
    origin = urlsplit(args.origin)
    if not (
        origin.scheme == "https" and origin.netloc == "beta.wiring.club"
        or origin.scheme == "http" and origin.hostname == "localhost" and origin.netloc == f"localhost:{origin.port}"
    ) or origin.path not in ("", "/") or origin.query or origin.fragment:
        parser.error("use https://beta.wiring.club or a local beta at http://localhost:<port>")

    if args.loopback_proxy and origin.scheme != "http":
        parser.error("--loopback-proxy is only for the private HTTP port on localhost")
    cookie_policy = DefaultCookiePolicy(secure_protocols=("https", "wss", "http")) if args.loopback_proxy else None
    browser = build_opener(ProxyHandler({}), NoRedirect(), HTTPCookieProcessor(CookieJar(policy=cookie_policy)))
    native = build_opener(ProxyHandler({}), NoRedirect())
    base = args.origin.rstrip("/")

    def call(client, path, *, data=None, headers=None, method=None, status=200):
        request = Request(base + path, method=method, headers=headers or {})
        if args.loopback_proxy:
            request.add_header("Host", "beta.wiring.club")
            request.add_header("X-Forwarded-Proto", "https")
        if data is not None:
            request.add_header("Content-Type", "application/json")
            request.data = json.dumps(data).encode()
        try:
            response = client.open(request, timeout=12)
        except HTTPError as error:
            response = error
        with response:
            assert response.status == status, (path, response.status, status)
            payload = response.read()
            if payload and response.headers.get_content_type() == "application/json":
                payload = json.loads(payload)
            return response.headers, payload

    shell, body = call(browser, "/login")
    assert b"/public/dist/app.js" in body
    if not args.loopback_proxy:
        assert shell["X-Robots-Tag"] == "noindex, nofollow"
    call(browser, "/public/dist/app.js")
    _, unauthorized = call(native, "/api/places", status=401)
    _, bad_bearer = call(native, "/api/places", headers={"Authorization": "Bearer invalid"}, status=401)
    assert bad_bearer.get("token_expired")
    credentials = {"email": "dev@wiring.test", "password": "wiring-dev"}
    web_login, profile = call(browser, "/api/login", data=credentials)
    assert profile["ok"] and web_login["Set-Cookie"].startswith("wiring_session=")
    assert ("Secure" in web_login["Set-Cookie"]) == (origin.scheme == "https" or args.loopback_proxy)
    _, peer = call(native, "/api/login", data={**credentials, "email": "peer@wiring.test"})
    _, thread = call(browser, f"/api/messages/{peer['user']['id']}")
    assert thread["ok"] and thread["messages"]

    token_headers, token = call(native, "/api/auth/token", data={**credentials, "device": "beta smoke"})
    assert token["ok"] and "Set-Cookie" not in token_headers
    bearer = {"Authorization": f"Bearer {token['access_token']}"}

    cases = [
        {}, {"q": "  São  "}, {"q": "Sao Paulo"}, {"q": "белград"}, {"q": "Beograd"},
        {"q": "東京"}, {"q": "İstanbul"}, {"q": "Нови сад"}, {"q": "Engels"},
        {"q": "новый йорк"}, {"q": "ß"}, {"q": "!!!"},
        {"country": "Бразилия"}, {"q": "par", "country": "Франция"},
        {"q": "par", "country": "unknown"}, {"q": "no_such_city_123"},
        {"countries": "1"}, {"countries": "1", "q": "uk"}, {"countries": "1", "q": "United"},
        {"countries": "1", "q": "россия", "country": "Сербия"},
    ]

    captured = {"anonymous": unauthorized, "invalid_bearer": bad_bearer, "queries": []}
    for language in ("ru", "en"):
        for query in cases:
            path = "/api/places?" + urlencode(query)
            headers = {"X-Wiring-Language": language}
            web_headers, web_result = call(browser, path, headers=headers)
            mobile_headers, mobile_result = call(native, path, headers={**bearer, **headers})
            assert web_result == mobile_result and web_result["ok"], (language, query)
            assert len(web_result["places"]) <= 60
            assert "Set-Cookie" not in mobile_headers
            for response_headers in (web_headers, mobile_headers):
                assert response_headers["Content-Language"] == language
                assert response_headers["X-Wiring-Beta-Places"] == ("rust" if args.backend == "rust" else None)
            captured["queries"].append({"language": language, "query": query, "response": web_result})

    head, _ = call(browser, "/api/places", method="HEAD")
    assert "X-Wiring-Beta-Places" not in head
    call(browser, "/api/places", method="POST", status=405)
    resolve, _ = call(browser, "/api/places/resolve", data={}, status=400)
    assert "X-Wiring-Beta-Places" not in resolve

    if args.compare:
        expected = json.loads(args.compare.read_text())
        assert len(captured["queries"]) == len(expected["queries"])
        for actual, baseline in zip(captured["queries"], expected["queries"]):
            assert actual == baseline, (actual["language"], actual["query"])
        assert captured == expected
    if args.write:
        args.write.write_text(json.dumps(captured, ensure_ascii=False, indent=2) + "\n")

    print(f"{args.backend}: login, chat, and {len(cases) * 4} city searches via cookie/Bearer passed")


if __name__ == "__main__":
    main()
