"""Compare public social routes against Flask, using a disposable local PostgreSQL database only."""
import json
import os
import sys
from http.client import HTTPConnection
from pathlib import Path
from urllib.parse import urlencode, urlsplit

import psycopg

flask_port, rust_port = map(int, sys.argv[1:3])
database = os.environ["DATABASE_URL"]
assert urlsplit(database).hostname == "127.0.0.1" and urlsplit(database).path == "/wiring_rust_test"
stage = sys.argv[3] if len(sys.argv) > 3 else "basic"
stages = ("basic", "albums", "places", "premium", "notifications", "history", "archive", "browser-push", "rewind", "place-validation", "photo-metadata", "discovery", "profile-edit", "message-send", "swipe", "recommendations", "purge", "media")


def through(check):
    return stages.index(stage) >= stages.index(check)


def request(port, path, method="GET", data=None, **headers):
    conn = HTTPConnection("127.0.0.1", port, timeout=12)
    headers = {"Host": "localhost", "X-Forwarded-Proto": "https", **headers}
    if data is not None:
        headers["Content-Type"] = "application/json"
    try:
        conn.request(method, path, None if data is None else json.dumps(data), headers)
        response = conn.getresponse()
        return response.status, response.headers, json.loads(response.read())
    finally:
        conn.close()


def session(email):
    credentials = {"email": email, "password": "wiring-dev"}
    status, headers, data = request(flask_port, "/api/login", "POST", credentials)
    assert status == 200 and data["ok"]
    status, _, data = request(flask_port, "/api/auth/token", "POST", credentials)
    assert status == 200 and data["ok"]
    return {"Cookie": headers["Set-Cookie"].split(";", 1)[0]}, {"Authorization": "Bearer " + data["access_token"]}


def compare(path, auth, method="GET", data=None, expected=200):
    auth = {"X-Wiring-Language": "en", **auth}
    old, _, before = request(flask_port, path, method, data, **auth)
    new, _, after = request(rust_port, path, method, data, **auth)
    assert old == new == expected, (path, old, new, expected)
    assert before == after, (path, before, after)
    return after


web, bearer = session("dev@wiring.test")
_, peer = session("peer@wiring.test")
with psycopg.connect(database) as connection:
    uid = connection.execute("SELECT id FROM users WHERE email='dev@wiring.test'").fetchone()[0]
    other = connection.execute("SELECT id FROM users WHERE email='peer@wiring.test'").fetchone()[0]
    connection.execute("""INSERT INTO support_replies(source_kind,source_id,user_id,body,request_id,created_at) VALUES
        ('ticket',1,%s,'first synthetic reply','rust-acceptance-a',101),
        ('report',2,%s,'second synthetic reply','rust-acceptance-b',102),
        ('ticket',3,%s,'other account reply','rust-acceptance-c',103)""", (uid, uid, other))
    message = connection.execute("INSERT INTO messages(from_id,to_id,body,created_at) VALUES (%s,%s,'test message',123) RETURNING id", (uid, other)).fetchone()[0]
    generation = connection.execute("SELECT feed_generation FROM users WHERE id=%s", (uid,)).fetchone()[0]
    connection.execute("""INSERT INTO feed_history(user_id,other_id,delivered_at,excluded_at) VALUES
        (%s,%s,123,124) ON CONFLICT(user_id,other_id) DO UPDATE SET excluded_at=124""", (uid, other))

for auth in (web, bearer):
    replies = compare("/api/support/replies", auth)["replies"]
    assert [reply["body"] for reply in replies] == ["second synthetic reply", "first synthetic reply"]
    person = compare(f"/api/people/{other}", auth)["person"]
    assert person["matched"] and "email" not in person and "notices" not in person
    assert compare(f"/api/people/{uid}", auth)["person"]["email"] == "dev@wiring.test"
    matches = compare("/api/matches", auth)["matches"]
    assert any(match["id"] == other for match in matches)
    assert all("email" not in match and "albums" not in match for match in matches)

compare(f"/api/messages/{message}", peer, "PATCH", {"body": "must not replace"}, 404)
compare(f"/api/messages/{message}", web, "PATCH", {"body": "  edited message  "})
compare(f"/api/messages/{message}/reaction", peer, "PUT", {"emoji": "👍"})
compare(f"/api/messages/{message}/reaction", peer, "PUT", {"emoji": None})
compare(f"/api/messages/{message}/reaction", peer, "PUT", {"emoji": "no"}, 400)
compare("/api/feed/view", bearer, "POST", {"target_id": other})
compare("/api/feed/view", bearer, "POST", {"target_id": 0}, 404)
compare("/api/deck/restart", bearer, "POST", {}, 410)
status, _, reset = request(rust_port, "/api/feed/reset", "POST", {"generation": generation}, **bearer)
assert status == 200 and reset == {"ok": True, "reset": True, "generation": generation + 1}
compare("/api/feed/reset", bearer, "POST", {"generation": generation})

if through("message-send"):
    compare("/api/messages", bearer, "POST", {"to_id":0,"body":"fixture"}, 403)
    compare("/api/messages", bearer, "POST", {"to_id":other,"body":"  "}, 400)
    compare("/api/messages", bearer, "POST", {"to_id":other,"body":"fixture","reply_to_id":"invalid"}, 400)
    payload={"to_id":other,"body":"text sent by Rust","reply_to_id":message,"client_id":"fixture-send-once"}
    status,_,sent=request(rust_port,"/api/messages","POST",payload,**bearer)
    assert status==200 and sent["ok"] and "duplicate" not in sent
    assert compare("/api/messages",bearer,"POST",payload)=={"ok":True,"id":sent["id"],"duplicate":True}
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT body,reply_to_id FROM messages WHERE from_id=%s AND client_id='fixture-send-once'",(uid,)).fetchall()==[(payload["body"],message)]

if through("swipe"):
    compare("/api/swipe", bearer, "POST", {"target_id":uid,"direction":"like"}, 400)
    compare("/api/swipe", bearer, "POST", {"target_id":other,"direction":"pass"}, 409)
    compare("/api/swipe", peer, "POST", {"target_id":uid,"direction":"snooze"}, 403)
    with psycopg.connect(database) as connection:
        selected=connection.execute("SELECT id FROM users WHERE id NOT IN (%s,%s) ORDER BY id DESC LIMIT 1",(uid,other)).fetchone()[0]
        connection.execute("DELETE FROM swipes WHERE from_id=%s AND to_id=%s",(selected,uid))
    status,_,swiped=request(rust_port,"/api/swipe","POST",{"target_id":selected,"direction":"pass"},**bearer)
    assert status==200 and not swiped["matched"]
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT excluded_at FROM feed_history WHERE user_id=%s AND other_id=%s",(uid,selected)).fetchone()[0]

if through("albums"):
    albums = compare("/api/albums", web)["albums"]
    assert albums and albums[0]["photos"]
    first = albums[0]["id"]
    compare("/api/albums", web, "POST", {}, 400)
    compare(f"/api/albums/{first}", web, "PATCH", {"title": ""}, 400)
    compare(f"/api/albums/{first}", peer, "PATCH", {"title": "not mine"}, 404)
    compare(f"/api/albums/{first}", web, "PATCH", {"title": "new title"})
    status, _, data = request(rust_port, f"/api/albums/{first}", "DELETE", **web)
    assert status == 200 and all(album["id"] != first for album in data["albums"])
    assert sum(len(album["photos"]) for album in data["albums"]) == sum(len(album["photos"]) for album in albums)
    compare("/api/albums", web)
    compare("/api/onboard/skip", bearer, "POST", {})

if through("places"):
    for language in ("ru", "en"):
        auth = {**bearer, "X-Wiring-Language": language}
        for query in ({"q": "Podgorica"}, {"q": "Moscow"}, {"q": "мск"}, {"q": "belgrade"},
                      {"q": "São Paulo"}, {"q": "Белград", "country": "Сербия"},
                      {"q": "Montenegro", "countries": "1"}, {"countries": "1"}, {"q": "!@#"}):
            compare("/api/places?" + urlencode(query), auth)

if through("premium"):
    compare("/api/plus", bearer, "PATCH", {"paused": True, "incognito": True})
    compare("/api/plus", peer, "PATCH", {"paused": True}, 403)
    compare("/api/premium/redeem", bearer, "POST", {"code": "DOES-NOT-EXIST"}, 400)
    with psycopg.connect(database) as connection:
        connection.execute("INSERT INTO promo_codes(code,days,max_uses,uses,benefit) VALUES ('RUST-FIXTURE',3,1,0,'plus')")
        before = connection.execute("SELECT premium_until FROM users WHERE id=%s", (uid,)).fetchone()[0]
    status, _, data = request(rust_port, "/api/premium/redeem", "POST", {"code": "rust-fixture"}, **bearer)
    assert status == 200 and data["plus_until"] == before + 3 * 86400
    compare("/api/premium/redeem", peer, "POST", {"code": "rust-fixture"}, 400)
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT uses FROM promo_codes WHERE code='RUST-FIXTURE'").fetchone()[0] == 1
        assert connection.execute("SELECT user_id FROM promo_redemptions WHERE code='RUST-FIXTURE'").fetchone()[0] == uid

if through("notifications"):
    with psycopg.connect(database) as connection:
        connection.execute("""INSERT INTO push_subscriptions(endpoint,user_id,p256dh,auth,created_at) VALUES
            ('https://fixture.invalid/dev',%s,'fake','fake',0),('https://fixture.invalid/peer',%s,'fake','fake',0)""", (uid, other))
        connection.execute("""INSERT INTO mobile_devices(token,user_id,platform,created_at,updated_at) VALUES
            ('ExpoPushToken[fixture-dev]',%s,'ios',0,0),('ExpoPushToken[fixture-peer]',%s,'ios',0,0)""", (uid, other))
    status, _, data = request(rust_port, "/api/notifications", "PATCH", {"enabled": False}, **bearer)
    assert status == 200 and not data["user"]["notify_enabled"] and not data["user"]["notify_push"]
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT user_id FROM push_subscriptions").fetchall() == [(other,)]
        assert connection.execute("SELECT user_id FROM mobile_devices").fetchall() == [(other,)]
    assert not compare("/api/notifications", bearer, "PATCH", {"enabled": True})["user"]["notify_push"]
    assert compare("/api/notifications", bearer, "PATCH", {"push": True})["user"]["notify_push"]

if through("history"):
    history = compare(f"/api/messages/{other}?limit=1", web)
    assert len(history["messages"]) == 1 and history["has_more"] and "email" not in history["peer"]
    compare(f"/api/messages/{other}?before={history['messages'][0]['id']}&limit=2", bearer)
    compare(f"/api/messages/{other}?after={message}&limit=1", bearer)
    compare("/api/messages/0", bearer, expected=403)
    with psycopg.connect(database) as connection:
        connection.execute("UPDATE messages SET deleted_at=123 WHERE (from_id=%s AND to_id=%s) OR (from_id=%s AND to_id=%s)", (uid,other,other,uid))
        connection.execute("DELETE FROM chat_openers WHERE user_id=%s AND other_id=%s", (uid,other))
    for language in ("en", "ru"):
        auth = {**bearer, "X-Wiring-Language": language}
        status, _, result = request(rust_port, f"/api/messages/{other}", **auth)
        assert status == 200 and len(result["openers"]) == 3 and not result["messages"]
        compare(f"/api/messages/{other}", auth)

if through("archive"):
    compare("/api/archive", bearer)
    compare("/api/archive", bearer, "POST", {"action":"unlike","user_id":other}, 409)
    with psycopg.connect(database) as connection:
        connection.execute("UPDATE messages SET deleted_at=NULL WHERE id=%s", (message,))
    status, _, archive = request(rust_port, "/api/archive", "POST", {"action":"unmatch","user_id":other}, **bearer)
    assert status == 200 and any(person["id"] == other for person in archive["passes"])
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT deleted_at FROM messages WHERE id=%s", (message,)).fetchone()[0]
    compare("/api/archive", web)

if through("browser-push"):
    status, _, data = request(rust_port, "/api/push/public-key", **web)
    assert status == 200 and len(data["publicKey"]) == 87
    assert compare("/api/push/public-key", bearer) == data
    from cryptography.hazmat.primitives import serialization
    from base64 import urlsafe_b64decode
    with psycopg.connect(database) as connection:
        pem = connection.execute("SELECT private_pem FROM push_vapid WHERE id=1").fetchone()[0]
        key = serialization.load_pem_private_key(pem.encode(), password=None).public_key()
        assert key.public_bytes(serialization.Encoding.X962,serialization.PublicFormat.UncompressedPoint) == urlsafe_b64decode(data["publicKey"] + "=")
    payload = {"endpoint":"https://fixture.invalid/browser","keys":{"p256dh":"fixture","auth":"fixture"}}
    compare("/api/push/subscribe", bearer, "POST", {}, 400)
    compare("/api/push/subscribe", web, "POST", payload)
    compare("/api/push/unsubscribe", peer, "POST", {"endpoint":payload["endpoint"]})
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT user_id FROM push_subscriptions WHERE endpoint=%s", (payload["endpoint"],)).fetchone() == (uid,)
    compare("/api/push/unsubscribe", web, "POST", {"endpoint":payload["endpoint"]})

if through("rewind"):
    compare("/api/rewind", bearer, "POST", {}, 409)
    with psycopg.connect(database) as connection:
        connection.execute("INSERT INTO snoozes(user_id,other_id,until_at,created_at) VALUES (%s,%s,2147480000,2147470000)", (uid,other))
    status, _, undone = request(rust_port, "/api/rewind", "POST", {}, **bearer)
    assert status == 200 and undone["undid"] == "snooze" and undone["card"]["id"] == other
    compare("/api/rewind", bearer, "POST", {}, 409)

if through("place-validation"):
    for city, country, expected in [("мск", "", 200), ("Москва", "Сербия", 400), ("Podgorica", "Черногория", 200),
                                    ("м", "", 400), ("unknownsyntheticplace", "", 503)]:
        compare("/api/places/resolve", bearer, "POST", {"city":city,"country":country}, expected)
    with psycopg.connect(database) as connection:
        connection.execute("INSERT INTO place_resolutions(query,country,city,created_at) VALUES ('testsyntheticcity','','Тестоград · Сербия',0)")
    compare("/api/places/resolve", bearer, "POST", {"city":"testsyntheticcity"})
    assert compare("/api/me/city", bearer, "POST", {"city":"Белград"})["user"]["city"] == "Белград"

if through("photo-metadata"):
    with psycopg.connect(database) as connection:
        first = connection.execute("SELECT id FROM photos WHERE user_id=%s ORDER BY id LIMIT 1", (uid,)).fetchone()[0]
        second = connection.execute("INSERT INTO photos(user_id,path,sort_order,created_at) VALUES (%s,'portraits/p03.jpg',2,0) RETURNING id", (uid,)).fetchone()[0]
    compare("/api/photos/order", bearer, "PATCH", {"ids":[first,first]}, 400)
    compare("/api/photos/order", bearer, "PATCH", {"ids":[first]}, 409)
    photos = compare("/api/photos/order", bearer, "PATCH", {"ids":[second,first]})["photos"]
    assert photos[0]["id"] == second and photos[0]["is_primary"]
    compare(f"/api/photos/{second}", peer, "PATCH", {"is_primary":True}, 404)
    assert compare(f"/api/photos/{first}", web, "PATCH", {"is_primary":True})["photos"][0]["id"] == first

if through("discovery"):
    compare("/api/likes", bearer)
    compare("/api/likes?neuro=audhd&country=" + urlencode({"x":"Сербия"})[2:], bearer)
    compare("/api/likes?city=syntheticmissingcity", bearer)
    with psycopg.connect(database) as connection:
        # Isolate one candidate; identical requests then have a deterministic result.
        fixture = connection.execute("SELECT id FROM users WHERE id NOT IN (%s,%s) AND photo<>'' ORDER BY id LIMIT 1", (uid,other)).fetchone()[0]
        skip = [row[0] for row in connection.execute("SELECT id FROM users WHERE id<>%s",(fixture,))]
        connection.execute("""UPDATE users SET paused=0,incognito=0,report_banned_at=NULL,seek_min_age=18,seek_max_age=99,
            seek_place='',hide_tags='',looking_for='everyone',profile_setup_required=0 WHERE id=%s""", (fixture,))
        connection.execute("DELETE FROM swipes WHERE from_id=%s AND to_id=%s", (uid,fixture))
        connection.execute("DELETE FROM feed_history WHERE user_id=%s AND other_id=%s", (uid,fixture))
    path = "/api/feed?" + urlencode({"skip":",".join(map(str,skip)),"hide_empty":"0","min_age":"19","limit":"1"})
    data = compare(path,bearer)
    assert [card["id"] for card in data["cards"]] == [fixture] and "email" not in data["cards"][0]
    with psycopg.connect(database) as connection:
        connection.execute("UPDATE users SET hide_tags='nonsmalltalk' WHERE id=%s", (fixture,))
    assert not compare(path,bearer)["cards"]
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT count(*) FROM filter_events WHERE user_id=%s AND source='feed' AND kind='age'", (uid,)).fetchone()[0] == 1

if through("profile-edit"):
    compare("/api/me", bearer, "PATCH", {"age":17}, 400)
    compare("/api/me", bearer, "PATCH", {"bio":"fuck"}, 400)
    compare("/api/me", bearer, "PATCH", {"primary_photo_id":0}, 404)
    draft = compare("/api/me", web, "PATCH", {"draft":True,"bio":"Draft fixture","seek_min_age":35,"seek_max_age":22})
    assert draft["draft"] and draft["user"]["seek_min_age"]==22
    saved = compare("/api/me", bearer, "PATCH", {"job":"Fixture occupation","prompts":[{"id":"green","answer":"Synthetic answer"}],"city":"Москва"})
    assert not saved["draft"] and saved["user"]["city"]=="Москва"
    compare("/api/me", bearer, "PATCH", {"name":"Synthetic new name"})
    compare("/api/me", bearer, "PATCH", {"name":"One more name"}, 409)

if through("recommendations"):
    compare("/api/recommendations",bearer,expected=403)
    with psycopg.connect(database) as connection:
        connection.execute("UPDATE users SET jev_recommendations_unlocked=1,jev_feed_enabled=1,seek_min_age=18,seek_max_age=99,paused=0,incognito=0 WHERE id=%s", (uid,))
    result=compare("/api/recommendations",bearer)
    assert result["recommendation_source"] in ("local","taste")
    assert all("email" not in card and card["recommendation_reasons"] for card in result["cards"])

with psycopg.connect(database) as connection:
    assert connection.execute("SELECT body FROM messages WHERE id=%s", (message,)).fetchone()[0] == "edited message"
    assert connection.execute("SELECT excluded_at, viewed_at FROM feed_history WHERE user_id=%s AND other_id=%s", (uid, other)).fetchone()[0] == 124
    connection.execute("UPDATE users SET report_banned_at=1 WHERE id=%s", (uid,))
compare("/api/support/replies", bearer)
for port in (flask_port, rust_port):
    status, _, body = request(port, f"/api/people/{other}", **bearer)
    assert status == 403 and body["report_banned"]
if stage == "media":
    with psycopg.connect(database) as connection:
        connection.execute("UPDATE users SET report_banned_at=NULL WHERE id=%s", (uid,))
        connection.execute("DELETE FROM blocks WHERE (from_id=%s AND to_id=%s) OR (from_id=%s AND to_id=%s)", (uid,other,other,uid))
        connection.execute("""INSERT INTO swipes(from_id,to_id,direction,created_at) VALUES (%s,%s,'like',1),(%s,%s,'like',1)
            ON CONFLICT(from_id,to_id) DO UPDATE SET direction='like'""", (uid,other,other,uid))

    def raw(path, auth, method="GET"):
        conn = HTTPConnection("127.0.0.1", rust_port, timeout=15)
        try:
            conn.request(method, path, headers={"Host": "localhost", **auth})
            res = conn.getresponse()
            res.read()
            return res.status, res.getheader("x-accel-redirect"), res.getheader("content-type")
        finally:
            conn.close()

    upload = Path(os.environ["UPLOAD_DIR"])
    private_path = f"chat/{uid}/private-test.jpg"
    private = upload / private_path
    private.parent.mkdir(parents=True, exist_ok=True)
    private.write_bytes(Path("public/portraits/p01.jpg").read_bytes())
    profile_path = f"{uid}/profile-test.jpg"
    profile_file = upload / profile_path
    profile_file.parent.mkdir(parents=True, exist_ok=True)
    profile_file.write_bytes(private.read_bytes())
    with psycopg.connect(database) as connection:
        media_id = connection.execute("INSERT INTO messages (from_id,to_id,body,photo,created_at) VALUES (%s,%s,'',%s,120) RETURNING id", (uid, other, private_path)).fetchone()[0]
    path = f"/api/messages/media/{media_id}"
    assert raw(path, {})[:2] == (401, None)
    assert raw(path, web) == (200, f"/_wiring_private_uploads/{private_path}", "image/jpeg")
    assert raw(path, peer, "HEAD") == raw(path, web)
    assert raw(path + "?s=sm", peer) == (200, f"/_wiring_private_thumbs/chat/{private_path}", "image/jpeg")
    assert raw(path + "?index=1", web)[:2] == (404, None)
    assert raw(f"/media/{private_path}", {})[:2] == (404, None)
    assert raw(f"/media/{profile_path}", {}) == (200, f"/_wiring_uploads/{profile_path}", "image/jpeg")
    if os.environ.get("SOCIAL_PHOTO_UPLOAD_CHECKS") == "1":
        def send_photos(path, fields, photos, auth):
            boundary="wiring-synthetic-photos"
            form=b"".join(f"--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n".encode() for name,value in fields.items())
            key="files" if path.endswith("/photo") else "file"
            for data in photos:
                form+=f"--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"; filename=\"fixture.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n".encode()+data+b"\r\n"
            form+=f"--{boundary}--\r\n".encode()
            conn=HTTPConnection("127.0.0.1",rust_port,timeout=20)
            conn.request("POST",path,form,{"Content-Type":f"multipart/form-data; boundary={boundary}",**auth})
            response=conn.getresponse()
            result=response.status,json.loads(response.read())
            conn.close()
            return result

        image=private.read_bytes()
        assert send_photos("/api/photos",{},[image],{})[0]==401
        assert send_photos("/api/photos",{},[b"no"],bearer)[0]==400
        assert send_photos("/api/photos",{"album_id":"0"},[image],bearer)[0]==404
        status, uploaded=send_photos("/api/photos",{"album":"synthetic album"},[image],bearer)
        assert status==200 and uploaded["photo"]["id"] and uploaded["photo"]["url"].startswith(f"/media/{uid}/")
        status, sent=send_photos("/api/messages/photo",{"to_id":other,"body":"Synthetic two photos"},[image,image],bearer)
        assert status==200 and sent["id"]
        with psycopg.connect(database) as connection:
            cover, extra=connection.execute("SELECT photo,extra_photos FROM messages WHERE id=%s",(sent["id"],)).fetchone()
        assert len(json.loads(extra))==2 and (upload/cover).read_bytes().startswith(b"\xff\xd8\xff")
    if os.environ.get("SOCIAL_VOICE_UPLOAD_CHECKS") == "1":
        import io
        import wave

        def voice(fields, data, auth):
            boundary="wiring-synthetic-upload"
            form=b"".join(f"--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n".encode() for name,value in fields.items())
            if data is not None:
                form+=f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"fixture.wav\"\r\nContent-Type: audio/wav\r\n\r\n".encode()+data+b"\r\n"
            form+=f"--{boundary}--\r\n".encode()
            conn = HTTPConnection("127.0.0.1", rust_port, timeout=12)
            conn.request("POST", "/api/messages/voice", form, {"Content-Type":f"multipart/form-data; boundary={boundary}",**auth})
            response=conn.getresponse()
            result=response.status,json.loads(response.read())
            conn.close()
            return result

        recording=io.BytesIO()
        with wave.open(recording,"wb") as output:
            output.setnchannels(1)
            output.setsampwidth(2)
            output.setframerate(8000)
            output.writeframes(b"\x00\x00"*8000)
        data=recording.getvalue()
        assert voice({"to_id":other},data,{})[0] == 401
        assert voice({"to_id":other},None,bearer)[0] == 400
        assert voice({"to_id":other},b"no",bearer)[0] == 400
        status, sent=voice({"to_id":other,"duration":900,"reply_to_id":media_id},data,bearer)
        assert status == 200 and sent["ok"]
        with psycopg.connect(database) as connection:
            rel, duration, reply=connection.execute("SELECT audio,audio_duration,reply_to_id FROM messages WHERE id=%s",(sent["id"],)).fetchone()
        assert duration==90 and reply==media_id and (upload/rel).read_bytes()==data
        assert raw(f"/api/messages/media/{sent['id']}",peer)[2]=="audio/mp4"
    if proxy_port := os.environ.get("SOCIAL_MEDIA_NGINX_PORT"):
        def proxy(path, auth, method="GET"):
            conn = HTTPConnection("127.0.0.1", int(proxy_port), timeout=10)
            try:
                conn.request(method, path, headers={"Host": "localhost", **auth})
                res = conn.getresponse()
                return res.status, res.headers, res.read()
            finally:
                conn.close()

        status, headers, body = proxy(path, {**web,"Range":"bytes=0-3"})
        assert status == 206 and headers["Content-Range"].startswith("bytes 0-3/") and body == private.read_bytes()[:4]
        assert headers["Cache-Control"] == "private, max-age=86400"
        status, headers, body = proxy(path, peer, "HEAD")
        assert status == 200 and not body and int(headers["Content-Length"]) == private.stat().st_size
        assert proxy(f"/_wiring_private_uploads/{private_path}", {})[0] == 404
        assert proxy(path, {})[0] == 401
        status, headers, body = proxy(path + "?s=sm", peer)
        assert status == 200 and headers["Content-Type"] == "image/jpeg" and 0 < len(body) < private.stat().st_size
        assert headers["Cache-Control"] == "private, max-age=86400"
    if os.environ.get("SOCIAL_FILE_WRITE_CHECKS") == "1":
        compare(f"/api/messages/{media_id}", peer, "DELETE", expected=404)
        status, _, deleted = request(rust_port,f"/api/messages/{media_id}","DELETE",**web)
        assert status == 200 and deleted == {"ok": True} and not private.exists()
        with psycopg.connect(database) as connection:
            assert connection.execute("SELECT deleted_at FROM messages WHERE id=%s",(media_id,)).fetchone()[0]
            audio_id = connection.execute("""INSERT INTO messages (from_id,to_id,body,audio,transcript,created_at)
                VALUES (%s,%s,'','chat/1/fixture.ogg','test transcript',120) RETURNING id""", (uid,other)).fetchone()[0]
        assert compare(f"/api/messages/{audio_id}/transcribe", peer, "POST") == {"ok": True,"transcript":"test transcript"}
        compare(f"/api/messages/{media_id}/transcribe", peer, "POST", expected=404)
    else:
        with psycopg.connect(database) as connection:
            connection.execute("UPDATE messages SET deleted_at=121 WHERE id=%s", (media_id,))
    assert raw(path, peer)[:2] == (404, None)
    status, cookies, guest = request(rust_port, "/api/demo", "POST", {})
    assert status == 200 and guest["user"]["guest"]
    guest_session = {"Cookie": next(cookie.split(";", 1)[0] for cookie in cookies.get_all("Set-Cookie") if cookie.startswith("wiring_session="))}
    assert compare("/api/me", guest_session)["user"]["id"] == guest["user"]["id"]
    compare("/api/report", bearer, "POST", {"user_id":uid,"reason":"spam"}, 400)
    compare("/api/report", bearer, "POST", {"user_id":other,"reason":"unknown"}, 400)
    with psycopg.connect(database) as connection:
        report_target = connection.execute("SELECT id FROM users WHERE id NOT IN (%s,%s,%s) ORDER BY id DESC LIMIT 1", (uid,other,guest["user"]["id"])).fetchone()[0]
        report_message = connection.execute("INSERT INTO messages(from_id,to_id,body,created_at) VALUES (%s,%s,'fixture report thread',123) RETURNING id", (uid,report_target)).fetchone()[0]
    status, _, report = request(rust_port,"/api/report","POST",{"user_id":report_target,"reason":"spam","details":"synthetic case"},**bearer)
    assert status == 200 and report == {"ok": True}
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT reason,details FROM reports WHERE from_id=%s AND to_id=%s", (uid,report_target)).fetchone() == ("spam","synthetic case")
        assert connection.execute("SELECT deleted_at FROM messages WHERE id=%s", (report_message,)).fetchone()[0]
    if os.environ.get("SOCIAL_PHOTO_DELETE_CHECKS") == "1":
        with psycopg.connect(database) as connection:
            album = connection.execute("SELECT id FROM albums WHERE user_id=%s ORDER BY id LIMIT 1",(uid,)).fetchone()[0]
            photo = connection.execute("INSERT INTO photos(user_id,album_id,path,is_primary,sort_order,created_at) VALUES (%s,%s,%s,0,100,123) RETURNING id", (uid,album,profile_path)).fetchone()[0]
            last = connection.execute("SELECT id FROM photos WHERE user_id=%s ORDER BY id LIMIT 1",(other,)).fetchone()[0]
            connection.execute("DELETE FROM photos WHERE user_id=%s AND id<>%s",(other,last))
        compare(f"/api/photos/{photo}", peer, "DELETE", expected=404)
        compare(f"/api/photos/{last}", peer, "DELETE", expected=400)
        status, _, payload = request(rust_port, f"/api/photos/{photo}", "DELETE", **web)
        assert status == 200 and payload["ok"] and payload["photos"] and all(item["id"] != photo for item in payload["photos"])
        assert not profile_file.exists()

print("Synthetic web and mobile social routes matched Flask; exclusions and report appeals were preserved.")

if os.environ.get("SOCIAL_SUPPORT_CHECKS") == "1":
    compare("/support", bearer, "POST", {"body":"short"}, 400)
    compare("/support", web, "POST", {"body":"long enough body", "email":"not mail"}, 400)
    status, _, result = request(rust_port,"/support","POST",{"body":"Synthetic mobile support ticket"},**bearer)
    assert status == 200 and result["ok"]
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT user_id FROM support_tickets WHERE body='Synthetic mobile support ticket'").fetchone() == (uid,)
    conn = HTTPConnection("127.0.0.1", rust_port, timeout=12)
    conn.request("GET", "/support")
    response = conn.getresponse()
    assert response.status == 200 and b"<html" in response.read()
    conn.close()
    compare("/support", {"Authorization":"Bearer synthetic-invalid"}, expected=401)
    conn=HTTPConnection("127.0.0.1",rust_port,timeout=12)
    conn.request("GET","/support",headers=bearer)
    response=conn.getresponse()
    assert response.status==200 and response.getheader("Set-Cookie") is None and b"<html" in response.read()
    conn.close()
    boundary="synthetic-fixture-boundary"
    body=(f"--{boundary}\r\nContent-Disposition: form-data; name=\"body\"\r\n\r\n"
          f"Synthetic browser multipart ticket\r\n--{boundary}--\r\n").encode()
    conn = HTTPConnection("127.0.0.1", rust_port, timeout=12)
    conn.request("POST", "/support", body, {"Content-Type":f"multipart/form-data; boundary={boundary}"})
    response = conn.getresponse()
    html = response.read()
    conn.close()
    assert response.status == 200 and b"__WIRING_SUPPORT_" not in html and b"<html" in html
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT user_id FROM support_tickets WHERE body='Synthetic browser multipart ticket'").fetchone() == (None,)

if os.environ.get("SOCIAL_ADMIN_CHECKS") == "1":
    from itsdangerous import URLSafeTimedSerializer
    from flask.sessions import SecureCookieSessionInterface

    interface=SecureCookieSessionInterface()
    signer=URLSafeTimedSerializer(os.environ["APP_SECRET_KEY"],salt=interface.salt,serializer=interface.serializer,
                                  signer_kwargs={"key_derivation":interface.key_derivation,"digest_method":interface.digest_method})
    admin={"Cookie":"wiring_session="+signer.dumps({"admin":True})}
    expected_origin={"Origin":"https://wiring.club"}

    def action(path, fields, headers):
        conn=HTTPConnection("127.0.0.1",rust_port,timeout=10)
        conn.request("POST",path,urlencode(fields),{"Content-Type":"application/x-www-form-urlencoded",**headers})
        response=conn.getresponse()
        result=response.status,response.getheader("location"),response.read()
        conn.close()
        return result

    assert action("/admin/promo",{"code":"SYNTHETIC"},expected_origin)[0]==403
    assert action("/admin/promo",{"code":"SYNTHETIC"},{**web,**expected_origin})[0]==403
    assert action("/admin/promo",{"code":"SYNTHETIC"},{**admin,"Origin":"https://example.invalid"})[0]==403
    assert action("/admin/promo",{"code":"SYNTHETIC"},admin)[0]==403
    assert action("/admin/promo",{"code":"SYNTHETIC","days":3,"max_uses":7},{**admin,**expected_origin})[:2]==(302,"/admin")
    assert action("/admin/premium",{"who":other,"days":10},{**admin,**expected_origin})[:2]==(302,"/admin")
    assert action(f"/admin/users/{uid}/report-ban-clear",{},{**admin,**expected_origin})[:2]==(302,"/admin#bans")
    assert action("/admin/tasks/0/decision",{"decision":"approved"},{**admin,**expected_origin})[0]==404
    with psycopg.connect(database) as connection:
        first = connection.execute("INSERT INTO support_tickets(name,email,body,ip,created_at) VALUES ('Fixture','','Please fix the broken upload button for all users','',100) RETURNING id::text").fetchone()[0]
        second = connection.execute("INSERT INTO support_tickets(name,email,body,ip,created_at) VALUES ('Fixture','','Please fix the broken upload button for all users','',101) RETURNING id::text").fetchone()[0]
    for _ in range(2):
        assert action("/admin/tasks/import-support",{},{**admin,**expected_origin})[:2]==(302,"/admin#tasks")
    with psycopg.connect(database) as connection:
        assert connection.execute("SELECT days,max_uses FROM promo_codes WHERE code='SYNTHETIC'").fetchone()==(3,7)
        assert connection.execute("SELECT premium_until FROM users WHERE id=%s",(other,)).fetchone()[0]>0
        assert connection.execute("SELECT report_banned_at FROM users WHERE id=%s",(uid,)).fetchone()==(None,)
        task = connection.execute("SELECT id,status FROM feature_tasks WHERE source_type='support' AND source_id=%s",(first,)).fetchone()
        assert task[1]=="candidate"
        assert connection.execute("SELECT duplicate_of,status FROM feature_tasks WHERE source_type='support' AND source_id=%s",(second,)).fetchone()==(task[0],"duplicate")
    assert action(f"/admin/tasks/{task[0]}/decision",{"decision":"approved"},{**admin,**expected_origin})[:2]==(302,"/admin#tasks")

if os.environ.get("SOCIAL_ADMIN_PAGE_CHECKS") == "1":
    import re

    def panel(path, cookie="", method="GET", fields=None, lang="en"):
        conn=HTTPConnection("127.0.0.1",rust_port,timeout=12)
        headers={"Cookie":cookie,"X-Wiring-Language":lang,"Origin":"https://wiring.club"}
        if fields is not None: headers["Content-Type"]="application/x-www-form-urlencoded"
        conn.request(method,path,None if fields is None else urlencode(fields),headers)
        response=conn.getresponse()
        result=response.status,response.headers,response.read().decode()
        conn.close()
        return result

    status, headers, content=panel("/admin")
    assert status==200 and headers["Cache-Control"]=="no-store" and "dev@wiring.test" not in content
    login_cookie=headers["Set-Cookie"].split(";",1)[0]
    csrf=re.search(r'name="csrf" value="([A-Za-z0-9_-]+)"',content).group(1)
    assert panel("/admin",login_cookie,"POST",{"token":"synthetic-admin-key-for-scratch","csrf":"wrong"})[0]==403
    status, headers, _=panel("/admin",login_cookie,"POST",{"token":"synthetic-admin-key-for-scratch","csrf":csrf})
    assert status==303 and headers["Location"]=="/admin"
    admin=headers["Set-Cookie"].split(";",1)[0]
    for language in ("en","ru"):
        status, headers, content=panel("/admin",admin,lang=language)
        assert status==200 and headers["Cache-Control"]=="no-store" and "data-admin-tabs" in content and "SYNTHETIC" in content, (status,"data-admin-tabs" in content,"SYNTHETIC" in content)
    assert panel("/admin/export/marketing-audience")[0]==403
    status, headers, content=panel("/admin/export/marketing-audience",admin)
    assert status==200 and "attachment" in headers["Content-Disposition"] and "dev@wiring.test" in content
    assert panel("/admin?out=1",admin)[0]==302
    if os.environ.get("SOCIAL_ADMIN_REPLY_CHECKS") == "1":
        status, headers, content=panel("/admin",admin)
        assert status==200
        token=re.search(r'data-csrf="([A-Za-z0-9_-]+)"',content).group(1)
        admin=headers["Set-Cookie"].split(";",1)[0]
        with psycopg.connect(database) as connection:
            ticket=connection.execute("INSERT INTO support_tickets(user_id,name,body,email,ip,created_at) VALUES(%s,'Fixture','Synthetic support case','','',100) RETURNING id",(uid,)).fetchone()[0]
        path=f"/admin/replies/ticket/{ticket}"
        fields={"csrf":token,"body":"Synthetic reply","request_id":"synthetic-reply-key-01"}
        assert panel(path,admin,"POST",{**fields,"csrf":"wrong"})[0]==403
        for _ in range(2):
            status, headers, _=panel(path,admin,"POST",fields)
            assert status==302 and headers["Location"]=="/admin#support"
        assert panel(path,admin,"POST",{**fields,"body":"Different message"})[0]==400
        with psycopg.connect(database) as connection:
            assert connection.execute("SELECT body,user_id FROM support_replies WHERE request_id=%s",(fields["request_id"],)).fetchall()==[(fields["body"],uid)]
        assert "Synthetic reply" in panel("/admin",admin)[2]
