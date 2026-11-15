"""Compare registration/removal of synthetic push devices on closed beta."""
import json
from http.client import HTTPConnection
import os
import sys
import uuid
import psycopg

flask = ("127.0.0.1", 5070)
rust = (sys.argv[1], int(sys.argv[2]))
path = "/api/push/device"


def fetch(addr, method, path, payload=None, bearer=None, language="en"):
    headers = {"Host": "beta.wiring.club", "X-Wiring-Language": language, "X-Forwarded-Proto": "https"}
    if payload is not None:
        headers["Content-Type"] = "application/json"
    if bearer:
        headers["Authorization"] = "Bearer " + bearer
    conn = HTTPConnection(*addr, timeout=10)
    try:
        conn.request(method, path, None if payload is None else json.dumps(payload), headers)
        res = conn.getresponse()
        body = res.read()
        if res.getheader("Content-Type") == "application/json" and body:
            body = json.loads(body)
        return res.status, res.headers, body
    finally:
        conn.close()


def compare(method, payload, bearer=None, language="en"):
    original, old, expected = fetch(flask, method, path, payload, bearer, language)
    status, new, body = fetch(rust, method, path, payload, bearer, language)
    assert (original, expected) == (status, body), (method, original, status)
    for name in "Content-Type", "Vary", "Content-Language", "Set-Cookie":
        assert old.get(name) == new.get(name), (method, name)
    assert set((old.get("Allow") or "").split(", ")) == set((new.get("Allow") or "").split(", "))
    return status


sessions = []
for name in "dev", "peer":
    status, _, token = fetch(flask, "POST", "/api/auth/token", {"email": name+"@wiring.test", "password": "wiring-dev"})
    assert status == 200
    sessions.append((token["access_token"], token["user"]["id"]))

token = "ExpoPushToken[" + uuid.uuid4().hex + "]"
for language in "ru", "en":
    assert compare("POST", {}, language=language) == 401
    assert compare("DELETE", {}, "invalid", language) == 401
    assert compare("POST", {"token": "invalid", "platform": "ios"}, sessions[0][0], language) == 400
    assert compare("POST", {"token": token, "platform": "windows"}, sessions[0][0], language) == 400
assert compare("OPTIONS", None) == 200
assert compare("GET", None) == 405
try:
    with psycopg.connect(os.environ["DATABASE_URL"], autocommit=True) as db:
        assert db.info.dbname == "wiring_beta"
        uid = sessions[0][1]
        settings = db.execute(
            "SELECT notify_enabled, notify_push FROM users WHERE id = %s AND email = 'dev@wiring.test'", (uid,),
        ).fetchone()
        assert settings is not None

        def stored():
            return db.execute("SELECT user_id, platform FROM mobile_devices WHERE token = %s", (token,)).fetchone()

        try:
            db.execute("UPDATE users SET notify_enabled = 1, notify_push = 0 WHERE id = %s", (uid,))
            assert compare("POST", {"token": token, "platform": "ios"}, sessions[0][0]) == 403
            db.execute("UPDATE users SET notify_push = 1 WHERE id = %s", (uid,))
            status, _, body = fetch(rust, "POST", path, {"token": token, "platform": "ios"}, sessions[0][0])
            assert status == 200, (status, body)
            assert stored() == (uid, "ios")
            assert fetch(rust, "POST", path, {"token": token, "platform": "android"}, sessions[0][0])[0] == 200
            assert stored() == (uid, "android")
            assert fetch(rust, "DELETE", path, {"token": token}, sessions[1][0])[0] == 200
            assert stored() == (uid, "android")
            assert fetch(rust, "DELETE", path, {"token": token}, sessions[0][0])[0] == 200
            assert stored() is None
        finally:
            db.execute("UPDATE users SET notify_enabled = %s, notify_push = %s WHERE id = %s", (*settings, uid))
finally:
    fetch(flask, "DELETE", path, {"token": token}, sessions[0][0])
print("Flask and Rust push device contracts passed with synthetic tokens.")
