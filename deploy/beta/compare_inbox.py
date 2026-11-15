"""Compare authenticated inbox requests with Flask using only beta accounts."""
from http.client import HTTPConnection
import json

flask = ("127.0.0.1", 5070)
rust = ("places", 5082)


def fetch(address, path, method="GET", data=None, **extra):
    conn = HTTPConnection(*address, timeout=10)
    try:
        headers = {"Host": "beta.wiring.club", "X-Wiring-Language": "en", "X-Forwarded-Proto": "https", **extra}
        if data is not None:
            headers["Content-Type"] = "application/json"
        conn.request(method, path, None if data is None else json.dumps(data), headers)
        response = conn.getresponse()
        body = response.read()
        if body and response.getheader("Content-Type") == "application/json":
            body = json.loads(body)
        return response.status, response.headers, body
    finally:
        conn.close()


def compare(path, method="GET", data=None, **headers):
    old_status, old_headers, old_body = fetch(flask, path, method, data, **headers)
    status, new_headers, body = fetch(rust, path, method, data, **headers)
    assert (old_status, old_body) == (status, body), (path, method, old_status, status)
    for header in "Content-Type", "Content-Language", "Vary":
        assert old_headers.get(header) == new_headers.get(header), (path, method, header)
    assert set((old_headers.get("Allow") or "").split(", ")) == set((new_headers.get("Allow") or "").split(", "))
    assert bool(old_headers.get("Set-Cookie")) == bool(new_headers.get("Set-Cookie")), (path, method)
    return status, new_headers, body


creds = {"email": "dev@wiring.test", "password": "wiring-dev"}
status, headers, _ = fetch(flask, "/api/login", "POST", creds)
assert status == 200
cookie = headers["Set-Cookie"].split(";", 1)[0]
status, _, native = fetch(flask, "/api/auth/token", "POST", creds)
assert status == 200
bearer = "Bearer " + native["access_token"]
assert compare("/api/inbox")[0] == 401
assert compare("/api/inbox", Cookie=cookie + "invalid")[0] == 401
assert compare("/api/inbox", Cookie=cookie, Authorization="Bearer invalid")[0] == 401
for language in ("en", "ru"):
    language = {"X-Wiring-Language": language}
    assert compare("/api/inbox", Authorization=bearer, **language)[0] == 200
    assert compare("/api/inbox", "HEAD", Authorization=bearer, **language)[0] == 200
    status, renewed, _ = compare("/api/inbox", Cookie=cookie, **language)
    assert status == 200
    renewed_cookie = renewed["Set-Cookie"].split(";", 1)[0]
    assert fetch(flask, "/api/me", Cookie=renewed_cookie)[0] == 200
    assert compare("/api/notices/read", "POST", {"ids": [-1, "bad"]}, Authorization=bearer, **language)[0] == 200
    assert fetch(rust, "/api/notices/read", "POST", {"ids": ["99999999999999999999999"]}, Authorization=bearer, **language)[0] == 200
    assert compare("/api/notices/read", "POST", {"ids": [-1]}, Cookie=cookie, **language)[0] == 200
assert compare("/api/inbox", "OPTIONS")[0] == 200
assert compare("/api/notices/read", "GET")[0] == 405
assert fetch(rust, "/api/inbox", **{"X-App-Version": "1.1"})[0] == 426
assert fetch(rust, "/chat/inbox", "POST", {})[0] == 404
print("Flask and Rust inbox matched for web cookies, bearer, anonymous and invalid sessions.")
