use std::{collections::BTreeMap, env, error::Error, time::{Duration,SystemTime,UNIX_EPOCH}};
use serde_json::json;
use tokio_postgres::{Client,NoTls};

pub(crate) async fn run()->Result<(),Box<dyn Error>> {
    let counter=env::var("YANDEX_METRIKA_ID").unwrap_or_default();
    let secret=env::var("YANDEX_METRIKA_MEASUREMENT_SECRET").unwrap_or_default();
    let counter=counter.trim();
    let secret=secret.trim();
    if !(5..=15).contains(&counter.len()) || !counter.bytes().all(|b|b.is_ascii_digit()) || secret.is_empty() {
        println!("{}",json!({"configured":false}));
        return Ok(());
    }
    let db=env::var("WIRING_REGISTRATION_WORKER_DATABASE_URL")?;
    let mut config:tokio_postgres::Config=db.parse()?;
    config.connect_timeout(Duration::from_secs(4));
    let (mut client,connection)=config.connect(NoTls).await?;
    tokio::spawn(async move {let _=connection.await;});
    client.batch_execute("SET statement_timeout = '8s'; SET lock_timeout = '4s'").await?;
    let host=env::var("SITE_URL").unwrap_or_else(|_|"https://wiring.club".into());
    let path=format!("{}/sign-up",host.trim_end_matches('/'));
    let http=reqwest::Client::builder().https_only(true).no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10)).build()?;
    let now=SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let lock:bool=client.query_one("SELECT pg_try_advisory_lock(739201,8)",&[]).await?.get(0);
    if !lock { println!("{}",json!({"busy":true})); return Ok(()); }
    let summary=sync(&mut client,&http,counter,secret,&path,now).await?;
    println!("{}",serde_json::to_string(&summary)?);
    Ok(())
}

async fn sync(client:&mut Client,http:&reqwest::Client,counter:&str,secret:&str,path:&str,now:i64)
    ->Result<BTreeMap<&'static str,u64>,tokio_postgres::Error>
{
    let mut result=BTreeMap::from([("submitted",0),("retry",0),("rejected",0),("unknown",0),("expired",0)]);
    let tx=client.transaction().await?;
    tx.execute("UPDATE registration_events SET delivery_state='unknown',error_code='interrupted'
        WHERE delivery_state='sending'",&[]).await?;
    let expired=tx.execute("UPDATE registration_events SET delivery_state='expired'
        WHERE delivery_state IN ('pending','needs_attribution') AND created_at < $1::bigint",&[&(now-43_200)]).await?;
    result.insert("expired",expired);
    tx.commit().await?;
    for _ in 0..25 {
        let tx=client.transaction().await?;
        let row=tx.query_opt("SELECT user_id,client_id,created_at FROM registration_events
            WHERE delivery_state='pending' AND consent_at IS NOT NULL AND client_id IS NOT NULL AND next_attempt_at <= $1::bigint
            ORDER BY created_at,user_id LIMIT 1 FOR UPDATE SKIP LOCKED",&[&now]).await?;
        let Some(row)=row else {tx.rollback().await?;break;};
        let uid:i64=row.get(0);
        let client_id:String=row.get(1);
        let created:i64=row.get(2);
        tx.execute("UPDATE registration_events SET delivery_state='sending',attempts=attempts+1 WHERE user_id=$1::bigint",&[&uid]).await?;
        tx.commit().await?;
        let outcome=deliver(http,counter,secret,&client_id,path,created).await;
        let state=if outcome=="retry" {"pending"} else {outcome};
        client.execute("UPDATE registration_events SET delivery_state=$2,submitted_at=$3::bigint,next_attempt_at=$4::bigint,error_code=$5
            WHERE user_id=$1::bigint AND delivery_state='sending'",
            &[&uid,&state,&(outcome=="submitted").then_some(now),&(now+300),&(outcome!="submitted").then_some(outcome)]).await?;
        *result.entry(outcome).or_default()+=1;
    }
    Ok(result)
}

async fn deliver(http:&reqwest::Client,counter:&str,secret:&str,cid:&str,path:&str,created:i64)->&'static str {
    let created=created.to_string();
    let body=form_urlencoded::Serializer::new(String::new()).extend_pairs(
        [("tid",counter),("ms",secret),("cid",cid),("t","event"),("ea","registration_success"),("et",&created),("dl",path)]).finish();
    let Ok(mut response)=http.post("https://mc.yandex.ru/collect")
        .header("Content-Type","application/x-www-form-urlencoded").body(body).send().await else {return "unknown";};
    match response.status().as_u16() {
        429=>"retry",
        400..=499=>"rejected",
        200=>{
            let mut data=Vec::new();
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) if data.len()+chunk.len()<=256=>data.extend_from_slice(&chunk),
                    Ok(None)=>break,
                    _=>return "unknown",
                }
            }
            if std::str::from_utf8(&data).is_ok_and(|s|s.trim()=="<!-- OK -->") {"submitted"} else {"unknown"}
        },
        _=>"unknown",
    }
}
