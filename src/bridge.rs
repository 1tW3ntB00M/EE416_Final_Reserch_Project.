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
use serde::Deserialize;
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

pub async fn run_mqtt(h_ingest: ActorHandle<IngestMsg>, lora: LoraConfig) {
  // 1. where's the mailbox? from lora.ron
  let (broker, uplink) = match &lora.bridge {
    crate::ingest::BridgeConfig::Mqtt { broker, uplink_topic, .. } =>
      (broker.clone(), uplink_topic.clone()),
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
  println!("bridge subscribed, waiting for uplink...");

  // 3. forever loop
  loop {
    match eventloop.poll().await {
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
}