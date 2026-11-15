use pbkdf2::sha2::{Digest,Sha256};
use tokio_postgres::Transaction;

use crate::{chat_write::TransactionResult,message_send::take};

const REQUESTS:&[&str]=&["добав","сдела","нужн","хочу","можн","убра","исправ","почин","поменя","улучш","не работает","слом","ошиб","баг","add","fix","remove","change","want","need","please"];
const NOISE:&[&str]=&["ага","ок","окей","спасибо","понятно","ясно","лол","круто","кайф","угу","да","нет","+1","👍","❤️","❤","...","-"];

pub(crate) async fn import(tx:&Transaction<'_>,now:i64,threshold:f32)->TransactionResult<()> {
    tx.execute("SELECT pg_advisory_xact_lock(739201,13)",&[]).await?;
    let tickets=tx.query("SELECT id::text,name,email,body,created_at::bigint FROM support_tickets ORDER BY id LIMIT 100",&[]).await?;
    for ticket in tickets {
        let id:&str=ticket.get(0);let name:&str=ticket.get(1);let email:&str=ticket.get(2);
        let body:&str=ticket.get(3);let timestamp:i64=ticket.get(4);
        let text=take(&body.split_whitespace().collect::<Vec<_>>().join(" "),4000);
        let normalized=fold(&text);
        let contains=|words:&[&str]|words.iter().any(|word|normalized.contains(word));
        let request=contains(REQUESTS);
        if normalized.is_empty() || NOISE.contains(&normalized.as_str()) || !request && normalized.chars().count()<28 {continue;}
        if tx.query_opt("SELECT 1 FROM feature_tasks WHERE source_type='support' AND source_id=$1",&[&id]).await?.is_some(){continue;}
        let category=if contains(&["не работает","слом","ошиб","баг","падает","не откры","не груз","fix"]){"баг"}
            else if contains(&["текст","слово","описан","перевод","опечат","глоссар","copy"]){"контент"}
            else if contains(&["кноп","экран","интерфейс","дизайн","мобил","верст","цвет","размер","удобн","ui","ux"]){"интерфейс"}
            else if contains(&["добав","хочу","нужн","можн","фича","функц","add","want","need"]){"фича"}else{"другое"};
        let impact=if contains(&["невозможно","никто","всем","теряем","оплата","безопас"]){5}else if category=="баг"{4}else{3};
        let reach=if contains(&["все","кажд","пользовател","мног","у всех","everyone","users"]){5}
            else if contains(&["мне","у меня","лично","my "]){2}else{3};
        let effort=if contains(&["интеграц","телеграм","аналитик","платеж","миграц","api","backend"]){5}
            else if category=="контент"{1}else if category=="интерфейс"{2}else{3};
        let confidence=((text.chars().count()/90+1) as i32+i32::from(request)).clamp(1,5);
        let score=(((impact as f64*0.35+reach as f64*0.25+(6-effort) as f64*0.2+confidence as f64*0.2)*2.0).clamp(0.0,10.0)*10.0).round_ties_even() as f32/10.0;
        let term=normalized.chars().map(|c|if c.is_alphanumeric()||c=='_'{c}else{' '}).collect::<String>();
        let term=term.split_whitespace().collect::<Vec<_>>().join(" ");
        let dedupe=Sha256::digest(term.as_bytes())[..16].iter().map(|b|format!("{b:02x}")).collect::<String>();
        let duplicate=tx.query_opt("SELECT id::bigint FROM feature_tasks WHERE dedupe_key=$1 AND status NOT IN ('rejected','duplicate') ORDER BY id LIMIT 1",&[&dedupe]).await?.map(|row|row.get::<_,i64>(0));
        let status=if duplicate.is_some(){"duplicate"}else if score>=threshold{"candidate"}else{"filtered"};
        let first=text.split(['.','!','?','\n']).next().unwrap_or_default().trim_matches([' ','—',':','-']);
        let title=take(if first.chars().count()<8{&text}else{first},140);
        let author=take(if !name.is_empty(){name}else if !email.is_empty(){email}else{"гость"},160);
        tx.execute("INSERT INTO feature_tasks(source_type,source_id,source_chat,source_author,source_created_at,raw_text,title,category,
            impact,reach,effort,confidence,score,threshold,status,duplicate_of,created_at,updated_at,dedupe_key)
            VALUES('support',$1,'форма поддержки',$2,$3::bigint,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14::bigint,$15::bigint,$15::bigint,$16)
            ON CONFLICT(source_type,source_id) DO NOTHING",&[&id,&author,&timestamp,&text,&title,&category,&impact,&reach,&effort,&confidence,
            &score,&threshold,&status,&duplicate,&now,&dedupe]).await?;
    }
    Ok(())
}

fn fold(text:&str)->String {text.to_lowercase().replace('ß',"ss").replace('ς',"σ")}
