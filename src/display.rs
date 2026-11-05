//! Versioned display state: the shown record (DAT-003, FUN-F6-001, IF-004).
//!
//! Produced once by the server from the classified event + registry geometry,
//! persisted and published verbatim. The browser renders it without re-deriving
//! color/position.

use serde::{Deserialize, Serialize};

use crate::config::SensorConfig;
use crate::protocol::{GspState, LightningEvent};

/// Bitmap + state-rule version stamped on every shown record.
pub const METHOD_VERSION: &str = "gsp-lora/v1:state-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatLonAlt {
    pub lat: f64,
    pub lon: f64,
    pub alt: f64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GspDisplayState {
  pub event_id: String,      // links to LightningEvent
  pub sensor_id: u16,
  pub state: GspState,
  pub wedge_color: String,   // e.g. "#FFFF00" — server decides, client renders
  pub dot: Option<LatLonAlt>,// Some iff Confirmed; approximate nominal point
  pub nominal_range_m: u32,  // input used for the dot
  pub t_v: i64,              // keep time on the shown record
  pub flags: u8,             // raw control bits {flash,edge,acoustic}
  pub confidence: u8,        // 0-100
  pub method_version: String,// e.g. "gsp-lora/v1:state-v1"
}

impl GspDisplayState {
  // Sole constructor — moves strikePos/wedge logic off gsp.js into Rust.
  // Takes classified evt + registry cfg, computes wedge_color + dot (None unless Confirmed+valid dt).
  pub fn from_event(evt: &LightningEvent, cfg: &SensorConfig, method_version: &str) -> Self {
    // Downgrade invalid Confirmed to wedge-only (FUN-F5-005) rather than fabricating a dot.
    let mut state = evt.state;
    if state == GspState::Confirmed {
      let dt_ok = match (evt.t_a, evt.delta_ms) {
        (Some(t_a), Some(d)) => t_a > evt.t_v && d > 0 && d <= 60_000,
        _ => false,
      };
      if !dt_ok {
        state = GspState::UnlocatedCandidate;
      }
    }
    let wedge_color = match state {
      GspState::Candidate => "#FFFF00".to_string(),
      GspState::Probable => "#FFA500".to_string(),
      GspState::Confirmed => "#FF0000".to_string(),
      GspState::UnlocatedCandidate => "#808080".to_string(),
    };
    let dot = if state == GspState::Confirmed {
      // Nominal ground point: FoV-center ray at configured range, pulled slightly
      // inside (x0.92) so the dot reads as inside the wedge (mirrors gsp.js strikePos).
      let (lat, lon) = dest_point(cfg.lat, cfg.lon, cfg.bearing_deg as f64, cfg.nominal_range_m as f64 * 0.92);
      Some(LatLonAlt { lat, lon, alt: cfg.alt })
    } else {
      None
    };
    Self {
      event_id: evt.event_id.clone(),
      sensor_id: evt.sensor_id,
      state,
      wedge_color,
      dot,
      nominal_range_m: cfg.nominal_range_m,
      t_v: evt.t_v,
      flags: evt.flags,
      confidence: evt.confidence,
      method_version: method_version.to_string(),
    }
  }

  // Enforces DAT-004/F5-005: Confirmed requires a dot; anything else forbids one.
  pub fn validate(&self) -> anyhow::Result<()> {
    if self.event_id.is_empty() {
      anyhow::bail!("empty event_id");
    }
    if self.method_version.is_empty() {
      anyhow::bail!("empty method_version");
    }
    match self.state {
      GspState::Confirmed => {
        if self.dot.is_none() {
          anyhow::bail!("Confirmed without dot");
        }
        Ok(())
      }
      _ => {
        if self.dot.is_some() {
          anyhow::bail!("{:?} must not carry a dot", self.state);
        }
        Ok(())
      }
    }
  }

  pub fn is_located(&self) -> bool {
    matches!(self.state, GspState::Confirmed) && self.dot.is_some()
  }
}

// Great-circle destination: mirrors assets/gsp.js destPoint.
fn dest_point(lat_deg: f64, lon_deg: f64, bearing_deg: f64, dist_m: f64) -> (f64, f64) {
  const R: f64 = 6_371_000.0;
  let d = dist_m / R;
  let br = bearing_deg.to_radians();
  let la1 = lat_deg.to_radians();
  let lo1 = lon_deg.to_radians();
  let la2 = (la1.sin() * d.cos() + la1.cos() * d.sin() * br.cos()).asin();
  let lo2 = lo1 + (br.sin() * d.sin() * la1.cos()).atan2(d.cos() - la1.sin() * la2.sin());
  (la2.to_degrees(), lo2.to_degrees())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn test_cfg() -> SensorConfig {
    SensorConfig {
      dev_eui: "TEST".to_string(),
      lat: 37.0, lon: -122.0, alt: 10.0,
      bearing_deg: 270.0, fov_h_deg: 60.0, fov_v_deg: 40.0,
      nominal_range_m: 5000, armed: true, sensitivity: 1,
    }
  }

  fn test_evt(state: GspState) -> LightningEvent {
    LightningEvent {
      event_id: "1-1759090000".to_string(),
      sensor_id: 1,
      t_v: 1759090000,
      t_a: Some(1759090012),
      delta_ms: Some(12000),
      pixel_col: 320,
      confidence: 85,
      flags: 0b111,
      state,
      lat: Some(37.0),
      lon: Some(-122.0),
      bearing_deg: 270.0,
      fov_h_deg: 60.0,
      fov_v_deg: 40.0,
      nominal_range_m: 5000,
    }
  }

  #[test]
  fn confirmed_valid_gets_dot() {
    let st = GspDisplayState::from_event(&test_evt(GspState::Confirmed), &test_cfg(), METHOD_VERSION);
    assert_eq!(st.state, GspState::Confirmed);
    assert_eq!(st.wedge_color, "#FF0000");
    assert!(st.dot.is_some());
    st.validate().unwrap();
    assert!(st.is_located());
  }

  #[test]
  fn confirmed_bad_dt_downgrades() {
    let mut evt = test_evt(GspState::Confirmed);
    evt.t_a = None;
    evt.delta_ms = None; // time-gating fails => wedge-only
    let st = GspDisplayState::from_event(&evt, &test_cfg(), METHOD_VERSION);
    assert_eq!(st.state, GspState::UnlocatedCandidate);
    assert_eq!(st.wedge_color, "#808080");
    assert!(st.dot.is_none());
    st.validate().unwrap();
    assert!(!st.is_located());
  }

  #[test]
  fn candidate_has_no_dot() {
    let st = GspDisplayState::from_event(&test_evt(GspState::Candidate), &test_cfg(), METHOD_VERSION);
    assert_eq!(st.wedge_color, "#FFFF00");
    assert!(st.dot.is_none());
    st.validate().unwrap();
  }

  #[test]
  fn validate_rejects_mismatched_dot() {
    let mut st = GspDisplayState::from_event(&test_evt(GspState::Candidate), &test_cfg(), METHOD_VERSION);
    st.dot = Some(LatLonAlt { lat: 0.0, lon: 0.0, alt: 0.0 });
    assert!(st.validate().is_err()); // non-Confirmed must not carry a dot
    let mut st2 = GspDisplayState::from_event(&test_evt(GspState::Confirmed), &test_cfg(), METHOD_VERSION);
    st2.dot = None;
    assert!(st2.validate().is_err()); // Confirmed requires a dot
  }
}
