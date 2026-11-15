"""Validate Rust login, compatible cookies/tokens, and recovery on closed beta."""
import json
from http.client import HTTPConnection
import os
import sys
import time
import uuid

import psycopg

flask, rust = ("127.0.0.1", 5070), (sys.argv[1], int(sys.argv[2]))
ip = "login-check-" + uuid.uuid4().hex


def request(server, path, method="POST", payload=None, **extra):
    conn = HTTPConnection(*server, timeout=12)
    headers = {"Host": "beta.wiring.club", "X-Forwarded-Proto": "https", "X-Forwarded-For": ip, **extra}
    if payload is not None:
        headers["Content-Type"] = "application/json"
    try:
        conn.request(method, path, json.dumps(payload) if payload is not None else None, headers)
        res = conn.getresponse()
        data = res.read()
        if data and res.getheader("Content-Type") == "application/json":
            data = json.loads(data)
        return res.status, res.headers, data
    finally:
        conn.close()


creds = {"email": " Dev@wiring.test ", "password": "wiring-dev"}
for locale in "en", "ru":
    lang = {"X-Wiring-Language": locale}
    for path in "/api/login", "/api/auth/token":
        for data in ({}, {**creds, "password": "invalid"}):
            f, _, expected = request(flask, path, payload=data, **lang)
            r, headers, got = request(rust, path, payload=data, **lang)
            assert (f, expected) == (r, got) and r == 401, (path, f, r)
            assert headers.get("Content-Language") == locale
        assert request(rust, path, "OPTIONS", **lang)[0] == 200
        assert request(rust, path, "GET", **lang)[0] == 405
    old_status, _, old_web = request(flask, "/api/login", payload=creds, **lang)
    status, headers, web = request(rust, "/api/login", payload=creds, **lang)
    assert status == old_status == 200 and old_web == web, (old_status, status)
    assert all(flag in headers["Set-Cookie"] for flag in ("Secure", "HttpOnly", "SameSite=Lax"))
    cookie = headers["Set-Cookie"].split(";", 1)[0]
    assert request(flask, "/api/me", "GET", Cookie=cookie, **lang)[2]["user"]["id"] == web["user"]["id"]
    status, headers, mobile = request(rust, "/api/auth/token", payload={**creds, "device": "beta Rust login"},
                                      Authorization="Bearer invalid", **lang)
    assert status == 200 and mobile["expires_in"] == 3600 and not headers.get("Set-Cookie"), status
    assert mobile["user"] == web["user"]
    assert request(flask, "/api/me", "GET", Authorization="Bearer "+mobile["access_token"], **lang)[0] == 200
    status, _, rotated = request(flask, "/api/auth/refresh", payload={"refresh_token": mobile["refresh_token"]}, **lang)
    assert status == 200 and rotated["refresh_token"] != mobile["refresh_token"]
    request(flask, "/api/auth/logout", payload={"refresh_token": rotated["refresh_token"]})

with psycopg.connect(os.environ["DATABASE_URL"], autocommit=True) as db:
    assert db.info.dbname == "wiring_beta"
    uid, deleted, verified = db.execute(
        "SELECT id, deleted_at, email_verified_at FROM users WHERE email = 'dev@wiring.test'"
    ).fetchone()
    try:
        db.execute("UPDATE users SET email_verified_at = NULL WHERE id = %s", (uid,))
        status, _, response = request(rust, "/api/login", payload=creds)
        assert status == 403 and response["needs_email_verify"] and response["email"] == "dev@wiring.test"
        db.execute("UPDATE users SET email_verified_at = %s, deleted_at = %s WHERE id = %s",
                   (verified, int(time.time()) - 8*86400, uid))
        status, _, response = request(rust, "/api/login", payload=creds)
        assert status == 403 and response["deleted"]
        db.execute("UPDATE users SET deleted_at = %s WHERE id = %s", (int(time.time()) - 3600, uid))
        status, _, response = request(rust, "/api/login", payload={**creds, "password": "invalid"})
        assert status == 401 and not response.get("restored")
        assert db.execute("SELECT deleted_at FROM users WHERE id = %s", (uid,)).fetchone()[0] is not None
        status, _, response = request(rust, "/api/login", payload=creds)
        assert status == 200 and response["restored"]
        assert db.execute("SELECT deleted_at FROM users WHERE id = %s", (uid,)).fetchone()[0] is None
    finally:
        db.execute("UPDATE users SET deleted_at = %s, email_verified_at = %s WHERE id = %s", (deleted, verified, uid))
print("Web and mobile login, language, verification, recovery and session compatibility passed.")
