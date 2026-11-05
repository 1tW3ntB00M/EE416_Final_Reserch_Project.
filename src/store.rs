// src/store.rs — sqlite persistence (DAT-006, INF-003)
use crate::protocol::LightningEvent;
use crate::display::GspDisplayState;

const CREATE_EVENTS: &str = "
CREATE TABLE IF NOT EXISTS events(
  event_id TEXT PRIMARY KEY,
  sensor_id INTEGER NOT NULL,
  t_v INTEGER NOT NULL,
  raw_json TEXT NOT NULL,
  created_at TEXT NOT NULL
)";

const CREATE_DISPLAY_STATES: &str = "
CREATE TABLE IF NOT EXISTS display_states(
  event_id TEXT PRIMARY KEY,
  sensor_id INTEGER NOT NULL,
  t_v INTEGER NOT NULL,
  state TEXT NOT NULL,
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
  sqlx::query(CREATE_DISPLAY_STATES).execute(&pool).await?;
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

pub async fn insert_display_state(pool: &sqlx::SqlitePool, st: &GspDisplayState) -> anyhow::Result<()> {
  sqlx::query(
    "INSERT OR REPLACE INTO display_states
     (event_id,sensor_id,t_v,state,raw_json,created_at)
     VALUES (?,?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))")
    .bind(&st.event_id).bind(st.sensor_id as i64)
    .bind(st.t_v)
    .bind(format!("{:?}", st.state))
    .bind(serde_json::to_string(st)?)
    .execute(pool).await?;
  Ok(())
}

pub async fn recent_states(pool: &sqlx::SqlitePool, limit: i64) -> anyhow::Result<Vec<GspDisplayState>> {
  let rows: Vec<(String,)> =
    sqlx::query_as("SELECT raw_json FROM display_states ORDER BY t_v DESC LIMIT ?")
      .bind(limit)
      .fetch_all(pool)
      .await?;
  let mut out = Vec::with_capacity(rows.len());
  for (raw,) in rows {
    if let Ok(st) = serde_json::from_str::<GspDisplayState>(&raw) {
      out.push(st);
    }
  }
  Ok(out)
}
