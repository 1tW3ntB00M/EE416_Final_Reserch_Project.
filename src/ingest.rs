use odin_actor::prelude::*;
use odin_server::prelude::*;
use serde::{Deserialize, Serialize};
use crate::gsp_service::GspService;

// --- 1. messages this actor can receive: one struct per mailbox "slot" ---
// Raw bytes as they arrive from the LoRa gateway (transport TBD - for now
// another task just does h_ingest.send_msg(LoraPacket(bytes)).await).
#[derive(Debug)]
pub struct LoraPacket(pub Vec<u8>);


// Decoded + validated event, ready to log/store (and later forward to
// odin_server / GSP estimator). Kept as separate message so you can inject
// already-decoded events in tests without going through the bit decoder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightningEvent {
    pub event_id: String, // UUID from sensor, or server-assigned if LoRa omits it
    pub sensor_id: u16,   // DAT-002 / FUN-F3-001
    pub t_v: i64,         // optical trigger Unix secs (UTC)
    pub t_a: i64,         // acoustic onset Unix secs (UTC)
    pub delta_ms: u32,    // (T_a - T_v) in ms, must be 0 < dt <= 60_000
    pub pixel_col: u16,   // strike column for azimuth (FUN-F5-002)
    pub confidence: u8,   // 0-100 edge confidence from sensor
    pub flags: u8,        // health / optical-only-test bit etc.
}

#[derive(Debug)]
pub struct SensorEvent(pub LightningEvent);

// --- 2. message set: the full alphabet this actor understands ---
// (system msgs _Start_, _Terminate_, ... are added automatically)
define_actor_msg_set! {pub IngestMsg = LoraPacket | SensorEvent}

impl LightningEvent {
    // FUN-F4-004: schema / time-order validation. Invalid => quarantine, never panic.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.t_a <= self.t_v {
            anyhow::bail!("T_a ({}) <= T_v ({})", self.t_a, self.t_v);
        }
        let dt = self.t_a - self.t_v;
        if dt > 60 {
            anyhow::bail!("delta {}s > 60s (BR-F2-01)", dt);
        }
        Ok(())
    }
}

// Bit decoder stub. TODO: replace with real byte layout once firmware freezes.
// Assumed minimal layout (little-endian, 13 bytes):
// [0..2] sensor_id u16 | [2..6] t_v u32 | [6..8] delta_ms u16 |
// [8..10] pixel_col u16 | [10] confidence u8 | [11] flags u8 | [12] checksum (xor 0..12)
pub fn decode_lora(bytes: &[u8]) -> anyhow::Result<LightningEvent> {
    if bytes.len() < 13 {
        anyhow::bail!("LoRa packet too short: {} < 13 bytes", bytes.len());
    }
    let sensor_id = u16::from_le_bytes([bytes[0], bytes[1]]);
    let t_v = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as i64;
    let delta_ms = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
    let pixel_col = u16::from_le_bytes([bytes[8], bytes[9]]);
    let confidence = bytes[10];
    let flags = bytes[11];
    let cksum = bytes[12];
    let calc: u8 = bytes[0..12].iter().fold(0u8, |a, b| a ^ b);
    if cksum != calc {
        anyhow::bail!("checksum mismatch: got {:02x} want {:02x}", cksum, calc);
    }
    let t_a = t_v + (delta_ms as i64) / 1000; // secs resolution for now
    Ok(LightningEvent {
        event_id: format!("{}-{}", sensor_id, t_v),
        sensor_id,
        t_v,
        t_a,
        delta_ms,
        pixel_col,
        confidence,
        flags,
    })
}

// --- 3. state + constructor: everything the actor remembers between msgs ---
pub struct IngestActor {
    hserver: ActorHandle<SpaServerMsg>,
    pub events_seen: u64,
    pub quarantined: u64,
    pub last_event: Option<LightningEvent>, // simple "store" for now
}

impl IngestActor {
    pub fn new(hserver: ActorHandle<SpaServerMsg>) -> Self {
        Self { hserver, events_seen: 0, quarantined: 0, last_event: None }
    }

    fn store(&mut self, evt: LightningEvent) {
        // TODO: replace with DB / forward to GSP estimator + odin_server BroadcastWsMsg
        match WsMsg::json(GspService::mod_path(), "gsp", &evt) {
            Ok(ws_msg) => {
                if let Err(e) = self.hserver.try_send_msg(BroadcastWsMsg{ws_msg}) {
                    warn!("broadcast failed: {}", e);
                }
            }
            Err(e) => warn!("ws json failed: {}", e),
        }

        info!("ingest store sensor={} dt={}ms pix={} conf={}",
            evt.sensor_id, evt.delta_ms, evt.pixel_col, evt.confidence);
        self.last_event = Some(evt);
        self.events_seen += 1;
    }

    fn quarantine(&mut self, reason: &str, raw: &[u8]) {
        // FUN-F4-004: never silently drop — count + log with reason
        self.quarantined += 1;
        warn!("ingest quarantine ({}), {} bytes: {:02x?}", reason, raw.len(), raw);
    }
}

// --- 4. behavior: what to do for each mailbox slot ---
// cont! = keep running, stop! = stop actor, term! = request system termination
impl_actor! { match msg for Actor<IngestActor, IngestMsg> as
    _Start_ => cont! {
        info!("IngestActor started");
    }
    LoraPacket => cont! {
        // decode bits -> validate -> store or quarantine
        match decode_lora(&msg.0) {
            Ok(evt) => match evt.validate() {
                Ok(()) => self.store(evt),
                Err(e) => self.quarantine(&e.to_string(), &msg.0),
            },
            Err(e) => self.quarantine(&e.to_string(), &msg.0),
        }
    }
    SensorEvent => cont! {
        match msg.0.validate() {
            Ok(()) => self.store(msg.0),
            Err(e) => warn!("quarantine decoded event {}: {}", msg.0.event_id, e),
        }
    }
}
