//! Ingest actor: LoRa packets in, validated events out (DB + browser).
//!
//! Thin owner of runtime state (counters, health map, pool, registry, downlink
//! lane). Wire codec lives in [`crate::protocol`], registry types in
//! [`crate::config`], display state in [`crate::display`].

use odin_actor::prelude::*;
use odin_server::prelude::*;
use crate::gsp_service::GspService;
use crate::config::LoraConfig;
use crate::protocol::{LightningEvent, SensorStatus, decode_lora, decode_status};
use crate::display::{GspDisplayState, METHOD_VERSION};

// --- 1. messages this actor can receive: one struct per mailbox "slot" ---
// Raw bytes as they arrive from the LoRa gateway (transport TBD - for now
// another task just does h_ingest.send_msg(LoraPacket(bytes)).await).
#[derive(Debug)]
pub struct LoraPacket{
    pub bytes: Vec<u8>,     // the 5/9 gsp-lora/v1 packet
    pub dev_eui: String,    // e.g. "70B3D57ED0000001"
    pub t_rx: i64           // gateway receive time, unix secs
}

#[derive(Debug)]
pub struct SensorEvent(pub LightningEvent);

#[derive(Debug)]
pub struct SensorStatusMsg(pub SensorStatus);

// Re-exported so `crate::ingest::SendCommand` paths used across the crate keep working.
pub use crate::protocol::SendCommand;

// --- 2. message set: the full alphabet this actor understands ---
// (system msgs _Start_, _Terminate_, ... are added automatically)
define_actor_msg_set! {pub IngestMsg = LoraPacket | SensorEvent | SensorStatusMsg | SendCommand}

// --- 3. state + constructor: everything the actor remembers between msgs ---
/// Per-sensor health/registry entry, written by the status lane (Flash=0).
/// Geometry (lat/lon/bearing/FoV) will join this struct once the registry
/// config (lora.ron) lands — for now it tracks health + last-seen only.
#[derive(Debug, Clone, Default)]
pub struct SensorState {
    pub battery_pct: Option<u8>,
    pub low_power: bool,
    pub battery_low: bool,
    pub last_seen_rx: i64,
    pub status_count: u64,
}

pub struct IngestActor {
    hserver: ActorHandle<SpaServerMsg>,
    pub events_seen: u64,
    pub quarantined: u64,
    pub last_event: Option<LightningEvent>, // simple "store" for now
    pub sensors: std::collections::HashMap<u16, SensorState>, // sensor_id -> health
    pub status_seen: u64,
    pool: sqlx::SqlitePool,
    lora: LoraConfig, // field registry + bridge settings from lora.ron
    tx_to_bridge: MpscSender<(String, Vec<u8>)>, // downlink TX lane into the gateway bridge (dev_eui, encoded bytes)
}

impl IngestActor {
    pub fn new(
        hserver: ActorHandle<SpaServerMsg>,
        pool: sqlx::SqlitePool,
        lora: LoraConfig,
        tx_to_bridge: MpscSender<(String, Vec<u8>)>,
    ) -> Self {
        if lora.version != "gsp-lora/v1" {
            println!("ingest WARNING: lora.ron version '{}' != 'gsp-lora/v1', bit map may mismatch", lora.version);
        }
        Self { hserver, events_seen: 0, quarantined: 0, last_event: None,
               sensors: std::collections::HashMap::new(), status_seen: 0, pool, lora, tx_to_bridge}
    }

    async fn store (&mut self, evt: LightningEvent) -> anyhow::Result<()> {
        // TODO: replace with DB / forward to GSP estimator + odin_server BroadcastWsMsg
        // NOTE: println! (not info!) — the tracing subscriber stays silent in this
        // setup (see ODIN_TRACE thread), but stdout demonstrably reaches the terminal.
        match WsMsg::json(GspService::mod_path(), "gsp", &evt) {
            Ok(ws_msg) => {
                if let Err(e) = self.hserver.try_send_msg(BroadcastWsMsg{ws_msg}) {
                    println!("ingest broadcast failed: {}", e);
                }
            }
            Err(e) => println!("ingest ws json failed: {}", e),
        }

        sqlx::query(
        "INSERT OR REPLACE INTO events
        (event_id,sensor_id,t_v,raw_json,created_at)
        VALUES (?,?,?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now'))")
        .bind(&evt.event_id).bind(evt.sensor_id as i64)
        .bind(evt.t_v)
        .bind(serde_json::to_string(&evt)?)
        .execute(&self.pool).await?;

        // Versioned shown record (DAT-003/IF-004): built once here, persisted
        // alongside the event. Broadcast still carries the event until gsp.js
        // is switched to verbatim state rendering.
        match self.lora.sensors.get(&evt.sensor_id.to_string()).cloned() {
            Some(cfg) => {
                let st = GspDisplayState::from_event(&evt, &cfg, METHOD_VERSION);
                match st.validate() {
                    Ok(()) => {
                        if let Err(e) = crate::store::insert_display_state(&self.pool, &st).await {
                            println!("ingest display-state persist failed {}: {}", st.event_id, e);
                        }
                    }
                    Err(e) => println!("ingest display-state invalid {}: {}", evt.event_id, e),
                }
            }
            None => println!("ingest no registry cfg for sensor {} — display state skipped", evt.sensor_id),
        }

        let dt = evt.delta_ms.map(|d| format!("{}ms", d)).unwrap_or_else(|| "-".to_string());
        println!("ingest store sensor={} dt={} pix={} conf={} flags={:#05b} state={:?}",
            evt.sensor_id, dt, evt.pixel_col, evt.confidence, evt.flags, evt.state);

        self.last_event = Some(evt);
        self.events_seen += 1;
        Ok(())
    }

    fn store_status(&mut self, st: SensorStatus) {
        // update registry, then broadcast on "status" (NOT "gsp" — gsp.js ignores it).
        let entry = self.sensors.entry(st.sensor_id).or_default();
        entry.battery_pct = st.battery_pct;
        entry.low_power = st.low_power;
        entry.battery_low = st.battery_low;
        entry.last_seen_rx = st.t_rx;
        entry.status_count += 1;
        self.status_seen += 1;

        if st.battery_low {
            println!("ingest sensor {} battery critical", st.sensor_id);
        }

        match WsMsg::json(GspService::mod_path(), "status", &st) {
            Ok(ws_msg) => {
                if let Err(e) = self.hserver.try_send_msg(BroadcastWsMsg{ws_msg}) {
                    println!("ingest status broadcast failed: {}", e);
                }
            }
            Err(e) => println!("ingest ws json failed: {}", e),
        }
        println!("ingest status sensor={} battery={:?} low_power={} battery_low={}",
            st.sensor_id, st.battery_pct, st.low_power, st.battery_low);
    }

    fn quarantine(&mut self, reason: &str, raw: &[u8]) {
        // FUN-F4-004: never silently drop — count + log with reason
        self.quarantined += 1;
        println!("ingest quarantine ({}), {} bytes: {:02x?}", reason, raw.len(), raw);
    }
}

// --- 4. behavior: what to do for each mailbox slot ---
// cont! = keep running, stop! = stop actor, term! = request system termination
impl_actor! { match msg for Actor<IngestActor, IngestMsg> as
    _Start_ => cont! {
        println!("IngestActor started");
    }
    LoraPacket => cont! {
        // Real path: bridge supplies DevEUI + gateway t_rx; look up sensor_id/cfg.
        if msg.bytes.is_empty() {
            self.quarantine("empty packet", &msg.bytes);
        } else if let Some((sensor_id, cfg)) = self.lora.by_deveui(&msg.dev_eui) {
            let cfg = cfg.clone();
            let t_rx = msg.t_rx;
            if msg.bytes[0] & 0x01 != 0 {
                match decode_lora(&msg.bytes, sensor_id, t_rx, &cfg) {
                    Ok(evt) => match evt.validate() {
                        Ok(()) => {
                            if let Err(e) = self.store(evt).await {
                                println!("ingest store failed: {}", e);
                            }
                        }
                        Err(e) => self.quarantine(&e.to_string(), &msg.bytes),
                    },
                    Err(e) => self.quarantine(&e.to_string(), &msg.bytes),
                }
            } else {
                match decode_status(&msg.bytes, sensor_id, t_rx) {
                    Ok(st) => match st.validate() {
                        Ok(()) => self.store_status(st),
                        Err(e) => self.quarantine(&e.to_string(), &msg.bytes),
                    },
                    Err(e) => self.quarantine(&e.to_string(), &msg.bytes),
                }
            }
        } else {
            let reason = format!("unknown deveui {}", msg.dev_eui);
            self.quarantine(&reason, &msg.bytes);
        }
    }
    SensorEvent => cont! {
        match msg.0.validate() {
            Ok(()) => {
                if let Err(e) = self.store(msg.0).await {
                    println!("ingest store failed: {}", e);
                }
            }
            Err(e) => println!("ingest quarantine decoded event {}: {}", msg.0.event_id, e),
        }
    }
    SensorStatusMsg => cont! {
        // direct injection path for tests (mirrors SensorEvent).
        match msg.0.validate() {
            Ok(()) => self.store_status(msg.0),
            Err(e) => println!("ingest quarantine status sensor {}: {}", msg.0.sensor_id, e),
        }
    }
    SendCommand => cont! {
        // Downlink: encode, resolve the target DevEUI, and hand (dev_eui, bytes)
        // to the gateway bridge TX lane. The bridge owns the MQTT socket and the
        // {dev_eui} topic substitution — we only address the device here.
        // NOTE: SendCommand is a named-field struct, so fields live on `msg` directly.
        match msg.encode() {
            Ok(bytes) => {
                match self.lora.deveui_of(msg.sensor_id) {
                    Some(dev_eui) => {
                        info!("command queued sensor={} {:?} dev_eui={} ({} bytes)",
                            msg.sensor_id, msg.cmd, dev_eui, bytes.len());
                        if let Err(e) = self.tx_to_bridge.send((dev_eui, bytes)).await {
                            println!("ingest bridge send failed sensor {}: {}", msg.sensor_id, e);
                        }
                    }
                    None => println!("ingest no dev_eui for sensor {} — command dropped", msg.sensor_id),
                }
            }
            Err(e) => println!("ingest bad command sensor {}: {}", msg.sensor_id, e),
        }
    }
}
