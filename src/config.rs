//! Sensor registry / bridge configuration (lora.ron).
//!
//! Pure data + DevEUI lookup. No actor, no MQTT, no decoding.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BridgeConfig {
    Mqtt { broker: String, uplink_topic: String, downlink_topic: String },
    Serial { port: String, baud: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorConfig {
    pub dev_eui: String,
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    pub bearing_deg: f32,
    pub fov_h_deg: f32,
    pub fov_v_deg: f32,
    pub nominal_range_m: u32,
    pub armed: bool,
    pub sensitivity: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoraConfig {
    pub version: String,
    pub bridge: BridgeConfig,
    pub sensors: std::collections::HashMap<String, SensorConfig>, // key = sensor_id as string
}

// Flat sensor registry entry sent to the browser ("sensors" WsMsg) so it can
// draw persistent blue dots + grey baseline FOV cones before any event arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorInfo {
    pub sensor_id: u16,
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    pub bearing_deg: f32,
    pub fov_h_deg: f32,
    pub fov_v_deg: f32,
    pub nominal_range_m: u32,
}

impl LoraConfig {
    pub fn sensor_list(&self) -> Vec<SensorInfo> {
        let mut out: Vec<SensorInfo> = self.sensors.iter().filter_map(|(id_str, cfg)| {
            id_str.parse::<u16>().ok().map(|sensor_id| SensorInfo {
                sensor_id,
                lat: cfg.lat,
                lon: cfg.lon,
                alt: cfg.alt,
                bearing_deg: cfg.bearing_deg,
                fov_h_deg: cfg.fov_h_deg,
                fov_v_deg: cfg.fov_v_deg,
                nominal_range_m: cfg.nominal_range_m,
            })
        }).collect();
        out.sort_by_key(|s| s.sensor_id);
        out
    }

    pub fn by_deveui(&self, dev_eui: &str) -> Option<(u16, &SensorConfig)> {
        self.sensors.iter().find_map(|(id_str, cfg)| {
            if cfg.dev_eui.eq_ignore_ascii_case(dev_eui) {
                id_str.parse::<u16>().ok().map(|id| (id, cfg))
            } else {
                None
            }
        })
    }

    /// reverse of `by_deveui`: resolve a configured sensor_id to its DevEUI.
    /// DevEUI is the addressing key for downlink MQTT topics (lora.ron), and
    /// commands arrive as sensor_id — so the downlink lane needs this lookup.
    pub fn deveui_of(&self, sensor_id: u16) -> Option<String> {
        self.sensors.get(&sensor_id.to_string()).map(|cfg| cfg.dev_eui.clone())
    }
}
