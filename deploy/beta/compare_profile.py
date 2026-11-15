"""Compare the actual authenticated profile response using beta users."""
import json
from http.client import HTTPConnection
import sys

flask = ("127.0.0.1", 5070)
rust = (sys.argv[1], int(sys.argv[2]))


def request(server, path, method="GET", body=None, **headers):
    conn = HTTPConnection(*server, timeout=12)
    headers = {"Host": "beta.wiring.club", "X-Forwarded-Proto": "https", **headers}
    if body is not None:
        headers["Content-Type"] = "application/json"
    try:
        conn.request(method, path, json.dumps(body) if body is not None else None, headers)
        res = conn.getresponse()
        data = res.read()
        if data and res.getheader("Content-Type") == "application/json":
            data = json.loads(data)
        return res.status, res.headers, data
    finally:
        conn.close()


for name in "dev", "peer":
    data = {"email": name + "@wiring.test", "password": "wiring-dev"}
    status, headers, _ = request(flask, "/api/login", "POST", data)
    assert status == 200
    web = headers["Set-Cookie"].split(";", 1)[0]
    status, _, tokens = request(flask, "/api/auth/token", "POST", data)
    assert status == 200
    mobile = "Bearer " + tokens["access_token"]
    for lang in "ru", "en":
        for auth in ({}, {"Cookie": web}, {"Authorization": mobile}, {"Cookie": web, "Authorization": "Bearer invalid"}):
            auth = {**auth, "X-Wiring-Language": lang}
            old, old_headers, old_profile = request(flask, "/api/me", **auth)
            new, headers, profile = request(rust, "/api/me", **auth)
            assert old == new, (name, lang, old, new)
            if old_profile != profile and old == 200 and old_profile["user"] and profile["user"]:
                mismatched = [k for k in old_profile["user"].keys() | profile["user"].keys()
                              if old_profile["user"].get(k) != profile["user"].get(k)]
                raise AssertionError((name, lang, mismatched))
            assert old_profile == profile
            assert bool(headers.get("Set-Cookie")) == bool(old_headers.get("Set-Cookie"))
            assert headers.get("Content-Language") == old_headers.get("Content-Language")
print("Web and native profile payloads matched Flask.")
