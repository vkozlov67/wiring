use std::{env,error::Error,time::Duration};

use tokio_postgres::NoTls;

const BASELINE:&str=include_str!("../schema/0001-baseline.sql");
const ID:&str="rust_baseline_0001";

pub(crate) async fn install()->Result<(),Box<dyn Error>> {
    let url=env::var("WIRING_SCHEMA_DATABASE_URL")?;
    let mut config:tokio_postgres::Config=url.parse().map_err(|_|"Invalid schema database config")?;
    config.connect_timeout(Duration::from_secs(4));
    let (mut db,connection)=config.connect(NoTls).await.map_err(|_|"Could not connect to schema database")?;
    tokio::spawn(async move {let _=connection.await;});
    let tx=db.transaction().await?;
    tx.batch_execute("SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='30s'; SELECT pg_advisory_xact_lock(739201,12)").await?;
    if tx.query_one("SELECT to_regclass('public.app_migrations') IS NOT NULL",&[]).await?.get::<_,bool>(0) &&
        tx.query_opt("SELECT 1 FROM public.app_migrations WHERE id=$1",&[&ID]).await?.is_some() {
        return Ok(());
    }
    let existing=tx.query_one("SELECT count(*) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname='public' AND c.relkind IN ('r','p','S','v','m','f')",&[]).await?.get::<_,i64>(0);
    if existing!=0 {return Err("Schema bootstrap requires an empty public schema; existing databases use incremental migrations".into());}
    tx.batch_execute(BASELINE).await.map_err(|error|format!("Schema baseline failed (SQLSTATE {})",error.code().map_or("unknown",|code|code.code())))?;
    tx.execute("INSERT INTO public.app_migrations(id,applied_at) VALUES ($1,EXTRACT(EPOCH FROM now())::integer)",&[&ID]).await?;
    tx.commit().await?;
    println!("Installed the WIRING PostgreSQL baseline");
    Ok(())
}
