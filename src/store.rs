// src/store.rs — sqlite persistence (DAT-006, INF-003)
use crate::ingest::LightningEvent;

const CREATE_EVENTS: &str = "
CREATE TABLE IF NOT EXISTS events(
  event_id TEXT PRIMARY KEY,
  sensor_id INTEGER NOT NULL,
  t_v INTEGER NOT NULL,
  raw_json TEXT NOT NULL,
  created_at TEXT NOT NULL
)";

const CREATE_AUDIT: &str = "
CREATE TABLE IF NOT EXISTS audit(
  ts TEXT NOT NULL,
  actor TEXT NOT NULL,
  action TEXT NOT NULL,
  detail TEXT NOT NULL
)";

pub async fn init_db(url: &str) -> anyhow::Result<sqlx::SqlitePool> {
  let pool = sqlx::SqlitePool::connect(url).await?;
  sqlx::query(CREATE_EVENTS).execute(&pool).await?;
  sqlx::query(CREATE_AUDIT).execute(&pool).await?;
  Ok(pool)
}

pub async fn recent_events(pool: &sqlx::SqlitePool, limit: i64) -> anyhow::Result<Vec<LightningEvent>> {
  let rows: Vec<(String,)> =
    sqlx::query_as("SELECT raw_json FROM events ORDER BY t_v DESC LIMIT ?")
      .bind(limit)
      .fetch_all(pool)
      .await?;
  let mut out = Vec::with_capacity(rows.len());
  for (raw,) in rows {
    if let Ok(evt) = serde_json::from_str::<LightningEvent>(&raw) {
      out.push(evt);
    }
  }
  Ok(out)
}
