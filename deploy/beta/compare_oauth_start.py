"""Exercise cross-runtime OAuth state and cookies with synthetic beta providers.

Cancellation and the Apple form bridge stop before any outbound provider exchange.
"""
from http.client import HTTPConnection
from http.cookies import SimpleCookie
from urllib.parse import parse_qs, urlencode, urlsplit
import base64
import hashlib
import json
import os
import sys
import uuid

import psycopg


def call(server, port, method, path, data=None, cookie=None, origin="https://beta.wiring.club", form=False):
    headers={"Host":"beta.wiring.club","X-Wiring-Language":"en","X-Forwarded-Proto":"https",
             "X-Forwarded-For":"beta-oauth-"+uuid.uuid4().hex}
    if origin: headers["Origin"]=origin
    if cookie: headers["Cookie"]=cookie
    if data is not None: headers["Content-Type"]="application/x-www-form-urlencoded" if form else "application/json"
    conn=HTTPConnection(server,port,timeout=10)
    try:
        conn.request(method,path,(urlencode(data) if form else json.dumps(data)) if data is not None else None,headers)
        res=conn.getresponse()
        body=res.read()
        if body and res.getheader("Content-Type")=="application/json": body=json.loads(body)
        cookies=SimpleCookie()
        for item in res.headers.get_all("Set-Cookie",[]): cookies.load(item)
        return res.status,res.headers,body,cookies
    finally: conn.close()


rust=(sys.argv[1],int(sys.argv[2]))
flask=("127.0.0.1",5070)
assert call(*rust,"GET","/api/auth/providers")[2]["providers"]==["google","yandex","apple"]
assert call(*flask,"GET","/api/auth/providers")[2]["providers"]==["google","yandex","apple"]
assert call(*rust,"POST","/api/auth/google/start",{"mode":"login"},origin="https://invalid.test")[0]==403
assert call(*rust,"POST","/api/auth/google/start",{"mode":"register"})[0]==400
assert call(*rust,"POST","/api/auth/google/start",{"mode":"login","native":True})[0]==400
assert call(*rust,"POST","/api/auth/unknown/start",{"mode":"login"})[0]==503

with psycopg.connect(os.environ["DATABASE_URL"],autocommit=True) as db:
    assert db.info.dbname=="wiring_beta"
    for provider, host in (("google","accounts.google.com"),("yandex","oauth.yandex.ru"),("apple","appleid.apple.com")):
        path=f"/api/auth/{provider}"
        status, headers, body, cookies=call(*rust,"POST",path+"/start",{"mode":"login"})
        assert status==200 and headers.get("Cache-Control")=="no-store" and headers.get("Referrer-Policy")=="no-referrer"
        query=parse_qs(urlsplit(body["url"]).query)
        assert urlsplit(body["url"]).netloc==host and query["redirect_uri"]==["https://beta.wiring.club"+path+"/callback"]
        state=query["state"][0]
        sha=hashlib.sha256(state.encode()).hexdigest()
        browser,verifier=db.execute("SELECT browser,verifier FROM oauth_flows WHERE state_hash=%s",(sha,)).fetchone()
        if provider=="apple":
            assert query["nonce"]==[verifier] and query["response_mode"]==["form_post"]
        else:
            challenge=base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).decode().rstrip("=")
            assert query["code_challenge"]==[challenge] and query["code_challenge_method"]==["S256"]
        cookie="wiring_session="+cookies["wiring_session"].value
        if provider=="apple":
            code,headers,_,_=call(*flask,"POST",path+"/callback",{"state":state,"error":"access_denied"},form=True)
            assert code==303
            callback=headers["Location"]
        else: callback=path+"/callback?"+urlencode({"state":state,"error":"access_denied"})
        code,headers,_,cookies=call(*flask,"GET",callback,cookie=cookie)
        assert code==302 and headers["Location"]=="/login"
        cookie="wiring_session="+cookies["wiring_session"].value
        code,_,body,cookies=call(*rust,"GET","/api/auth/providers",cookie=cookie)
        assert code==200 and "cancel" in body["error_message"].lower()
        updated=cookies.get("wiring_session")
        assert call(*flask,"GET","/api/auth/providers",cookie="wiring_session="+updated.value if updated else None)[2]["error_message"]==""
        assert not db.execute("SELECT 1 FROM oauth_flows WHERE state_hash=%s",(sha,)).fetchone()

    native={"mode":"login","native":True,"code_challenge":"x"*43,"native_state":"y"*32}
    for start, resume in ((rust,flask),(flask,rust)):
        status,_,body,cookies=call(*start,"POST","/api/auth/google/start",native)
        assert status==200 and "wiring_session" not in cookies
        uri=urlsplit(body["url"])
        status,headers,_,cookies=call(*resume,"GET",uri.path+"?"+uri.query)
        assert status==302 and urlsplit(headers["Location"]).netloc=="accounts.google.com"
        assert cookies["wiring_session"]["secure"]
        state=parse_qs(uri.query)["state"][0]
        assert call(*resume,"GET",uri.path+"?"+urlencode({"state":state,"ticket":"z"*43}))[0]==400
        db.execute("DELETE FROM oauth_flows WHERE state_hash=%s",(hashlib.sha256(state.encode()).hexdigest(),))
print("OAuth start, provider list, browser cookie, cancellation, PKCE and Apple form bridge passed.")
