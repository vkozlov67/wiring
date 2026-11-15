use std::collections::HashMap;

use serde_json::{Value,json};
use tokio_postgres::Transaction;

use crate::chat_write::TransactionResult;

pub(crate) async fn dashboard(tx:&Transaction<'_>,now:i64,catalog:&Value,translate:&(impl Fn(&str)->String + Sync))->TransactionResult<Value> {
    let metrics=tx.query_one("WITH classified AS (
          SELECT id,is_seed<>0 AS seed,email LIKE '%@wiring.guest' OR email='demo@wiring.app' AS guest,premium_until FROM users
        ), pairs AS (
          SELECT s.from_id,s.to_id,a.seed AS aseed,a.guest AS aguest,b.seed AS bseed,b.guest AS bguest,
            EXISTS(SELECT 1 FROM messages m WHERE COALESCE(m.deleted_at,0)=0 AND
              ((m.from_id=s.from_id AND m.to_id=s.to_id) OR (m.from_id=s.to_id AND m.to_id=s.from_id))) AS chat
          FROM swipes s JOIN swipes r ON s.from_id=r.to_id AND s.to_id=r.from_id
          LEFT JOIN classified a ON a.id=s.from_id LEFT JOIN classified b ON b.id=s.to_id
          WHERE s.direction='like' AND r.direction='like' AND s.from_id<s.to_id
        ) SELECT jsonb_build_object(
          'users_real',(SELECT count(*) FROM classified WHERE NOT seed AND NOT guest),
          'users_guest',(SELECT count(*) FROM classified WHERE NOT seed AND guest),
          'users_seed',(SELECT count(*) FROM classified WHERE seed),
          'users_plus',(SELECT count(*) FROM classified WHERE NOT seed AND NOT guest AND premium_until>$1::bigint),
          'swipes',(SELECT count(*) FROM swipes),'likes',(SELECT count(*) FROM swipes WHERE direction='like'),
          'passes',(SELECT count(*) FROM swipes WHERE direction='pass'),
          'swipes_today',(SELECT count(*) FROM swipes WHERE created_at>=$1::bigint-86400),
          'swipes_yesterday',(SELECT count(*) FROM swipes WHERE created_at>=$1::bigint-172800 AND created_at<$1::bigint-86400),
          'matches_total',(SELECT count(*) FROM pairs),
          'matches_real',(SELECT count(*) FROM pairs WHERE NOT (aseed OR aguest OR bseed OR bguest)),
          'matches_seed',(SELECT count(*) FROM pairs WHERE (aseed AND NOT bseed AND NOT bguest) OR (bseed AND NOT aseed AND NOT aguest)),
          'match_chats',(SELECT count(*) FROM pairs WHERE chat),
          'match_chats_real',(SELECT count(*) FROM pairs WHERE chat AND NOT (aseed OR aguest OR bseed OR bguest)),
          'messages',(SELECT count(from_id) FROM messages),'reports',(SELECT count(id) FROM reports),'blocks',(SELECT count(from_id) FROM blocks),
          'report_banned',(SELECT count(*) FROM users WHERE report_banned_at IS NOT NULL AND COALESCE(deleted_at,0)=0),
          'audience_export_count',(SELECT count(*) FROM users WHERE deleted_at IS NULL AND COALESCE(is_seed,0)=0 AND email NOT LIKE '%@wiring.guest' AND email<>'demo@wiring.app' AND btrim(email)<>''),
          'filter_sessions_7d',(SELECT count(DISTINCT user_id||':'||fingerprint) FROM filter_events WHERE created_at>=$1::bigint-604800),
          'filter_sessions_30d',(SELECT count(DISTINCT user_id||':'||fingerprint) FROM filter_events WHERE created_at>=$1::bigint-2592000),
          'filter_users_30d',(SELECT count(DISTINCT user_id) FROM filter_events WHERE created_at>=$1::bigint-2592000),
          'device_visits_7d',(SELECT count(created_at) FROM device_visits WHERE created_at>=$1::bigint-604800),
          'device_visits_30d',(SELECT count(created_at) FROM device_visits WHERE created_at>=$1::bigint-2592000)
        )::text",&[&now]).await?;
    let mut stats:Value=serde_json::from_str(metrics.get(0))?;
    for (key,sql) in [
        ("days","SELECT row_to_json(q)::text FROM (SELECT to_char(to_timestamp(created_at),'YYYY-MM-DD') AS day,
            count(*) FILTER(WHERE email NOT LIKE '%@wiring.guest' AND email<>'demo@wiring.app') AS real,
            count(*) FILTER(WHERE email LIKE '%@wiring.guest' OR email='demo@wiring.app') AS guest
            FROM users WHERE COALESCE(is_seed,0)=0 GROUP BY 1 ORDER BY 1 DESC LIMIT 14) q"),
        ("silent","SELECT row_to_json(q)::text FROM (SELECT name,city,age FROM users u WHERE COALESCE(is_seed,0)=0
            AND email NOT LIKE '%@wiring.guest' AND email<>'demo@wiring.app' AND NOT EXISTS(SELECT 1 FROM swipes WHERE from_id=u.id)) q"),
        ("report_banned_users","SELECT row_to_json(q)::text FROM (SELECT u.id,name,email,
            CASE WHEN u.report_banned_at=0 THEN '—' ELSE to_char(to_timestamp(u.report_banned_at),'YYYY-MM-DD HH24:MI') END banned_at,
            (SELECT count(*) FROM reports WHERE to_id=u.id) report_count,(SELECT count(DISTINCT from_id) FROM reports WHERE to_id=u.id) reporter_count
            FROM users u WHERE u.report_banned_at IS NOT NULL AND COALESCE(u.deleted_at,0)=0 ORDER BY u.report_banned_at DESC LIMIT 80) q"),
        ("report_queue","SELECT row_to_json(q)::text FROM (SELECT r.id,r.from_id reporter_id,r.to_id target_id,r.reason,r.details,
            CASE WHEN r.created_at=0 THEN '—' ELSE to_char(to_timestamp(r.created_at),'YYYY-MM-DD HH24:MI') END created_at,
            a.name reporter_name,a.email reporter_email,b.name target_name,b.email target_email,b.is_seed<>0 target_seed,
            COALESCE(b.report_banned_at,0)<>0 target_banned,(SELECT count(*) FROM reports WHERE to_id=b.id) target_report_count,
            (SELECT count(DISTINCT from_id) FROM reports WHERE to_id=b.id) target_reporter_count
            FROM reports r JOIN users a ON a.id=r.from_id JOIN users b ON b.id=r.to_id ORDER BY r.created_at DESC LIMIT 50) q"),
        ("registration_delivery","SELECT row_to_json(q)::text FROM (SELECT delivery_state,count(*) total FROM registration_events GROUP BY 1 ORDER BY 1) q"),
    ] {stats[key]=json!(rows(tx,sql,&[]).await?);}
    stats["recent"]=json!(rows(tx,"SELECT row_to_json(q)::text FROM (SELECT id,name,age,city,email,created_at,
        (SELECT count(*) FROM swipes WHERE from_id=u.id) swipes,COALESCE(premium_until,0)>$1::bigint plus,
        CASE WHEN premium_until>$1::bigint THEN to_char(to_timestamp(premium_until),'YYYY-MM-DD') ELSE '' END plus_until
        FROM users u WHERE COALESCE(is_seed,0)=0 AND email NOT LIKE '%@wiring.guest' AND email<>'demo@wiring.app' ORDER BY created_at DESC LIMIT 30) q",&[&now]).await?);
    for (key,kind,since,limit) in [("filters_neuro","neuro",0,12),("filters_vibe","vibe",0,12),("filters_intent","intent",0,12),
        ("filters_city","city",0,12),("filters_age","age",0,8),("filters_neuro_7d","neuro",now-604800,8)] {
        let mut entries=rows(tx,"SELECT row_to_json(q)::text FROM (SELECT value,count(*) hits,count(DISTINCT user_id) users FROM filter_events
            WHERE kind=$1 AND created_at>=$2::bigint GROUP BY value ORDER BY hits DESC,users DESC,value LIMIT $3::bigint) q",&[&kind,&since,&(limit as i64)]).await?;
        for entry in &mut entries {
            let id=entry["value"].as_str().unwrap_or_default();
            entry["label"]=Value::String(label(catalog,if kind=="intent"{"intents"}else{kind},id));
        }
        stats[key]=json!(entries);
    }
    for (key,os,since,limit) in [("devices_30d",false,now-2592000,8),("devices_7d",false,now-604800,8),("device_os_30d",true,now-2592000,10)] {
        let mut entries=rows(tx,"SELECT row_to_json(q)::text FROM (SELECT CASE WHEN $1 THEN os_name ELSE device_class END value,count(*) hits,
            count(DISTINCT CASE WHEN user_id IS NOT NULL THEN 'u'||user_id ELSE 'v'||visitor_key END) visitors FROM device_visits
            WHERE created_at>=$2::bigint GROUP BY 1 ORDER BY hits DESC,value LIMIT $3::bigint) q",&[&os,&since,&(limit as i64)]).await?;
        for entry in &mut entries {
            let id=entry["value"].as_str().unwrap_or_default();
            let text=match id {"desktop"=>"компьютер","mobile"=>"телефон","tablet"=>"планшет","ios"=>"iOS","android"=>"Android",
                "macos"=>"macOS","windows"=>"Windows","linux"=>"Linux","other"=>"другое",_=>id};
            entry["label"]=json!(translate(text));
        }
        stats[key]=json!(entries);
    }
    if let Some(reports)=stats["report_queue"].as_array_mut(){for report in reports {
        let reason=report["reason"].as_str().unwrap_or_default();
        report["reason"]=json!(label(catalog,"report_reasons",reason));
    }}
    if let Some(delivery)=stats["registration_delivery"].as_array_mut(){for item in delivery {
        let state=item["delivery_state"].as_str().unwrap_or_default();
        item["label"]=json!(translate(match state {"historical"=>"Исторические — без отправки","legacy_browser"=>"Браузерная цель (старый клиент)",
            "no_consent"=>"Нет согласия на аналитику","needs_attribution"=>"Нет ClientID","pending"=>"Ожидает отправки","sending"=>"Отправляется",
            "submitted"=>"Принято Метрикой (API)","rejected"=>"Отклонено Метрикой","unknown"=>"Неизвестный результат — нужна сверка",
            "expired"=>"Истёк срок отправки","erased"=>"Данные удалены",_=>state}));
    }}
    let codes=rows(tx,"SELECT row_to_json(q)::text FROM (SELECT code,days,max_uses,uses,benefit FROM promo_codes ORDER BY code) q",&[]).await?;
    let tickets=rows(tx,"SELECT row_to_json(q)::text FROM (SELECT id,user_id,name,email,body,created_at FROM support_tickets ORDER BY id DESC LIMIT 40) q",&[]).await?;
    let tasks=rows(tx,"SELECT row_to_json(q)::text FROM (SELECT id,title,category,score,status,source_type,source_author,source_chat,raw_text,
      impact,reach,effort,confidence,duplicate_of FROM feature_tasks ORDER BY CASE status WHEN 'candidate' THEN 0 WHEN 'approved' THEN 1
       WHEN 'filtered' THEN 2 WHEN 'duplicate' THEN 3 ELSE 4 END,score DESC,updated_at DESC,id DESC LIMIT 120) q",&[]).await?;
    let replies=rows(tx,"SELECT row_to_json(q)::text FROM (SELECT source_kind,source_id,user_id,email,body,request_id,email_sent_at FROM support_replies ORDER BY id DESC LIMIT 400) q",&[]).await?;
    let mut history=HashMap::<String,HashMap<i64,Vec<Value>>>::from([("report".into(),HashMap::new()),("ticket".into(),HashMap::new())]);
    for row in replies {history.entry(row["source_kind"].as_str().unwrap_or_default().into()).or_default()
        .entry(row["source_id"].as_i64().unwrap_or_default()).or_default().push(row);}
    let task_counts:HashMap<String,i64>=tx.query("SELECT status,count(*) FROM feature_tasks GROUP BY status",&[]).await?.into_iter().map(|row|(row.get(0),row.get(1))).collect();
    Ok(json!({"s":stats,"codes":codes,"tickets":tickets,"tasks":tasks,"task_counts":task_counts,"reply_history":history}))
}

async fn rows(tx:&Transaction<'_>,statement:&str,parameters:&[&(dyn tokio_postgres::types::ToSql+Sync)]) -> TransactionResult<Vec<Value>> {
    let mut result=Vec::new();
    for row in tx.query(statement,parameters).await? {result.push(serde_json::from_str(row.get(0))?);}
    Ok(result)
}

fn label(catalog:&Value,kind:&str,id:&str)->String {
    catalog[kind].as_array().and_then(|values|values.iter().find(|row|row["id"]==id)).and_then(|row|row["label"].as_str()).unwrap_or(id).to_owned()
}
