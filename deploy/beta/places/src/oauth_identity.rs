use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde_json::Value;
use subtle::ConstantTimeEq;

pub(crate) type Text = (&'static str, &'static str);
const RETRY: Text = ("Could not sign in. Please try again.", "Не удалось войти. Попробуй ещё раз.");
const UNAVAILABLE: Text = ("The sign-in service is temporarily unavailable. Please try again.",
    "Сервис входа временно недоступен. Попробуй ещё раз.");
const APPLE: Text = ("Could not verify your Apple account. Start signing in again.",
    "Не удалось проверить аккаунт Apple. Начни вход заново.");

pub(crate) struct Identity {
    pub subject: String,
    pub email: String,
    pub name: String,
    pub authoritative: bool,
}

pub(crate) fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder().https_only(true).no_proxy()
        .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(8)).build()
}

pub(crate) async fn exchange(http: &reqwest::Client, provider: &str, id: &str, secret: &str,
    code: &str, callback: &str, verifier: &str, now: i64) -> Result<Identity, Text> {
    let (token_url, info_url) = match provider {
        "google" => ("https://oauth2.googleapis.com/token","https://openidconnect.googleapis.com/v1/userinfo"),
        "yandex" => ("https://oauth.yandex.ru/token","https://login.yandex.ru/info?format=json"),
        "apple" => ("https://appleid.apple.com/auth/token",""),
        _ => return Err(RETRY),
    };
    let mut form = vec![("grant_type","authorization_code"),("code",code),
        ("client_id",id),("client_secret",secret),("redirect_uri",callback)];
    if provider != "apple" { form.push(("code_verifier",verifier)); }
    let encoded = form_urlencoded::Serializer::new(String::new()).extend_pairs(form).finish();
    let tokens = fetch(http.post(token_url).header("Content-Type","application/x-www-form-urlencoded").body(encoded)).await?;
    let info = if provider == "apple" {
        let token = tokens.get("id_token").and_then(Value::as_str).ok_or(APPLE)?;
        let keys = fetch(http.get("https://appleid.apple.com/auth/keys")).await?;
        apple_claims(token,&keys,id,verifier,now).ok_or(APPLE)?
    } else {
        let token = tokens.get("access_token").and_then(Value::as_str).filter(|s|!s.is_empty()).ok_or(RETRY)?;
        let scheme = if provider=="google" {"Bearer"} else {"OAuth"};
        fetch(http.get(info_url).header("Authorization",format!("{scheme} {token}"))).await?
    };
    identity(provider,id,&info)
}

async fn fetch(request: reqwest::RequestBuilder) -> Result<Value, Text> {
    let mut response = request.send().await.map_err(|_| UNAVAILABLE)?;
    if !response.status().is_success() { return Err(UNAVAILABLE); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| UNAVAILABLE)? {
        if bytes.len()+chunk.len() > 65_536 { return Err(UNAVAILABLE); }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_|UNAVAILABLE)?;
    if !value.is_object() || value.get("error").is_some_and(|error|!error.is_null()) { return Err(RETRY); }
    Ok(value)
}

fn apple_claims(token: &str, keys: &Value, client: &str, nonce: &str, now: i64) -> Option<Value> {
    if token.len()>8192 { return None; }
    let (signed, sig)=token.rsplit_once('.')?;
    let (header, claims)=signed.split_once('.')?;
    let header:Value=serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()?;
    if header["alg"] != "RS256" {return None;}
    let kid=header["kid"].as_str()?;
    let key=keys["keys"].as_array()?.iter().find(|key| key["kid"]==kid && key["kty"]=="RSA"
        && key.get("alg").is_none_or(|alg|alg=="RS256") && key.get("use").is_none_or(|usage|usage=="sig"))?;
    let n=URL_SAFE_NO_PAD.decode(key["n"].as_str()?).ok()?;
    let e=URL_SAFE_NO_PAD.decode(key["e"].as_str()?).ok()?;
    RsaPublicKeyComponents {n,e}.verify(&RSA_PKCS1_2048_8192_SHA256,signed.as_bytes(),&URL_SAFE_NO_PAD.decode(sig).ok()?).ok()?;
    let claims:Value=serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).ok()?).ok()?;
    let audience=claims["aud"]==client || claims["aud"].as_array().is_some_and(|aud|aud.iter().any(|v|v==client));
    if claims["iss"]!="https://appleid.apple.com" || !audience
        || claims["exp"].as_i64()?<=now || claims["iat"].as_i64()?>now
        || claims.get("nbf").is_some_and(|v|v.as_i64().is_none_or(|s|s>now))
        || !bool::from(nonce.as_bytes().ct_eq(claims["nonce"].as_str()?.as_bytes()))
        || !(claims["email_verified"]==true || claims["email_verified"]=="true")
    { return None; }
    Some(claims)
}

fn identity(provider:&str,client:&str,info:&Value)->Result<Identity,Text> {
    let text = |key:&str| info.get(key).and_then(Value::as_str).unwrap_or("").trim();
    let email=text(if provider=="yandex" {"default_email"} else {"email"}).to_lowercase();
    let id=if provider=="yandex" {"id"} else {"sub"};
    let subject=if let Some(number)=info.get(id).and_then(Value::as_i64) {number.to_string()} else {text(id).to_owned()};
    let email_shape = email.split_once('@').is_some_and(|(local,domain)|!local.is_empty() && !domain.contains('@')
        && domain.rsplit_once('.').is_some_and(|(l,r)|!l.is_empty() && !r.is_empty()) && !email.chars().any(|c|c.is_whitespace()||c.is_control()));
    if subject.is_empty() || subject.chars().count()>255 || subject.contains('\0') || !email_shape {
        return Err(("The sign-in service did not provide an email address. Allow email access and try again.",
            "Сервис входа не передал адрес почты. Разреши доступ к почте и повтори вход."));
    }
    if ["@wiring.guest","@wiring.demo","@deleted.wiring"].iter().any(|d|email.ends_with(d)) || email=="demo@wiring.app" {
        return Err(("This address cannot be used to sign in.","Этот адрес нельзя использовать для входа."));
    }
    if provider=="google" && info["email_verified"]!=true {
        return Err(("Verify your email in your Google account first.","Сначала подтверди почту в аккаунте Google."));
    }
    if provider=="yandex" && info["client_id"]!=client {
        return Err(("Could not verify your Yandex account.","Не удалось проверить аккаунт Яндекса."));
    }
    let authoritative=if provider=="google" {email.ends_with("@gmail.com")||!text("hd").is_empty()}
    else {provider=="yandex" && email.rsplit_once('@').is_some_and(|(_,domain)|
        ["yandex.ru","yandex.com","ya.ru","yandex.by","yandex.kz","yandex.ua","yandex.com.tr"].contains(&domain))};
    let first=text(if provider=="google" {"given_name"} else {"first_name"});
    let name:String=(if first.is_empty() {text("display_name")} else {first}).chars().filter(|c|*c!='\0').take(32).collect();
    let name=if name.chars().count()>=2 {name} else {String::new()};
    Ok(Identity {subject,email,name,authoritative})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_identity_policy_does_not_link_unverified_or_third_party_mail() {
        let google = json!({"sub":"synthetic","email":"first@example.test","email_verified":true});
        assert!(!identity("google","wiring-test",&google).unwrap().authoritative);
        assert!(identity("google","wiring-test",&json!({"sub":"synthetic","email":"a@gmail.com",
            "email_verified":true})).unwrap().authoritative);
        assert!(identity("google","wiring-test",&json!({"sub":"synthetic","email":"a@gmail.com",
            "email_verified":false})).is_err());
        assert!(identity("yandex","wiring-test",&json!({"id":"synthetic","default_email":"a@yandex.ru",
            "client_id":"other-app"})).is_err());
        assert!(!identity("apple","wiring-test",&json!({"sub":"synthetic","email":"a@example.test"})).unwrap().authoritative);
    }
}
