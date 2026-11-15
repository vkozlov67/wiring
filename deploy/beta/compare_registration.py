"""Check consent, signup side effects and links in the isolated beta database."""
from concurrent.futures import ThreadPoolExecutor
from http.client import HTTPConnection
from http.cookies import SimpleCookie
from itsdangerous import URLSafeTimedSerializer
import json
import os
import sys
import uuid
import psycopg


def call(server, port, path, payload, extra=None):
    headers = {"Host":"beta.wiring.club", "Content-Type":"application/json",
               "X-Wiring-Language":"en", "X-Forwarded-Proto":"https",
               "X-Forwarded-For":"beta-registration-"+uuid.uuid4().hex, **(extra or {})}
    conn = HTTPConnection(server, port, timeout=12)
    try:
        conn.request("POST", path, json.dumps(payload), headers)
        response=conn.getresponse()
        cookies=SimpleCookie()
        for raw in response.headers.get_all("Set-Cookie", []): cookies.load(raw)
        return response.status, cookies, json.loads(response.read())
    finally:
        conn.close()


rust=(sys.argv[1],int(sys.argv[2]))
flask=("127.0.0.1",5070)
path="/api/register"
for invalid in ({},{"age_confirm":True},{"age_confirm":True,"privacy_confirm":True,"name":"x"},
                {"age_confirm":True,"privacy_confirm":True,"name":"Beta tester","email":"invalid"}):
    a,_,x=call(*flask,path,invalid)
    b,_,y=call(*rust,path,invalid)
    assert (a,x)==(b,y) and b==400

email="beta-registration-"+uuid.uuid4().hex+"@wiring.test"
native=email.replace("registration","native")
with psycopg.connect(os.environ["DATABASE_URL"],autocommit=True) as db:
    assert db.info.dbname=="wiring_beta"
    owner, ref, before = db.execute("SELECT id,referral_code,premium_until FROM users WHERE email='peer@wiring.test'").fetchone()
    start=db.execute("SELECT COALESCE(MAX(id),0) FROM notifications WHERE user_id=%s",(owner,)).fetchone()[0]
    signup={"name":"Beta test", "email":email, "password":"beta-password", "age_confirm":True,
            "privacy_confirm":True,"marketing_consent":True,"ref":ref}
    cookie={"X-Wiring-Registration-Tracking":"server-v1","Cookie":"wiring-analytics-consent=granted; _ym_uid=1234567890"}
    try:
        status,cookies,body=call(*rust,path,signup,cookie)
        assert status==200 and body=={"ok":True,"needs_email_verify":True,"email":email,"registration_tracking":"server"}, (status,body)
        uid, age, premium, marketing=db.execute("SELECT id,age,premium_until,marketing_consent_at FROM users WHERE email=%s",(email,)).fetchone()
        assert age==0 and marketing and premium>=marketing+120*86400
        assert db.execute("SELECT referrer_id FROM referrals WHERE referred_id=%s",(uid,)).fetchone()[0]==owner
        assert db.execute("SELECT channel,mode,delivery_state,client_id FROM registration_events WHERE user_id=%s",(uid,)).fetchone()==("web_email","server","pending","1234567890")
        receipt=cookies["wiring-registration-receipt"]
        assert receipt["httponly"] and receipt["secure"] and receipt["path"]=="/api/analytics"
        assert URLSafeTimedSerializer(os.environ["APP_SECRET_KEY"],salt="wiring-registration-v1").loads(receipt.value)==uid
        assert call(*rust,path,signup,cookie)[0]==409
        old=db.execute("SELECT token FROM email_verifications WHERE user_id=%s",(uid,)).fetchone()[0]
        assert call(*rust,"/api/email/resend",{"email":email})[0]==200
        new=db.execute("SELECT token FROM email_verifications WHERE user_id=%s",(uid,)).fetchone()[0]
        assert new!=old
        assert call(*rust,"/api/email/verify",{"token":old})[0]==400
        assert call(*rust,"/api/email/verify",{"token":new})[0]==200
        assert call(*rust,"/api/password/forgot",{"email":email})[0]==200
        reset=db.execute("SELECT token FROM password_resets WHERE user_id=%s",(uid,)).fetchone()[0]
        assert call(*rust,"/api/password/reset",{"token":reset,"password":"beta-new-password"})[0]==200
        assert call(*flask,"/api/login",{"email":email,"password":"beta-new-password"})[0]==200
        def create_native(_):
            return call(*rust,path,{**signup,"email":native,"ref":""},{**cookie,"X-App-Platform":"ios"})
        with ThreadPoolExecutor(2) as pool:
            results=list(pool.map(create_native,(1,2)))
        assert sorted(item[0] for item in results)==[200,409]
        status,cookies,body=next(item for item in results if item[0]==200)
        assert "registration_tracking" not in body and "wiring-registration-receipt" not in cookies
        assert db.execute("SELECT channel,mode,delivery_state,client_id FROM registration_events e JOIN users u ON u.id=e.user_id WHERE u.email=%s",(native,)).fetchone()==("native_email","native","no_consent",None)
    finally:
        db.execute("DELETE FROM users WHERE email IN (%s,%s)",(email,native))
        db.execute("DELETE FROM notifications WHERE user_id=%s AND id>%s AND kind='referral'",(owner,start))
        db.execute("UPDATE users SET premium_until=%s WHERE id=%s",(before,owner))
print("Signup consent, one account per email, referral, receipt, resend and reset passed on closed beta.")
