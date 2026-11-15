"""Exercise one-time token consumption on beta with synthetic fixtures only."""
from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection
import base64
import hashlib
import json
import os
import secrets
import sys
import time

import psycopg


def request(host, port, path, payload, cookie=None, auth=None, method="POST"):
    headers = {"Host": "beta.wiring.club", "Content-Type": "application/json",
               "X-Wiring-Language": "en", "X-Forwarded-Proto": "https"}
    if cookie:
        headers["Cookie"] = cookie
    if auth:
        headers["Authorization"] = "Bearer " + auth
    conn = HTTPConnection(host, port, timeout=15)
    try:
        conn.request(method, path, json.dumps(payload), headers)
        res = conn.getresponse()
        return res.status, res.headers, json.loads(res.read())
    finally:
        conn.close()


rust = (sys.argv[1], int(sys.argv[2]))
flask = ("127.0.0.1", 5070)
for path, data in (("/api/email/verify", {}), ("/api/password/reset", {}),
                   ("/api/password/reset", {"token": "missing", "password": "test-longer"}),
                   ("/api/auth/native/exchange", {})):
    expected, _, before = request(*flask, path, data)
    status, _, after = request(*rust, path, data)
    assert (status, after) == (expected, before) and status == 400, (path, status)

with psycopg.connect(os.environ["DATABASE_URL"], autocommit=True) as db:
    assert db.info.dbname == "wiring_beta"
    uid, verified, password_hash, last_seen = db.execute(
        "SELECT id,email_verified_at,password_hash,last_seen FROM users WHERE email='peer@wiring.test'"
    ).fetchone()
    token, reset, oauth = (secrets.token_urlsafe(32) for _ in range(3))
    verifier = secrets.token_urlsafe(48)
    oauth_hash = hashlib.sha256(oauth.encode()).hexdigest()
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).decode().rstrip("=")
    now = int(time.time())
    try:
        for mobile in (False, True):
            db.execute("UPDATE users SET email_verified_at=NULL WHERE id=%s", (uid,))
            db.execute("INSERT INTO email_verifications(token,user_id,created_at) VALUES(%s,%s,%s)", (token, uid, now))
            status, headers, result = request(*rust, "/api/email/verify", {"token": token, "mobile": mobile})
            assert status == 200 and result["user"]["id"] == uid, status
            assert request(*rust, "/api/email/verify", {"token": token, "mobile": mobile})[0] == 400
            if mobile:
                assert not headers.get("Set-Cookie") and headers.get("Cache-Control") == "no-store"
                assert request(*flask, "/api/me", {}, auth=result["access_token"], method="GET")[0] == 200
                request(*flask, "/api/auth/logout", {"refresh_token": result["refresh_token"]})
            else:
                cookie = headers["Set-Cookie"].split(";", 1)[0]
                assert request(*flask, "/api/me", {}, cookie=cookie, method="GET")[2]["user"]["id"] == uid
            db.execute("DELETE FROM email_verifications WHERE token=%s", (token,))
        db.execute("INSERT INTO oauth_native_codes(code_hash,user_id,challenge,created_at) VALUES(%s,%s,%s,%s)",
                   (oauth_hash, uid, challenge, now))
        rejected = request(*rust, "/api/auth/native/exchange", {"code": oauth, "code_verifier": "x"*43})
        assert rejected[0] == 400, rejected[0]
        status, headers, result = request(*rust, "/api/auth/native/exchange", {"code": oauth, "code_verifier": verifier})
        assert status == 200 and result["user"]["id"] == uid
        assert headers.get("Cache-Control") == "no-store" and headers.get("Referrer-Policy") == "no-referrer"
        assert request(*rust, "/api/auth/native/exchange", {"code": oauth, "code_verifier": verifier})[0] == 400
        assert request(*flask, "/api/me", {}, auth=result["access_token"], method="GET")[0] == 200
        request(*flask, "/api/auth/logout", {"refresh_token": result["refresh_token"]})

        db.execute("INSERT INTO password_resets(token,user_id,created_at) VALUES(%s,%s,%s)", (reset, uid, now))
        def reset_attempt(_):
            return request(*rust, "/api/password/reset", {"token": reset, "password": "beta-new-password"})[0]
        with ThreadPoolExecutor(2) as pool:
            assert sorted(pool.map(reset_attempt, (1, 2))) == [200, 400]
        assert request(*flask, "/api/login", {"email": "peer@wiring.test", "password": "beta-new-password"})[0] == 200
        assert request(*flask, "/api/me", {}, auth=result["access_token"], method="GET")[0] == 401
    finally:
        db.execute("DELETE FROM oauth_native_codes WHERE code_hash=%s", (oauth_hash,))
        db.execute("DELETE FROM password_resets WHERE token=%s", (reset,))
        db.execute("DELETE FROM email_verifications WHERE token=%s", (token,))
        db.execute("UPDATE users SET email_verified_at=%s,password_hash=%s,last_seen=%s WHERE id=%s",
                   (verified, password_hash, last_seen, uid))
print("Email verification, simultaneous reset requests and native OAuth exchange passed.")
