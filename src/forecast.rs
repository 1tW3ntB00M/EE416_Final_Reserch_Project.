//! Forecast-driven arming (FUN-F4-002 / FUN-F4-003, IF-003).
//!
//! Polls open-meteo `hourly=cape` per sensor cell at a configurable cadence
//! (default 15 min). CAPE is used for ARM/DISARM decisions only, never for
//! ranging or display-state computation.
//!
//! Rule (configurable, logged with reason code):
//!   max CAPE over next `horizon_hours` >= `cape_threshold` => ARM
//!   else => DISARM (IDLE, sensors hold last profile, Degraded on fetch loss).
//!
//! Only sends `SendCommand` on state change to avoid downlink spam. All
//! decisions are logged with reason `forecast-rule-v1`.

use std::collections::HashMap;
use std::time::Duration;
use odin_actor::prelude::*;
use serde::{Deserialize, Serialize};
use crate::ingest::{IngestMsg, LoraConfig, SendCommand, SensorCommandKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForecastConfig {
  pub poll_secs: u64,        // <=900 per FUN-F4-002 (default 900 = 15 min)
  pub cape_threshold: f64,   // J/kg, ARM at >= threshold (default 500)
  pub horizon_hours: usize,  // how many hourly steps ahead to consider (default 6)
  pub arm_profile: u8,       // sensitivity profile id for Arm command
}

impl Default for ForecastConfig {
  fn default() -> Self {
    Self { poll_secs: 900, cape_threshold: 500.0, horizon_hours: 6, arm_profile: 2 }
  }
}

impl ForecastConfig {
  pub fn validate(&self) -> anyhow::Result<()> {
    if self.poll_secs == 0 || self.poll_secs > 3600 {
      anyhow::bail!("poll_secs must be 1..=3600, got {}", self.poll_secs);
    }
    if self.cape_threshold < 0.0 {
      anyhow::bail!("cape_threshold must be >= 0, got {}", self.cape_threshold);
    }
    if self.horizon_hours == 0 || self.horizon_hours > 48 {
      anyhow::bail!("horizon_hours must be 1..=48, got {}", self.horizon_hours);
    }
    Ok(())
  }
}

// Pure decision: testable without network (FUN-F4-003 rule).
pub fn should_arm(cape_max: f64, threshold: f64) -> bool {
  cape_max >= threshold
}

pub fn max_cape(capes: &[f64], horizon: usize) -> f64 {
  capes.iter().take(horizon).copied().fold(0.0_f64, f64::max)
}

// Minimal open-meteo hourly shape we need.
#[derive(Debug, Deserialize)]
struct OpenMeteoResp {
  #[serde(default)]
  hourly: Option<Hourly>,
}

#[derive(Debug, Deserialize)]
struct Hourly {
  #[serde(default)]
  cape: Vec<f64>,
}

pub fn parse_cape_max(body: &str, horizon: usize) -> anyhow::Result<f64> {
  let resp: OpenMeteoResp = serde_json::from_str(body)?;
  let capes = resp.hourly.map(|h| h.cape).unwrap_or_default();
  if capes.is_empty() {
    anyhow::bail!("no cape data in forecast response");
  }
  Ok(max_cape(&capes, horizon))
}

pub fn forecast_url(lat: f64, lon: f64) -> String {
  format!(
    "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}&hourly=cape&forecast_days=2&timezone=UTC",
    lat, lon
  )
}

async fn fetch_cape_max(client: &reqwest::Client, lat: f64, lon: f64, horizon: usize) -> anyhow::Result<f64> {
  let body = client.get(forecast_url(lat, lon)).send().await?.error_for_status()?.text().await?;
  parse_cape_max(&body, horizon)
}

/// Polling task. Holds last commanded ARM state per sensor (seeded from
/// `lora.ron` `armed` flags) and only downlinks on change (FUN-F4-003).
pub async fn run_forecast(h_ingest: ActorHandle<IngestMsg>, lora: LoraConfig, cfg: ForecastConfig) {
  if let Err(e) = cfg.validate() {
    println!("forecast bad config: {} — task exiting", e);
    return;
  }
  let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build();
  let client = match client {
    Ok(c) => c,
    Err(e) => { println!("forecast client build failed: {} — task exiting", e); return; }
  };

  // sensor_id -> last commanded armed state
  let mut last: HashMap<u16, bool> = HashMap::new();
  for (id_str, scfg) in &lora.sensors {
    if let Ok(id) = id_str.parse::<u16>() {
      last.insert(id, scfg.armed);
    }
  }

  println!("forecast task started: poll={}s cape>={} horizon={}h ({} sensors)",
    cfg.poll_secs, cfg.cape_threshold, cfg.horizon_hours, last.len());

  loop {
    for (id_str, scfg) in &lora.sensors {
      let sensor_id: u16 = match id_str.parse() {
        Ok(id) => id,
        Err(_) => continue,
      };
      match fetch_cape_max(&client, scfg.lat, scfg.lon, cfg.horizon_hours).await {
        Ok(cape_max) => {
          let want_armed = should_arm(cape_max, cfg.cape_threshold);
          let prev = last.get(&sensor_id).copied().unwrap_or(false);
          if want_armed != prev {
            let (cmd, arg) = if want_armed {
              (SensorCommandKind::Arm, cfg.arm_profile)
            } else {
              (SensorCommandKind::Disarm, 0)
            };
            // reason code logged per FUN-F4-003 (forecast-rule-v1)
            println!("forecast-rule-v1 sensor={} cape_max={:.0} threshold={:.0} {} -> {}",
              sensor_id, cape_max, cfg.cape_threshold,
              if prev { "ARMED" } else { "IDLE" },
              if want_armed { "ARM" } else { "DISARM" });
            if let Err(e) = h_ingest.send_msg(SendCommand { sensor_id, cmd, arg }).await {
              println!("forecast send failed sensor {}: {}", sensor_id, e);
            } else {
              last.insert(sensor_id, want_armed);
            }
          }
        }
        Err(e) => {
          // COND-04 / OS-03: hold last profile, log Degraded, keep polling
          println!("forecast DEGRADED sensor={} (hold last): {}", sensor_id, e);
        }
      }
    }
    tokio::time::sleep(Duration::from_secs(cfg.poll_secs)).await;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn arm_rule_threshold() {
    assert!(should_arm(500.0, 500.0));
    assert!(should_arm(1200.0, 500.0));
    assert!(!should_arm(499.9, 500.0));
  }

  #[test]
  fn max_cape_horizon() {
    assert_eq!(max_cape(&[10.0, 900.0, 50.0], 1), 10.0);
    assert_eq!(max_cape(&[10.0, 900.0, 50.0], 6), 900.0);
  }

  #[test]
  fn parse_openmeteo_body() {
    let body = r#"{"hourly":{"time":["2026-10-27T00:00"],"cape":[0.0,800.0,100.0]}}"#;
    assert_eq!(parse_cape_max(body, 6).unwrap(), 800.0);
    assert_eq!(parse_cape_max(body, 1).unwrap(), 0.0);
    assert!(parse_cape_max(r#"{"hourly":{}}"#, 6).is_err());
  }

  #[test]
  fn forecast_config_bounds() {
    assert!(ForecastConfig::default().validate().is_ok());
    assert!(ForecastConfig { poll_secs: 0, ..Default::default() }.validate().is_err());
    assert!(ForecastConfig { horizon_hours: 99, ..Default::default() }.validate().is_err());
  }
}
