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
    pub event_id: String, // server-assigned `{sensor_id}-{t_v}` (LoRa omits it)
    pub sensor_id: u16,   // from DevEUI registry map, NOT on air (DAT-002 / FUN-F3-001)
    pub t_v: i64,         // optical trigger Unix secs (UTC) — gateway T_rx, NOT on air
    pub t_a: Option<i64>, // acoustic onset Unix secs (UTC) — Some iff thunder variant (9B)
    pub delta_ms: Option<u32>, // (T_a - T_v) in ms, Some iff thunder variant; 0 < dt <= 60_000
    pub pixel_col: u16,   // strike column for azimuth (FUN-F5-002)
    pub confidence: u8,   // 0-100 edge confidence from sensor
    pub flags: u8,        // gsp-lora/v1 flags byte verbatim: bit0 Flash, bit1 Line, bit2 Thunder, bits3-7 reserved=0
    pub lat: Option<f64>,
    pub lon: Option<f64>
}

#[derive(Debug)]
pub struct SensorEvent(pub LightningEvent);

// --- Status lane (Flash=0) ---
// Status wire layout (gsp-lora/v1, 5 bytes) — TEAM TODO: confirm bytes 1-3 with firmware.
// [0] flags (bit0 Flash=0, bit1 low_power, bit2 battery_low, bits3-7 reserved=0)
// [1] battery_pct (0-100, 0xFF = unknown)
// [2..4] reserved (send 0x0000 until assigned)
// [4] checksum (xor 0..4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorStatus {
    pub sensor_id: u16,   // from DevEUI registry map, NOT on air
    pub t_rx: i64,        // gateway receive time, NOT on air
    pub battery_pct: Option<u8>, // None if 0xFF unknown
    pub low_power: bool,  // flags bit1
    pub battery_low: bool, // flags bit2 (low power due to battery)
    pub flags: u8,        // flags byte verbatim
}

#[derive(Debug)]
pub struct SensorStatusMsg(pub SensorStatus);

impl SensorStatus {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.flags & 0x01 != 0 {
            anyhow::bail!("not a status packet (Flash=1)");
        }
        if self.flags & 0xF8 != 0 {
            anyhow::bail!("reserved flag bits set: {:#04x}", self.flags);
        }
        Ok(())
    }
}

/// Decode a Flash=0 status packet. sensor_id/t_rx come from the DevEUI
/// registry + gateway timestamp, same as decode_lora.
pub fn decode_status(bytes: &[u8], sensor_id: u16, t_rx: i64) -> anyhow::Result<SensorStatus> {
    if bytes.len() != 5 {
        anyhow::bail!("status packet must be 5 bytes, got {}", bytes.len());
    }
    let flags = bytes[0];
    if flags & 0x01 != 0 {
        anyhow::bail!("not a status packet (Flash=1) — route to decode_lora");
    }
    if flags & 0xF8 != 0 {
        anyhow::bail!("reserved flag bits set: {:#04x}", flags);
    }
    let calc: u8 = bytes[..4].iter().fold(0u8, |a, b| a ^ b);
    if bytes[4] != calc {
        anyhow::bail!("checksum mismatch: got {:02x} want {:02x}", bytes[4], calc);
    }
    // TEAM TODO: update if firmware assigns bytes 2-3 (currently reserved, ignored).
    let battery_pct = match bytes[1] {
        0xFF => None,
        v => Some(v),
    };
    Ok(SensorStatus {
        sensor_id,
        t_rx,
        battery_pct,
        low_power: flags & 0x02 != 0,
        battery_low: flags & 0x04 != 0,
        flags,
    })
}

// --- Downlink (server → sensor, IF-002) ---
// Command set the server can issue. Encoding is 2 bytes: [0] opcode, [1] arg.
// Opcodes are provisional — confirm with firmware before field use.
#[derive(Debug, Clone, Copy)]
pub enum SensorCommandKind {
    Sleep,        // opcode 0x01: enter low-power mode; arg = minutes (0 = until ARM)
    Arm,          // opcode 0x02: enter ARMED scan; arg = sensitivity profile id
    Disarm,       // opcode 0x03: enter IDLE; arg reserved (0)
    SetWindow,    // opcode 0x04: acoustic window secs (1-30); arg = secs
}

#[derive(Debug, Clone)]
pub struct SendCommand {
    pub sensor_id: u16,
    pub cmd: SensorCommandKind,
    pub arg: u8,
}

impl SendCommand {
    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        let op = match self.cmd {
            SensorCommandKind::Sleep => 0x01,
            SensorCommandKind::Arm => 0x02,
            SensorCommandKind::Disarm => 0x03,
            SensorCommandKind::SetWindow => {
                if !(1..=30).contains(&self.arg) {
                    anyhow::bail!("window must be 1-30s, got {}", self.arg);
                }
                0x04
            }
        };
        Ok(vec![op, self.arg])
    }
}

// --- 2. message set: the full alphabet this actor understands ---
// (system msgs _Start_, _Terminate_, ... are added automatically)
define_actor_msg_set! {pub IngestMsg = LoraPacket | SensorEvent | SensorStatusMsg | SendCommand}

impl LightningEvent {
    // FUN-F4-004: schema / time-order validation. Invalid => quarantine, never panic.
    // gsp-lora/v1: Flash=0 (status) never validates as lightning; reserved bits must be 0;
    // thunder variant (Some) enforces 0 < dt <= 60s (BR-F2-01), no-thunder (None) caps at PROBABLE.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.flags & 0x01 == 0 {
            anyhow::bail!("status packet (Flash=0), not a lightning event");
        }
        if self.flags & 0xF8 != 0 {
            anyhow::bail!("reserved flag bits set: {:#04x}", self.flags);
        }
        match (self.t_a, self.delta_ms) {
            (Some(t_a), Some(_)) => {
                if t_a <= self.t_v {
                    anyhow::bail!("T_a ({}) <= T_v ({})", t_a, self.t_v);
                }
                let dt = t_a - self.t_v;
                if dt > 60 {
                    anyhow::bail!("delta {}s > 60s (BR-F2-01)", dt);
                }
                Ok(())
            }
            (None, None) => Ok(()), // no-thunder 5B variant: no dt gating, at most PROBABLE
            _ => anyhow::bail!("inconsistent thunder fields: t_a={:?} delta_ms={:?}", self.t_a, self.delta_ms),
        }
    }
}

// gsp-lora/v1 wire format (little-endian). Identity + time are NOT on air:
// sensor_id comes from the DevEUI registry map, t_v is gateway T_rx — both passed in.
// 5-byte (no thunder): [0] flags | [1] confidence | [2..4] pixel_col u16 | [4] checksum (xor 0..4)
// 9-byte (thunder):    [0..4] same as above + [4..8] delta_ms u32 | [8] checksum (xor 0..8)
// flags: bit0 Flash (1=lightning, 0=status), bit1 Line, bit2 Thunder, bits3-7 reserved=0.
pub fn decode_lora(bytes: &[u8], sensor_id: u16, t_v: i64, lat: Option<f64>, lon: Option<f64>) -> anyhow::Result<LightningEvent> {
    if bytes.len() != 5 && bytes.len() != 9 {
        anyhow::bail!("LoRa packet must be 5 or 9 bytes, got {}", bytes.len());
    }
    let flags = bytes[0];
    let thunder_bit = flags & 0x04 != 0;
    if thunder_bit && bytes.len() != 9 {
        anyhow::bail!("thunder bit set but got {}-byte packet (want 9)", bytes.len());
    }
    if !thunder_bit && bytes.len() != 5 {
        anyhow::bail!("thunder bit clear but got {}-byte packet (want 5)", bytes.len());
    }
    if flags & 0x01 == 0 {
        anyhow::bail!("status packet (Flash=0): line/low-power thunder/battery — route to health log, not LightningEvent");
    }
    if flags & 0xF8 != 0 {
        anyhow::bail!("reserved flag bits set: {:#04x}", flags);
    }
    let confidence = bytes[1];
    let pixel_col = u16::from_le_bytes([bytes[2], bytes[3]]);
    let (cksum, covered) = (bytes[bytes.len() - 1], &bytes[..bytes.len() - 1]);
    let calc: u8 = covered.iter().fold(0u8, |a, b| a ^ b);
    if cksum != calc {
        anyhow::bail!("checksum mismatch: got {:02x} want {:02x}", cksum, calc);
    }
    let (t_a, delta_ms) = if bytes.len() == 9 {
        let delta = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        (Some(t_v + (delta as i64) / 1000), Some(delta))
    } else {
        (None, None)
    };
    Ok(LightningEvent {
        event_id: format!("{}-{}", sensor_id, t_v),
        sensor_id,
        t_v,
        t_a,
        delta_ms,
        pixel_col,
        confidence,
        flags,
        lat,
        lon
    })
}

// Hardcoded test data for webpage display. t_a > t_v and dt <= 60s so
// validate() passes; lat/lon are required by assets/gsp.js upsertEntity()
// (events without lat/lon are hidden on the globe).
pub fn test_events() -> Vec<LightningEvent> {
    vec![
        LightningEvent {
            event_id: "test-001".to_string(),
            sensor_id: 1,
            t_v: 1759090000,
            t_a: Some(1759090012),
            delta_ms: Some(12000),
            pixel_col: 320,
            confidence: 85,
            flags: 0b111, // Flash=1, Line=1, Thunder=1
            lat: Some(37.7749),
            lon: Some(-122.4194),
        },
        LightningEvent {
            event_id: "test-002".to_string(),
            sensor_id: 2,
            t_v: 1759090060,
            t_a: Some(1759090075),
            delta_ms: Some(15000),
            pixel_col: 410,
            confidence: 42,
            flags: 0b101, // Flash=1, Line=0, Thunder=1
            lat: Some(37.3382),
            lon: Some(-121.8863),
        },
        LightningEvent {
            event_id: "test-003".to_string(),
            sensor_id: 1,
            t_v: 1759090120,
            t_a: None, // 5-byte no-thunder variant
            delta_ms: None,
            pixel_col: 288,
            confidence: 95,
            flags: 0b011, // Flash=1, Line=1, Thunder=0
            lat: Some(38.5816),
            lon: Some(-121.4944),
        },
    ]
}

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
}

impl IngestActor {
    pub fn new(hserver: ActorHandle<SpaServerMsg>) -> Self {
        Self { hserver, events_seen: 0, quarantined: 0, last_event: None,
               sensors: std::collections::HashMap::new(), status_seen: 0 }
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

        info!("ingest store sensor={} dt={:?}ms pix={} conf={} flags={:#05b}",
            evt.sensor_id, evt.delta_ms, evt.pixel_col, evt.confidence, evt.flags);
        self.last_event = Some(evt);
        self.events_seen += 1;
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
            warn!("sensor {} battery critical", st.sensor_id);
        }

        match WsMsg::json(GspService::mod_path(), "status", &st) {
            Ok(ws_msg) => {
                if let Err(e) = self.hserver.try_send_msg(BroadcastWsMsg{ws_msg}) {
                    warn!("status broadcast failed: {}", e);
                }
            }
            Err(e) => warn!("status ws json failed: {}", e),
        }
        info!("ingest status sensor={} battery={:?} low_power={} battery_low={}",
            st.sensor_id, st.battery_pct, st.low_power, st.battery_low);
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
        // push hardcoded test data so the webpage has something to display
        for evt in test_events() {
            match evt.validate() {
                Ok(()) => self.store(evt),
                Err(e) => warn!("quarantine test event: {}", e),
            }
        }
    }
    LoraPacket => cont! {
        // Branch on Flash bit (byte 0, bit 0): 1 = lightning event, 0 = status.
        // TODO (gateway bridge): replace sensor_id/t_rx/lat/lon with DevEUI registry
        // lookup + gateway timestamp. Currently stubbed for local testing.
        if msg.0.is_empty() {
            self.quarantine("empty packet", &msg.0);
        } else {
            let t_rx = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            if msg.0[0] & 0x01 != 0 {
                match decode_lora(&msg.0, 0, t_rx, None, None) {
                    Ok(evt) => match evt.validate() {
                        Ok(()) => self.store(evt),
                        Err(e) => self.quarantine(&e.to_string(), &msg.0),
                    },
                    Err(e) => self.quarantine(&e.to_string(), &msg.0),
                }
            } else {
                match decode_status(&msg.0, 0, t_rx) {
                    Ok(st) => match st.validate() {
                        Ok(()) => self.store_status(st),
                        Err(e) => self.quarantine(&e.to_string(), &msg.0),
                    },
                    Err(e) => self.quarantine(&e.to_string(), &msg.0),
                }
            }
        }
    }
    SensorEvent => cont! {
        match msg.0.validate() {
            Ok(()) => self.store(msg.0),
            Err(e) => warn!("quarantine decoded event {}: {}", msg.0.event_id, e),
        }
    }
    SensorStatusMsg => cont! {
        // direct injection path for tests (mirrors SensorEvent).
        match msg.0.validate() {
            Ok(()) => self.store_status(msg.0),
            Err(e) => warn!("quarantine status sensor {}: {}", msg.0.sensor_id, e),
        }
    }
    SendCommand => cont! {
        // Downlink skeleton: encode + hand to the gateway bridge TX lane.
        // The bridge task (MQTT/serial, not yet spawned — see main.rs TODO) will
        // own the actual socket; this just validates + logs for now.
        // NOTE: SendCommand is a named-field struct, so fields live on `msg` directly.
        match msg.encode() {
            Ok(bytes) => {
                info!("command queued sensor={} {:?} ({} bytes, bridge TODO)",
                    msg.sensor_id, msg.cmd, bytes.len());
                // TODO: self.tx_to_bridge.send(bytes).await
            }
            Err(e) => warn!("bad command sensor {}: {}", msg.sensor_id, e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt5(flags: u8, conf: u8, pix: u16) -> Vec<u8> {
        let b = vec![flags, conf, (pix & 0xFF) as u8, (pix >> 8) as u8];
        let c = b.iter().fold(0u8, |a, x| a ^ x);
        [b, vec![c]].concat()
    }

    #[test]
    fn decode_5b_no_thunder() {
        let raw = pkt5(0b011, 95, 288);
        let e = decode_lora(&raw, 1, 1759090120, None, None).unwrap();
        assert_eq!(e.pixel_col, 288);
        assert_eq!((e.t_a, e.delta_ms), (None, None));
        e.validate().unwrap();
    }

    #[test]
    fn decode_9b_thunder() {
        let mut raw = pkt5(0b111, 85, 320);
        raw.pop(); // drop 5B checksum, append delta + new checksum
        raw.extend(12000u32.to_le_bytes());
        let c = raw.iter().fold(0u8, |a, x| a ^ x);
        raw.push(c);
        let e = decode_lora(&raw, 1, 1759090000, None, None).unwrap();
        assert_eq!(e.delta_ms, Some(12000));
        assert_eq!(e.t_a, Some(1759090012));
        e.validate().unwrap();
    }

    #[test]
    fn decode_status_lane() {
        // flags=0b100 (Flash=0, battery_low), battery 42%
        let b = vec![0b100, 42, 0, 0];
        let c = b.iter().fold(0u8, |a, x| a ^ x);
        let raw = [b, vec![c]].concat();
        let st = decode_status(&raw, 7, 1759090000).unwrap();
        assert_eq!(st.battery_pct, Some(42));
        assert!(st.battery_low);
        st.validate().unwrap();
    }

    #[test]
    fn reserved_bits_rejected() {
        let raw = pkt5(0b011 | 0x08, 95, 288); // reserved bit3 set
        assert!(decode_lora(&raw, 1, 1, None, None).is_err());
    }

    #[test]
    fn command_encode_window_bounds() {
        let ok = SendCommand { sensor_id: 1, cmd: SensorCommandKind::SetWindow, arg: 12 };
        assert_eq!(ok.encode().unwrap(), vec![0x04, 12]);
        let bad = SendCommand { sensor_id: 1, cmd: SensorCommandKind::SetWindow, arg: 31 };
        assert!(bad.encode().is_err());
    }
}
