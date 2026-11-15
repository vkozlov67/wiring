use std::sync::Arc;

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::{Value, json};
use tokio_postgres::{Transaction, types::ToSql};

use crate::{chat_pair::{PairOutcome, hide_pair}, chat_write::{TransactionResult, Writer, lock_pair, pair_access},
    profile_read::{ProfileRules, media_url}};

enum Outcome {
    MissingSession, Banned, UnknownAction, MissingTarget, SelfTarget, MissingPerson, NoLike, Mutual, Blocked,
    NoPass, NoBlock, NoMatch, NoUndo, Passed, MatchedUndo, Data(Value),
}

pub(crate) enum Action { Read, Revise(Value), Rewind(Arc<ProfileRules>) }

pub(crate) async fn serve(db: &Writer, uid: i64, action: Action, now: i64, lang: &'static str) -> Result<Response, StatusCode> {
    let gate = db.transaction(move |tx| Box::pin(async move {
        let row = tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0 FROM users
            WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
        let Some(row) = row else { return Ok(Some(Outcome::MissingSession)); };
        if row.get::<_,bool>(0) { return Ok(Some(Outcome::Banned)); }
        tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
        Ok(None)
    })).await?.0;
    let result = if let Some(gate) = gate { gate } else {
        db.transaction(move |tx| Box::pin(async move {
            match action {
                Action::Read => (),
                Action::Revise(fields) => {
                    if let Some(outcome) = apply(tx, uid, fields, now).await? { return Ok(outcome); }
                }
                Action::Rewind(profile) => return rewind(tx, &profile, uid, now, lang).await,
            }
            Ok(Outcome::Data(lists(tx, uid, now).await?))
        })).await?.0
    };
    let error = |status, en, ru| (status, Json(json!({"ok":false,"error":if lang == "en" { en } else { ru }}))).into_response();
    Ok(match result {
        Outcome::MissingSession => crate::account_settings::signed_out(lang),
        Outcome::Banned => crate::account_settings::report_ban_response(lang),
        Outcome::UnknownAction => error(StatusCode::BAD_REQUEST,"Unknown action","неизвестное действие"),
        Outcome::MissingTarget => error(StatusCode::BAD_REQUEST,"Person not found","нет человека"),
        Outcome::SelfTarget => error(StatusCode::BAD_REQUEST,"This is you","это ты"),
        Outcome::MissingPerson => error(StatusCode::NOT_FOUND,"Person not found","человек не найден"),
        Outcome::NoLike => error(StatusCode::NOT_FOUND,"No like to remove","лайка нет"),
        Outcome::Mutual => error(StatusCode::CONFLICT,"This is a mutual like. You can remove it from Chats.","это взаимный лайк — убрать можно из чатов"),
        Outcome::Blocked => error(StatusCode::CONFLICT,"Unblock this person first","сначала разблокируй"),
        Outcome::NoPass => error(StatusCode::NOT_FOUND,"No pass to remove","дизлайка нет"),
        Outcome::NoBlock => error(StatusCode::NOT_FOUND,"This person is not blocked","блока нет"),
        Outcome::NoMatch => error(StatusCode::BAD_REQUEST,"There is no mutual like","взаимного лайка нет"),
        Outcome::NoUndo => error(StatusCode::NOT_FOUND,"Nothing to restore","нечего возвращать"),
        Outcome::Passed => error(StatusCode::CONFLICT,"excluded profiles stay hidden","исключённые анкеты не возвращаются"),
        Outcome::MatchedUndo => error(StatusCode::CONFLICT,"This is already a mutual like. You can remove it only from the chat.","это уже взаимный лайк — убрать можно только из чата"),
        Outcome::Data(data) => Json(data).into_response(),
    })
}

async fn rewind(tx: &Transaction<'_>, profile: &ProfileRules, uid: i64, now: i64, lang: &str) -> TransactionResult<Outcome> {
    let last = tx.query_opt("SELECT to_id::bigint,direction,created_at::bigint FROM swipes WHERE from_id=$1::bigint
        ORDER BY created_at DESC,to_id DESC LIMIT 1", &[&uid]).await?;
    let parked = tx.query_opt("SELECT other_id::bigint,created_at::bigint FROM snoozes WHERE user_id=$1::bigint
        ORDER BY created_at DESC,other_id DESC LIMIT 1", &[&uid]).await?;
    if let Some(row) = parked.filter(|row| last.as_ref().is_none_or(|last| row.get::<_,i64>(1)>=last.get::<_,i64>(2))) {
        let other: i64 = row.get(0);
        let pair: [&(dyn ToSql + Sync);2] = [&uid,&other];
        lock_pair(tx,&pair).await?;
        if tx.execute("DELETE FROM snoozes WHERE user_id=$1::bigint AND other_id=$2::bigint",&pair).await? == 0 { return Ok(Outcome::NoUndo); }
        let card = profile.card(tx,other,now,lang).await?;
        return Ok(Outcome::Data(json!({"ok":true,"card":card,"undid":"snooze"})));
    }
    let Some(last) = last else { return Ok(Outcome::NoUndo); };
    let other: i64 = last.get(0);
    let pair: [&(dyn ToSql + Sync);2] = [&uid,&other];
    lock_pair(tx,&pair).await?;
    let Some(row) = tx.query_opt("SELECT direction FROM swipes WHERE from_id=$1::bigint AND to_id=$2::bigint",&pair).await? else { return Ok(Outcome::NoUndo); };
    let direction: &str = row.get(0);
    if direction == "pass" { return Ok(Outcome::Passed); }
    if direction == "like" && pair_access(tx,&pair).await?.1 { return Ok(Outcome::MatchedUndo); }
    tx.execute("DELETE FROM swipes WHERE from_id=$1::bigint AND to_id=$2::bigint",&pair).await?;
    if tx.query_opt("SELECT is_seed!=0 FROM users WHERE id=$1::bigint",&[&other]).await?.is_some_and(|row| row.get::<_,bool>(0)) {
        tx.execute("DELETE FROM swipes WHERE from_id=$2::bigint AND to_id=$1::bigint",&pair).await?;
    }
    let card = profile.card(tx,other,now,lang).await?;
    Ok(Outcome::Data(json!({"ok":true,"card":card,"undid":direction})))
}

async fn apply(tx: &Transaction<'_>, uid: i64, fields: Value, now: i64) -> TransactionResult<Option<Outcome>> {
    let action = fields["action"].as_str().unwrap_or("");
    if !matches!(action, "unlike" | "restore" | "unblock" | "unmatch" | "unsnooze") {
        return Ok(Some(Outcome::UnknownAction));
    }
    let other = match &fields["user_id"] {
        Value::String(s) => s.trim().parse::<i64>().ok(),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|n|
            (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(n)).map(|n| n as i64)),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    };
    let Some(other) = other else { return Ok(Some(Outcome::MissingTarget)); };
    if other == uid { return Ok(Some(Outcome::SelfTarget)); }
    let pair: [&(dyn ToSql + Sync);2] = [&uid,&other];
    lock_pair(tx,&pair).await?;
    let Some(target) = tx.query_opt("SELECT COALESCE(is_seed,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&other]).await? else {
        return Ok(Some(Outcome::MissingPerson));
    };
    match action {
        "unlike" | "restore" => {
            if action == "restore" && tx.query_opt("SELECT 1 FROM blocks WHERE from_id=$1::bigint AND to_id=$2::bigint", &pair).await?.is_some() {
                return Ok(Some(Outcome::Blocked));
            }
            let direction = if action == "unlike" { "like" } else { "pass" };
            if tx.query_opt("SELECT 1 FROM swipes WHERE from_id=$1::bigint AND to_id=$2::bigint AND direction=$3", &[&uid,&other,&direction]).await?.is_none() {
                return Ok(Some(if action == "unlike" { Outcome::NoLike } else { Outcome::NoPass }));
            }
            if action == "unlike" && pair_access(tx,&pair).await?.1 { return Ok(Some(Outcome::Mutual)); }
            if action == "restore" {
                tx.execute("DELETE FROM snoozes WHERE user_id=$1::bigint AND other_id=$2::bigint", &pair).await?;
            }
            tx.execute("DELETE FROM swipes WHERE from_id=$1::bigint AND to_id=$2::bigint", &pair).await?;
            if target.get::<_,bool>(0) { tx.execute("DELETE FROM swipes WHERE from_id=$2::bigint AND to_id=$1::bigint", &pair).await?; }
            tx.execute("DELETE FROM feed_history WHERE user_id=$1::bigint AND other_id=$2::bigint", &pair).await?;
        }
        "unsnooze" => {
            if tx.execute("DELETE FROM snoozes WHERE user_id=$1::bigint AND other_id=$2::bigint", &pair).await? != 0 {
                tx.execute("DELETE FROM feed_history WHERE user_id=$1::bigint AND other_id=$2::bigint AND excluded_at IS NULL", &pair).await?;
            }
        }
        "unblock" => {
            if tx.execute("DELETE FROM blocks WHERE from_id=$1::bigint AND to_id=$2::bigint", &pair).await? == 0 { return Ok(Some(Outcome::NoBlock)); }
        }
        _ => if matches!(hide_pair(tx,uid,other,now,false).await?, PairOutcome::NoMatch) { return Ok(Some(Outcome::NoMatch)); },
    }
    Ok(None)
}

async fn lists(tx: &Transaction<'_>, uid: i64, now: i64) -> TransactionResult<Value> {
    let mut data = json!({"ok":true});
    // All lists refer to the viewer's own decisions. Only snoozes consult blocks in both directions.
    for (key, relation, filter, matched) in [
        ("likes", "JOIN swipes s ON s.to_id=u.id AND s.from_id=$1::bigint AND s.direction='like'", "", "EXISTS(SELECT 1 FROM swipes t WHERE t.from_id=u.id AND t.to_id=$1::bigint AND t.direction='like')"),
        ("passes", "JOIN swipes s ON s.to_id=u.id AND s.from_id=$1::bigint AND s.direction='pass'", "AND NOT EXISTS(SELECT 1 FROM blocks b WHERE b.from_id=$1::bigint AND b.to_id=u.id)", "false"),
        ("blocks", "JOIN blocks s ON s.to_id=u.id AND s.from_id=$1::bigint", "", "false"),
        ("snoozes", "JOIN snoozes s ON s.other_id=u.id AND s.user_id=$1::bigint", "AND s.until_at>$2::bigint AND NOT EXISTS(SELECT 1 FROM blocks b WHERE (b.from_id=$1::bigint AND b.to_id=u.id) OR (b.to_id=$1::bigint AND b.from_id=u.id))", "false"),
    ] {
        let sql = format!("SELECT u.id::bigint,u.name,u.age::bigint,COALESCE(u.city,''),
            COALESCE((SELECT path FROM photos p WHERE p.user_id=u.id ORDER BY p.is_primary DESC,p.sort_order,p.id LIMIT 1),u.photo,''), {matched}
            FROM users u {relation} WHERE COALESCE(u.deleted_at,0)=0 {filter} ORDER BY s.created_at DESC,u.id DESC LIMIT 200");
        let args: [&(dyn ToSql + Sync);2] = [&uid,&now];
        let rows = tx.query(&sql, if key == "snoozes" { &args[..] } else { &args[..1] }).await?;
        data[key] = Value::Array(rows.iter().map(|row| json!({"id":row.get::<_,i64>(0),"name":row.get::<_,Option<&str>>(1),
            "age":row.get::<_,Option<i64>>(2),"city":row.get::<_,&str>(3),"photo":media_url(row.get(4)),"matched":row.get::<_,bool>(5)})).collect());
    }
    Ok(data)
}
