use odin_actor::prelude::*;
use odin_server::prelude::*;
use serde::{Deserialize, Serialize};
use crate::gsp_service::GspService;

// --- 1. messages this actor can receive: one struct per mailbox "slot" ---
// Raw bytes as they arrive from the LoRa gateway (transport TBD - for now
// another task just does h_ingest.send_msg(LoraPacket(bytes)).await).
#[derive(Debug)]
pub struct LoraPacket{
    pub bytes: Vec<u8>,     // the 5/9 gsp-lora/v1 packet
    pub dev_eui: String,    // e.g. "70B3D57ED0000001"
    pub t_rx: i64           // gateway receive time, unix secs
}

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
}


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
    pub state: GspState, // classified display state (set at decode time via classify)
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    // sensor geometry from lora.ron (copied at decode time so the browser can
    // draw the FoV cone without a second lookup)
    pub bearing_deg: f32,
    pub fov_h_deg: f32,
    pub fov_v_deg: f32,
    pub nominal_range_m: u32,
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

#[derive(Debug)]
pub struct SensorStatusMsg(pub SensorStatus);

impl LoraConfig {
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

#[derive(Debug, Clone)]
pub struct ReceiveCommand {
    pub sensor_id: u16,
    pub cmd: SensorCommandKind,
    pub arg: u8
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

impl ReceiveCommand {
    /// inverse of `SendCommand::encode()`: parse the 2-byte downlink
    /// `[opcode, arg]`. `sensor_id` is NOT on the wire, so the caller supplies
    /// it from the DevEUI registry (same split as decode_lora/decode_status).
    pub fn decode(bytes: &[u8], sensor_id: u16) -> anyhow::Result<Self> {
        if bytes.len() != 2 {
            anyhow::bail!("command packet must be 2 bytes, got {}", bytes.len());
        }
        let (op, arg) = (bytes[0], bytes[1]);
        let cmd = match op {
            0x01 => SensorCommandKind::Sleep,
            0x02 => SensorCommandKind::Arm,
            0x03 => SensorCommandKind::Disarm,
            0x04 => {
                if !(1..=30).contains(&arg) {
                    anyhow::bail!("window must be 1-30s, got {}", arg);
                }
                SensorCommandKind::SetWindow
            }
            _ => anyhow::bail!("unknown command opcode {:#04x}", op),
        };
        Ok(ReceiveCommand { sensor_id, cmd, arg })
    }
}

// --- 2. message set: the full alphabet this actor understands ---
// (system msgs _Start_, _Terminate_, ... are added automatically)
define_actor_msg_set! {pub IngestMsg = LoraPacket | SensorEvent | SensorStatusMsg | SendCommand}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GspState { Candidate, Probable, Confirmed, UnlocatedCandidate }

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
    pub fn classify(&self) -> GspState {
        let line = self.flags & 0x02 != 0;
        let thunder = (self.flags & 0x04 != 0) && self.delta_ms.is_some(); // 9B, time-gated
        match (line, thunder) {
            (true, true) => GspState::Confirmed, // edge + acoustic + time-gated
            (false, false) => GspState::Candidate, // flash-only
            _ => GspState::Probable, // partial
        }
    }
}

// gsp-lora/v1 wire format (little-endian). Identity, time, position and geometry
// are NOT on air: sensor_id comes from the DevEUI registry map, t_v is gateway
// T_rx, and everything else comes from cfg — all passed in by the caller.
// 5-byte (no thunder): [0] flags | [1] confidence | [2..4] pixel_col u16 | [4] checksum (xor 0..4)
// 9-byte (thunder):    [0..4] same as above + [4..8] delta_ms u32 | [8] checksum (xor 0..8)
// flags: bit0 Flash (1=lightning, 0=status), bit1 Line, bit2 Thunder, bits3-7 reserved=0.
pub fn decode_lora(bytes: &[u8], sensor_id: u16, t_v: i64, cfg: &SensorConfig) -> anyhow::Result<LightningEvent> {
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
    let mut evt = LightningEvent {
        event_id: format!("{}-{}", sensor_id, t_v),
        sensor_id,
        t_v,
        t_a,
        delta_ms,
        pixel_col,
        confidence,
        flags,
        state: GspState::Candidate,
        lat: Some(cfg.lat),
        lon: Some(cfg.lon),
        bearing_deg: cfg.bearing_deg,
        fov_h_deg: cfg.fov_h_deg,
        fov_v_deg: cfg.fov_v_deg,
        nominal_range_m: cfg.nominal_range_m,
    };
    evt.state = evt.classify();
    Ok(evt)
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
            Err(e) => println!("ingest status ws json failed: {}", e),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt5(flags: u8, conf: u8, pix: u16) -> Vec<u8> {
        let b = vec![flags, conf, (pix & 0xFF) as u8, (pix >> 8) as u8];
        let c = b.iter().fold(0u8, |a, x| a ^ x);
        [b, vec![c]].concat()
    }

    fn test_cfg() -> SensorConfig {
        SensorConfig {
            dev_eui: "TEST".to_string(),
            lat: 37.0, lon: -122.0, alt: 10.0,
            bearing_deg: 270.0, fov_h_deg: 60.0, fov_v_deg: 40.0,
            nominal_range_m: 5000, armed: true, sensitivity: 1,
        }
    }

    #[test]
    fn decode_5b_no_thunder() {
        let raw = pkt5(0b011, 95, 288);
        let e = decode_lora(&raw, 1, 1759090120, &test_cfg()).unwrap();
        assert_eq!(e.pixel_col, 288);
        assert_eq!((e.t_a, e.delta_ms), (None, None));
        e.validate().unwrap();
        assert_eq!(e.state, GspState::Probable);
        assert_eq!((e.lat, e.lon), (Some(37.0), Some(-122.0)));
        assert_eq!(e.bearing_deg, 270.0);
    }

    #[test]
    fn decode_9b_thunder() {
        let mut raw = pkt5(0b111, 85, 320);
        raw.pop(); // drop 5B checksum, append delta + new checksum
        raw.extend(12000u32.to_le_bytes());
        let c = raw.iter().fold(0u8, |a, x| a ^ x);
        raw.push(c);
        let e = decode_lora(&raw, 1, 1759090000, &test_cfg()).unwrap();
        assert_eq!(e.delta_ms, Some(12000));
        assert_eq!(e.t_a, Some(1759090012));
        e.validate().unwrap();
        assert_eq!(e.state, GspState::Confirmed);
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
        assert!(decode_lora(&raw, 1, 1, &test_cfg()).is_err());
    }

    #[test]
    fn command_encode_window_bounds() {
        let ok = SendCommand { sensor_id: 1, cmd: SensorCommandKind::SetWindow, arg: 12 };
        assert_eq!(ok.encode().unwrap(), vec![0x04, 12]);
        let bad = SendCommand { sensor_id: 1, cmd: SensorCommandKind::SetWindow, arg: 31 };
        assert!(bad.encode().is_err());
    }

    #[test]
    fn command_decode_roundtrip() {
        let sent = SendCommand { sensor_id: 7, cmd: SensorCommandKind::SetWindow, arg: 12 };
        let bytes = sent.encode().unwrap();
        let recv = ReceiveCommand::decode(&bytes, sent.sensor_id).unwrap();
        assert_eq!(recv.sensor_id, 7);
        assert!(matches!(recv.cmd, SensorCommandKind::SetWindow));
        assert_eq!(recv.arg, 12);

        assert!(ReceiveCommand::decode(&[0xFF, 0], 1).is_err()); // unknown opcode
        assert!(ReceiveCommand::decode(&[0x01], 1).is_err());    // wrong length
        assert!(ReceiveCommand::decode(&[0x04, 31], 1).is_err()); // window out of range
    }
}
