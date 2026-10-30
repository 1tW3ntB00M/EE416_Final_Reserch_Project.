//! Gateway bridge: the LoRa connection into the actor system.
//!
//! The bridge owns the transport (socket/serial) and translates raw bytes
//! into `LoraPacket` messages for `IngestActor`. The actor never touches the
//! network — all it sees is `h_ingest.send_msg(LoraPacket(bytes))`.
//!
//! Two entry points:
//! - `run_simulated` — fake gateway on a timer. Start here: it proves the
//!   end-to-end lane (bridge → decode → validate → store/broadcast) with no
//!   hardware. Delete it once real hardware lands.
//! - `run_mqtt` — skeleton for the real ChirpStack/TTN bridge (rumqttc).
//!   Fill in broker URL/topics from `lora.ron` when the field gateway exists.

use odin_actor::prelude::*;
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS};
use serde::{Deserialize, Serialize};
use base64::prelude::BASE64_STANDARD;
use base64::Engine as _;
use std::time::Duration;
use crate::ingest::{IngestMsg, LoraConfig, LoraPacket};


fn checksum(covered: &[u8]) -> u8 {
    covered.iter().fold(0u8, |a, b| a ^ b)
}

/// Build a 5-byte lightning packet (Flash=1, Thunder=0).
fn event_5b(flags: u8, conf: u8, pix: u16) -> Vec<u8> {
    let mut v = vec![flags, conf, (pix & 0xFF) as u8, (pix >> 8) as u8];
    v.push(checksum(&v));
    v
}

/// Build a 9-byte lightning packet (Flash=1, Thunder=1) with delta_ms.
fn event_9b(flags: u8, conf: u8, pix: u16, delta_ms: u32) -> Vec<u8> {
    let mut v = vec![flags, conf, (pix & 0xFF) as u8, (pix >> 8) as u8];
    v.extend(delta_ms.to_le_bytes());
    v.push(checksum(&v));
    v
}

/// Build a 5-byte status packet (Flash=0).
fn status_5b(flags: u8, battery_pct: u8) -> Vec<u8> {
    let mut v = vec![flags, battery_pct, 0, 0];
    v.push(checksum(&v));
    v
}

/// Fake gateway: sends one event + one status every 10s so the globe and the
/// registry both move. `sensor_id`/time are stubbed inside IngestActor for now
/// (DevEUI lookup lands with the real bridge).
/*pub async fn run_simulated(h_ingest: ActorHandle<IngestMsg>) {
    let mut n: u32 = 0;
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        n += 1;
        // alternate: 9B thunder event, 5B flash-only event, status report
        let (kind, pkt) = match n % 3 {
            1 => ("9B event", event_9b(0b111, 85, 320, 12000)),
            2 => ("5B event", event_5b(0b011, 95, 288)),
            _ => ("status", status_5b(0b100, 42)),
        };
        if let Err(e) = h_ingest.send_msg(LoraPacket(pkt)).await {
            println!("bridge send failed: {}", e);
        } else {
            println!("bridge sent {}", kind);
        }
    }
}
*/

/// Real bridge skeleton (ChirpStack/TTN via MQTT).
/// add `rumqttc` to Cargo.toml, connect with broker URL from
/// `lora.ron`, subscribe to the uplink topic, base64-decode `frm_payload`,
/// look up `sensor_id` from the DevEUI registry, stamp gateway time, and call
/// `h_ingest.send_msg(LoraPacket(bytes)).await` exactly like run_simulated does.
/// Downlink: receive `SendCommand` bytes (see IngestActor TODO) and publish to
/// the downlink topic.
// ChirpStack `event/up` shape (only fields we need)
#[derive(Debug, Deserialize)]
struct Up {
  #[serde(default)]
  data: String,
  #[serde(rename = "deviceInfo", default)]
  device_info: DevInfo,
  #[serde(default)]
  time: String,
}
#[derive(Debug, Default, Deserialize)]
struct DevInfo {
  #[serde(rename = "devEui", default)]
  dev_eui: String,
}

fn parse_broker(broker: &str) -> (String, u16) {
  let s = broker.trim_start_matches("mqtt://").trim_start_matches("tcp://");
  match s.rsplit_once(':') {
    Some((host, port)) => (host.to_string(), port.parse().unwrap_or(1883)),
    None => (s.to_string(), 1883),
  }
}

// ChirpStack v4 MQTT downlink (IF-002): publish to
// `application/{app}/device/{dev_eui}/command/down` with JSON
// `{devEui, confirmed, fPort, data(base64)}`. DevEUI also comes from the topic,
// but ChirpStack requires it in the payload too.
#[derive(Debug, Serialize, Deserialize)]
struct Downlink {
  #[serde(rename = "devEui")]
  dev_eui: String,
  confirmed: bool,
  #[serde(rename = "fPort")]
  f_port: u8,
  data: String, // base64 command bytes (plaintext, ChirpStack encrypts)
}

// FPort for GSP 2-byte commands (Sleep/Arm/Disarm/SetWindow). Provisional —
// confirm with firmware before field use.
pub const DOWNLINK_FPORT: u8 = 10;

pub fn downlink_topic(template: &str, dev_eui: &str) -> String {
  template.replace("{dev_eui}", dev_eui).replace("{devEui}", dev_eui).replace("{deveui}", dev_eui)
}

pub fn encode_downlink(dev_eui: &str, bytes: &[u8]) -> (String, String) {
  let payload = Downlink {
    dev_eui: dev_eui.to_string(),
    confirmed: false,
    f_port: DOWNLINK_FPORT,
    data: BASE64_STANDARD.encode(bytes),
  };
  (dev_eui.to_string(), serde_json::to_string(&payload).unwrap_or_default())
}

pub async fn run_mqtt(h_ingest: ActorHandle<IngestMsg>, lora: LoraConfig, rx_down: MpscReceiver<(String, Vec<u8>)>) {
  // 1. topics from lora.ron
  let (broker, uplink, downlink_template) = match &lora.bridge {
    crate::ingest::BridgeConfig::Mqtt { broker, uplink_topic, downlink_topic } =>
      (broker.clone(), uplink_topic.clone(), downlink_topic.clone()),
    _ => { println!("bridge: Serial not yet supported"); return; }
  };
  let (host, port) = parse_broker(&broker);

  // 2. connect
  let mut opts = MqttOptions::new("odin-gsp-1", host, port);
  opts.set_keep_alive(Duration::from_secs(30));
  opts.set_clean_session(true);
  let (client, mut eventloop) = AsyncClient::new(opts, 10);
  if let Err(e) = client.subscribe(uplink, QoS::AtLeastOnce).await {
    println!("bridge subscribe failed: {}", e);
    return;
  }
  println!("bridge subscribed, waiting for uplink/downlink...");

  // 3. forever loop: uplink (MQTT -> IngestActor) + downlink (IngestActor -> MQTT, IF-002)
  loop {
    tokio::select! {
      // --- uplink lane ---
      ev = eventloop.poll() => {
        match ev {
          Ok(Event::Incoming(Incoming::Publish(p))) => {
            let text = String::from_utf8_lossy(&p.payload).to_string();
            let up: Up = match serde_json::from_str(&text) {
              Ok(u) => u,
              Err(e) => { println!("bridge bad json: {} | {}", e, text); continue; }
            };
            let raw: Vec<u8> = match BASE64_STANDARD.decode(up.data.trim()) {
              Ok(b) => b,
              Err(e) => { println!("bridge bad base64: {}", e); continue; }
            };
            println!("bridge got {} bytes from {} @ {}", raw.len(), up.device_info.dev_eui, up.time);
            // t_rx: TODO parse up.time RFC3339 -> i64; for now gateway now()
            let t_rx = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            if let Err(e) = h_ingest.send_msg(LoraPacket { bytes: raw, dev_eui: up.device_info.dev_eui, t_rx }).await {
              println!("bridge send failed: {}", e);
            }
          }
          Ok(_) => {} // ping / ack — ignore
          Err(e) => {
            println!("mqtt err (will reconnect): {}", e);
            tokio::time::sleep(Duration::from_secs(1)).await;
          }
        }
      }
      // --- downlink lane (IF-002): SendCommand bytes -> ChirpStack command/down ---
      cmd = recv(&rx_down) => {
        match cmd {
          Ok((dev_eui, bytes)) => {
            let topic = downlink_topic(&downlink_template, &dev_eui);
            let (_, json) = encode_downlink(&dev_eui, &bytes);
            println!("bridge downlink {} ({} bytes) -> {}", dev_eui, bytes.len(), topic);
            if let Err(e) = client.publish(topic, QoS::AtLeastOnce, false, json).await {
              println!("bridge downlink publish failed {}: {}", dev_eui, e);
            }
          }
          Err(_) => { println!("bridge downlink channel closed, exiting"); break; }
        }
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn downlink_topic_substitutes_deveui() {
    let t = downlink_topic("application/1/device/{dev_eui}/command/down", "70B3D57ED0000001");
    assert_eq!(t, "application/1/device/70B3D57ED0000001/command/down");
  }

  #[test]
  fn downlink_payload_is_chirpstack_json() {
    let (_, json) = encode_downlink("70B3D57ED0000001", &[0x02, 0x02]); // Arm profile 2
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["devEui"], "70B3D57ED0000001");
    assert_eq!(v["fPort"], DOWNLINK_FPORT);
    assert_eq!(v["confirmed"], false);
    // [0x02,0x02] base64 is "AgI="
    assert_eq!(v["data"], "AgI=");
  }
}