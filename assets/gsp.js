/*
 * GSP Cesium client - served by odin_server SpaService.
 * Rust side must send: WsMsg::json(GspService::mod_path(), "gsp", payload)
 * where payload is a LightningEvent + optional lat/lon for confirmed strikes.
 */
import { config } from "./gsp_config.js";

import * as main from "../odin_server/main.js";
import * as util from "../odin_server/ui_util.js";
import * as ui from "../odin_server/ui.js";
import * as ws from "../odin_server/ws.js";
import * as odinCesium from "../odin_cesium/odin_cesium.js";

// MUST match Rust: GspService::mod_path(), e.g. "odin_gsp::gsp_service::GspService"
const MOD_PATH = "odin_gsp::gsp_service::GspService";

ws.addWsHandler(MOD_PATH, handleWsMessages);

let dataSource = new Cesium.CustomDataSource("gsp");
odinCesium.addDataSource(dataSource);

let events = []; // newest last
let selected = undefined;
let sensors = new Map(); // sensor_id -> SensorInfo registry (persistent)
let sensorDots = new Map(); // sensor_id -> blue dot entity (always visible)
let cones = new Map(); // sensor_id -> FoV cone entity (grey baseline, state-tinted)

createIcon();
createWindow();
const eventView = initEventView();
odinCesium.setEntitySelectionHandler(gspSelection);
odinCesium.initLayerPanel("gsp", config, showGsp);
console.log("gsp initialized");

// --- persistent sensors: blue dot + grey baseline cone, always visible ---

function sensorFromEvent(evt) {
    // LightningEvent carries sensor geometry (copied from lora.ron at decode
    // time); evt.lat/lon is the sensor position (strike estimate is derived
    // separately in strikePos()). Returns undefined if geometry is missing.
    if (evt.lat === undefined || evt.lon === undefined ||
        evt.bearing_deg === undefined || evt.fov_h_deg === undefined ||
        evt.nominal_range_m === undefined) return undefined;
    return {
        sensor_id: evt.sensor_id,
        lat: evt.lat,
        lon: evt.lon,
        alt: 0,
        bearing_deg: evt.bearing_deg,
        fov_h_deg: evt.fov_h_deg,
        fov_v_deg: evt.fov_v_deg,
        nominal_range_m: evt.nominal_range_m,
    };
}

function ensureSensorEntities(sensor) {
    if (!sensor || sensor.lat === undefined || sensor.lon === undefined) return;
    sensors.set(sensor.sensor_id, sensor);
    // blue dot at sensor location (never recolored by event state)
    if (!sensorDots.has(sensor.sensor_id)) {
        const pos = Cesium.Cartesian3.fromDegrees(sensor.lon, sensor.lat, 0);
        const dot = dataSource.entities.add({
            id: "sensor-" + sensor.sensor_id,
            position: pos,
            point: {
                pixelSize: config.sensorPointSize,
                color: config.sensorColor,
                outlineColor: config.sensorOutline,
                outlineWidth: config.sensorOutlineWidth,
                distanceDisplayCondition: config.pointDC,
                heightReference: Cesium.HeightReference.CLAMP_TO_GROUND,
                disableDepthTestDistance: Number.POSITIVE_INFINITY,
            },
            label: {
                text: String(sensor.sensor_id),
                scale: 0.8,
                horizontalOrigin: Cesium.HorizontalOrigin.LEFT,
                verticalOrigin: Cesium.VerticalOrigin.TOP,
                pixelOffset: new Cesium.Cartesian2(8, -8),
                fillColor: config.sensorColor,
                showBackground: false,
                disableDepthTestDistance: Number.POSITIVE_INFINITY,
            },
        });
        dot._sensorId = sensor.sensor_id;
        sensorDots.set(sensor.sensor_id, dot);
    }
    // grey baseline cone if none exists yet for this sensor
    if (!cones.has(sensor.sensor_id)) {
        updateCone(sensor, config.coneIdleColor, undefined);
    }
    odinCesium.requestRender();
}

function handleSensors(list) {
    console.log("gsp sensors received: " + (Array.isArray(list) ? list.length : typeof list));
    if (!Array.isArray(list)) return;
    for (const s of list) ensureSensorEntities(s);
    console.log("gsp sensor dots: " + sensorDots.size + " cones: " + cones.size);
}

function coneColorForState(state) {
    switch (state) {
        case "Confirmed": return config.coneConfirmedColor;
        case "Probable": return config.coneProbableColor;
        case "Candidate": return config.coneCandidateColor;
        default: return config.coneIdleColor;
    }
}

function buildConePositions(lat, lon, bearing_deg, fov_h_deg, range_m) {
    // Circular sector (not a triangle): a centerline strike at full range
    // would otherwise fall past the triangle's straight base edge.
    const half = fov_h_deg / 2;
    const segs = 32;
    const flat = [lon, lat]; // apex first, then arc edge-to-edge
    for (let i = 0; i <= segs; i++) {
        const b = bearing_deg - half + (fov_h_deg * i) / segs;
        const [la, lo] = destPoint(lat, lon, b, range_m);
        flat.push(lo, la);
    }
    return Cesium.Cartesian3.fromDegreesArray(flat);
}

function updateCone(sensor, color, evt) {
    const positions = buildConePositions(
        sensor.lat, sensor.lon, sensor.bearing_deg, sensor.fov_h_deg, sensor.nominal_range_m);
    console.log("cone update sensor=" + sensor.sensor_id + " positions=" + positions.length +
        " state=" + (evt ? evt.state : "idle"));
    const old = cones.get(sensor.sensor_id);
    if (old) dataSource.entities.remove(old);
    // NOTE: wedge shows the LATEST event for this sensor (cones are per-sensor,
    // dots are per-event). Clicking the wedge selects that latest event.
    const cone = dataSource.entities.add({
        id: "cone-" + sensor.sensor_id,
        polygon: {
            hierarchy: new Cesium.PolygonHierarchy(positions),
            height: 0,
            // clamped to terrain: without this the wedge sits at ellipsoid
            // height 0, i.e. UNDER the terrain, and is invisible
            heightReference: Cesium.HeightReference.CLAMP_TO_GROUND,
            material: color.withAlpha(config.coneAlpha),
            outline: true,
            outlineColor: Cesium.Color.BLACK,
        }
    });
    if (evt) cone._evt = evt;
    cones.set(sensor.sensor_id, cone);
    return cone;
}

// Strike position for a Confirmed event: backend-provided strike_lat/lon wins
// when present; otherwise estimate along the pixel-derived azimuth at
// nominal_range so the red dot lands inside the red cone.
function strikePos(evt, sensor) {
    if (evt.strike_lat !== undefined && evt.strike_lon !== undefined) {
        return Cesium.Cartesian3.fromDegrees(evt.strike_lon, evt.strike_lat, 0);
    }
    if (sensor && sensor.lat !== undefined && sensor.bearing_deg !== undefined) {
        const cols = config.sensorCols || 640;
        const pix = Math.min(Math.max(evt.pixel_col || 0, 0), cols);
        const az = sensor.bearing_deg - sensor.fov_h_deg / 2 + (pix / cols) * sensor.fov_h_deg;
        // Estimated range pulls slightly inside the sector arc (x0.92) so the
        // dot reads as inside, not straddling the edge. Explicit backend
        // strike_lat/lon or strike_range_m above are honored verbatim.
        const range = (evt.strike_range_m !== undefined ? evt.strike_range_m : sensor.nominal_range_m) * 0.92;
        const [sLat, sLon] = destPoint(sensor.lat, sensor.lon, az, range);
        return Cesium.Cartesian3.fromDegrees(sLon, sLat, 0);
    }
    if (evt.lat !== undefined && evt.lon !== undefined) {
        return Cesium.Cartesian3.fromDegrees(evt.lon, evt.lat, 0);
    }
    return Cesium.Cartesian3.fromDegrees(0, 0, 0);
}

function getOrCreateCone(sensor_id) {
    return cones.get(sensor_id); // may be undefined: call updateCone/ensureSensorEntities instead
}

// (Re)build the FoV wedge for an event, hierarchy set at construction time
// (post-hoc polygon.hierarchy assignment does not reliably update in Cesium).
// Cheap enough to recreate per event: ≤20 sensors, seconds between updates.
function ensureCone(evt) {
    const sensor = sensors.get(evt.sensor_id) || sensorFromEvent(evt);
    if (sensor) ensureSensorEntities(sensor);
    const s = sensors.get(evt.sensor_id);
    if (!s) return undefined;
    return updateCone(s, coneColorForState(evt.state), evt);
}

// great-circle destination: point distM meters from (lat,lon) along bearingDeg
function destPoint(latDeg, lonDeg, bearingDeg, distM) {
    const R = 6371000, d = distM / R, br = bearingDeg * Math.PI / 180;
    const la1 = latDeg * Math.PI / 180, lo1 = lonDeg * Math.PI / 180;
    const la2 = Math.asin(Math.sin(la1) * Math.cos(d) + Math.cos(la1) * Math.sin(d) * Math.cos(br));
    const lo2 = lo1 + Math.atan2(Math.sin(br) * Math.sin(d) * Math.cos(la1),
                                Math.cos(d) - Math.sin(la1) * Math.sin(la2));
    return [la2 * 180 / Math.PI, lo2 * 180 / Math.PI];
}

// setConeHierarchy: superseded by ensureCone() above (kept for reference).
// Post-hoc `cone.polygon.hierarchy = ...` assignment does not reliably update;
// always (re)build with hierarchy at construction time instead.

function createIcon() {
    return ui.Icon(
        "./asset/odin-gsp/lightning-icon.svg",
        (e) => ui.toggleWindow(e, "gsp"),
        "GSP lightning strikes",
    );
}

function createWindow() {
    return ui.Window("Lightning GSP", "gsp", "./asset/odin-gsp/lightning-icon.svg")(
        ui.LayerPanel("gsp", toggleShowGsp),
        ui.Panel("events", true)(
            ui.RowContainer()(
                ui.CheckBox("follow latest", toggleFollowLatest, "gsp.followLatest"),
            ),
            ui.List("gsp.events", 10, selectEvent, null, null, zoomToEvent),
        ),
        ui.Panel("detail", true)(
            ui.VarText(null, "gsp.detail", 0, 0, { isFixed: true }),
        ),
    );
}

function initEventView() {
    const view = ui.getList("gsp.events");
    if (view) {
        ui.setListItemDisplayColumns(view, ["fit", "header"], [
            { name: "sensor", tip: "sensor id", width: "3rem", attrs: [], map: (e) => String(e.sensor_id) },
            { name: "time", tip: "optical trigger UTC", width: "8rem", attrs: ["fixed", "alignRight"], map: (e) => util.toLocalMDHMString(e.t_v * 1000) },
            { name: "dt", tip: "Ta-Tv ms", width: "4rem", attrs: ["fixed", "alignRight"], map: (e) => String(e.delta_ms) },
            { name: "conf", tip: "confidence 0-100", width: "3rem", attrs: ["fixed", "alignRight"], map: (e) => String(e.confidence) },
        ]);
    }
    return view;
}

// --- Cesium entities: blue sensor dot (always), grey/yellow/orange/red cone,
// red strike dot only for Confirmed ---

function strikeDotColor() {
    return config.strikeColor;
}

function upsertEntity(evt) {
    // 1. make sure the persistent sensor dot + cone exist, then tint the cone
    // by event state: grey idle, yellow candidate, orange probable, red confirmed.
    const derived = sensorFromEvent(evt);
    if (derived && !sensors.has(evt.sensor_id)) ensureSensorEntities(derived);
    const sensor = sensors.get(evt.sensor_id) || derived;
    if (sensor) updateCone(sensor, coneColorForState(evt.state), evt);

    // 2. strike dot: red dot inside the cone, Confirmed only.
    const id = evt.event_id;
    let e = dataSource.entities.getById(id);
    const isConfirmed = evt.state === "Confirmed";
    const hasPos = isConfirmed && sensor !== undefined;
    const pos = hasPos ? strikePos(evt, sensor) : Cesium.Cartesian3.fromDegrees(0, 0, 0);

    if (e) {
        e.position = pos;
        e.show = isConfirmed;
        e._evt = evt;
    } else {
        e = new Cesium.Entity({
            id,
            position: pos,
            show: isConfirmed,
            point: {
                pixelSize: config.pointSize,
                color: strikeDotColor(),
                outlineColor: config.pointOutline,
                outlineWidth: config.outlineWidth,
                distanceDisplayCondition: config.pointDC,
                heightReference: Cesium.HeightReference.CLAMP_TO_GROUND,
                disableDepthTestDistance: Number.POSITIVE_INFINITY,
            },
            description: describe(evt),
        });
        e._evt = evt;
        dataSource.entities.add(e);
    }
    odinCesium.requestRender();
}

function describe(evt) {
    const t = new Date(evt.t_v * 1000).toISOString();
    const h = healthBySensor.get(evt.sensor_id);
    let batt = "";
    if (h) {
        const pct = (h.battery_pct === null || h.battery_pct === undefined) ? "?" : h.battery_pct + "%";
        batt = `<br>batt ${pct}${h.battery_low ? " LOW" : ""}${h.low_power ? " (low-power)" : ""}`;
    }
    return `sensor ${evt.sensor_id}<br>Tv ${t}<br>dt ${evt.delta_ms}ms<br>pix ${evt.pixel_col} conf ${evt.confidence}${batt}`;
}

// Plain-text variant for the VarText detail panel (single-line, no HTML).
function describeText(evt) {
    return describe(evt).replace(/<br>/g, " | ");
}

let healthBySensor = new Map(); // sensor_id -> latest status msg

// --- ws messages ---

function handleWsMessages(msgType, msg) {
    if (msgType === "sensors") {
        handleSensors(msg);
    } else if (msgType === "gsp" || msgType === "event") {
        handleEvent(msg);
    } else if (msgType === "status") {
        handleStatus(msg);
    }
}

function handleStatus(st) {
    healthBySensor.set(st.sensor_id, st);
    // refresh detail view if it currently shows an event from this sensor
    if (selected && selected.sensor_id === st.sensor_id) {
        ui.setVarText(ui.getVarText("gsp.detail"), describeText(selected));
    }
}

function handleEvent(evt) {
    events.push(evt);
    if (events.length > 200) events.shift(); // FUN-F6-004 cap
    upsertEntity(evt);
    ui.setListItems(eventView, [...events].reverse());
    if (config.followLatest) ui.selectFirstListItem(eventView);
}

// --- ui slots ---

function selectEvent(event) {
    selected = event.detail.curSelection;
    ui.setVarText(ui.getVarText("gsp.detail"), selected ? describeText(selected) : null);
}

function zoomToEvent(event) {
    const lv = ui.getList(event);
    const evt = lv ? ui.getSelectedListItem(lv) : selected;
    if (!evt) return;
    const sensor = sensors.get(evt.sensor_id) || sensorFromEvent(evt);
    let target;
    if (evt.state === "Confirmed" && sensor) {
        target = strikePos(evt, sensor);
    } else if (sensor) {
        target = Cesium.Cartesian3.fromDegrees(sensor.lon, sensor.lat, config.zoomHeight);
        odinCesium.zoomTo(target);
        const cone = cones.get(evt.sensor_id);
        if (cone) odinCesium.setSelectedEntity(cone);
        return;
    } else if (evt.lat !== undefined) {
        target = Cesium.Cartesian3.fromDegrees(evt.lon, evt.lat, config.zoomHeight);
    } else {
        return;
    }
    // raise the strike target to zoom height for the camera move
    const carto = Cesium.Cartographic.fromCartesian(target);
    target = Cesium.Cartesian3.fromRadians(carto.longitude, carto.latitude, config.zoomHeight);
    odinCesium.zoomTo(target);
    const e = dataSource.entities.getById(evt.event_id);
    if (e) odinCesium.setSelectedEntity(e);
}

function toggleShowGsp(event) {
    const cb = ui.getCheckBox(event.target);
    if (cb) showGsp(ui.isCheckBoxSelected(cb));
}

function showGsp(cond) {
    dataSource.show = cond;
    odinCesium.requestRender();
}

function toggleFollowLatest(event) {
    const cb = ui.getCheckBox(event.target);
    if (cb) config.followLatest = ui.isCheckBoxSelected(cb);
}

function gspSelection() {
    const sel = odinCesium.getSelectedEntity();
    if (sel && sel._evt) ui.setSelectedListItem(eventView, sel._evt);
}
