from http.client import HTTPConnection
import json

def fetch(server, port, path, method="GET", data=None, **extra):
    conn = HTTPConnection(server, port, timeout=8)
    try:
        headers = {"Host": "beta.wiring.club", "X-Wiring-Language": "en", "X-Forwarded-Proto": "https", **extra}
        if data is not None:
            headers["Content-Type"] = "application/json"
        conn.request(method, path, None if data is None else json.dumps(data), headers)
        response = conn.getresponse()
        body = response.read()
        if response.getheader("Content-Type") == "application/json" and body:
            body = json.loads(body)
        return response.status, response.headers, body
    finally:
        conn.close()

rust = ("places", 5082)
flask = ("127.0.0.1", 5070)
assert fetch(*rust, "/auth/refresh", "POST", {})[0] == 404
paths = "/api/auth/refresh", "/api/auth/logout"
for path in paths:
    for method, data in (("POST", {}), ("HEAD", None), ("OPTIONS", None), ("GET", None)):
        before, old_headers, previous = fetch(*flask, path, method, data)
        after, new_headers, current = fetch(*rust, path, method, data)
        assert (before, previous) == (after, current), (path, method, before, after)
        for name in ("Content-Language", "Content-Type", "Vary"):
            assert old_headers.get(name) == new_headers.get(name), (path, method, name)
        assert set((old_headers.get("Allow") or "").split(", ")) == set((new_headers.get("Allow") or "").split(", "))
    status, _, body = fetch(*rust, path, "POST", {}, **{"X-App-Version": "1.1"})
    assert status == 426 and body["upgrade"] and body["min_version"] == "1.2.0", (path, status)
    status, _, _ = fetch(*rust, path, "POST", {}, **{"X-App-Version": "1.2.0", "Authorization": "Bearer invalid"})
    assert status == (401 if path.endswith("refresh") else 200), (path, status)

status, _, token = fetch(*flask, "/api/auth/token", "POST", {"email": "dev@wiring.test", "password": "wiring-dev"})
assert status == 200
status, _, rotated = fetch(*rust, paths[0], "POST", {"refresh_token": token["refresh_token"]})
assert status == 200 and token["refresh_token"] != rotated["refresh_token"]
assert fetch(*flask, "/api/me", Authorization="Bearer "+rotated["access_token"])[0] == 200
status, _, rotated_again = fetch(*flask, paths[0], "POST", {"refresh_token": rotated["refresh_token"]})
assert status == 200
assert fetch(*rust, paths[1], "POST", {"refresh_token": rotated_again["refresh_token"]})[0] == 200
assert fetch(*flask, paths[0], "POST", {"refresh_token": rotated_again["refresh_token"]})[0] == 401
print("Mobile HTTP contract, version gate and Flask↔Rust token rotation passed.")
