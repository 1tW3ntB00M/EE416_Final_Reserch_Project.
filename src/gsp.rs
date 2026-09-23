use serde::{Serialize, Deserialize};


#[derive(Serialize,Deserialize,Debug,Clone)]
pub struct UncertaintyEllipse {
  pub semi_major_m: f64,
  pub semi_minor_m: f64,
  pub orient_deg: f64,
}

// this # statement defines the traits the struct has
#[derive(Serialize,Deserialize,Debug,Clone)]
pub struct Sensor {

}

#[derive(Serialize,Deserialize,Debug,Clone)] 
pub struct SensorConfig{

}

#[derive(Serialize,Deserialize,Debug,Clone)]
pub struct LightningEvent {
  pub event_id: String,       // UUID
  pub sensor_id: String,
  pub tv_millis: i64,         // UTC epoch ms (= odin_common::EpochMillis) — not f64
  pub ta_millis: i64,
  pub dt_secs: f64,           // derived (Ta-Tv)/1000
  pub azimuth_deg: f64,
  pub range_m: f64,
  pub lat: f64,
  pub lon: f64,
  pub ellipse: UncertaintyEllipse, // { semi_major_m: f64, semi_minor_m: f64, orient_deg: f64 }
  pub confidence: f64,        
  pub config_version: String, 
  pub image_ref: Option<String>,
}

impl LightningEvent {
  pub fn validate(&self) -> Result<(), String> {
    if !(self.ta_millis > self.tv_millis && (self.ta_millis-self.tv_millis) <= 60_000) {
      return Err("invalid dt".into());
    }
    if self.lat < -90.0 || self.lat > 90.0 {
    return Err("invalid latitude".into());
    }
    if self.lon < -180.0 || self.lon > 180.0 {
        return Err("invalide longitude".into())
    }
    if self.confidence < 0.0 ||  self.confidence > 1.0 {
        return Err("invalid confidence".into())
    }
    Ok(())
  }
}


#[derive(Serialize,Deserialize,Debug,Clone)]
pub struct GSPEstimate{

}