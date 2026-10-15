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
let cones = new Map(); // sensor_id -> FoV cone entity

createIcon();
createWindow();
const eventView = initEventView();
odinCesium.setEntitySelectionHandler(gspSelection);
odinCesium.initLayerPanel("gsp", config, showGsp);
console.log("gsp initialized");

function getOrCreateCone(sensor_id) {
    return cones.get(sensor_id); // may be undefined: call ensureCone(evt) instead
}

// (Re)build the FoV wedge for an event, hierarchy set at construction time
// (post-hoc polygon.hierarchy assignment does not reliably update in Cesium).
// Cheap enough to recreate per event: ≤20 sensors, seconds between updates.
function ensureCone(evt) {
    if (evt.bearing_deg === undefined || evt.fov_h_deg === undefined ||
        evt.nominal_range_m === undefined) return undefined;
    const half = evt.fov_h_deg / 2;
    const [latL, lonL] = destPoint(evt.lat, evt.lon, evt.bearing_deg - half, evt.nominal_range_m);
    const [latR, lonR] = destPoint(evt.lat, evt.lon, evt.bearing_deg + half, evt.nominal_range_m);
    const positions = Cesium.Cartesian3.fromDegreesArray(
        [evt.lon, evt.lat, lonL, latL, lonR, latR]);
    console.log("cone update sensor=" + evt.sensor_id + " positions=" + positions.length +
        " state=" + evt.state);

    let cone = cones.get(evt.sensor_id);
    if (cone) dataSource.entities.remove(cone);
    // NOTE: wedge shows the LATEST event for this sensor (cones are per-sensor,
    // dots are per-event). Clicking the wedge selects that latest event.
    // (_evt assigned after construction: the Entity constructor ignores
    // unknown options, same as the dot entities below do with e._evt.)
    cone = dataSource.entities.add({
        id: "cone-" + evt.sensor_id,
        polygon: {
            hierarchy: new Cesium.PolygonHierarchy(positions),
            height: 0,
            // clamped to terrain: without this the wedge sits at ellipsoid
            // height 0, i.e. UNDER the terrain, and is invisible
            heightReference: Cesium.HeightReference.CLAMP_TO_GROUND,
            material: confColor(evt).withAlpha(0.3), // tint with state color
            outline: true,
            outlineColor: Cesium.Color.BLACK,
        }
    });
    cone._evt = evt;
    cones.set(evt.sensor_id, cone);
    return cone;
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

// --- Cesium entities (red dot for confirmed strikes, no uncertainty ellipse) ---

function confColor(evt) {
    if (evt.lat === undefined || evt.lon === undefined) return config.unlocatedColor;
    switch (evt.state) {
        case "Confirmed": return Cesium.Color.fromCssColorString('#FF0000'); // red dot
        case "Probable":  return Cesium.Color.fromCssColorString('#FFA500'); // orange
        default:          return Cesium.Color.fromCssColorString('#FFFF00'); // yellow
    }
}

function upsertEntity(evt) {
    const id = evt.event_id;
    let e = dataSource.entities.getById(id);
    const hasPos = evt.lat !== undefined && evt.lon !== undefined;
    // BR-F5-01: red dot only for CONFIRMED; CANDIDATE/PROBABLE are wedge-only.
    const showDot = hasPos && evt.state === "Confirmed";
    const pos = hasPos
        ? Cesium.Cartesian3.fromDegrees(evt.lon, evt.lat, 0)
        : Cesium.Cartesian3.fromDegrees(0, 0, 0); // unlocated: hidden until solved

    if (e) {
        e.position = pos;
        e.point.color = confColor(evt);
        e.show = showDot;
        e._evt = evt;
    } else {
        e = new Cesium.Entity({
            id,
            position: pos,
            show: showDot,
            point: {
                pixelSize: config.pointSize,
                color: confColor(evt),
                outlineColor: config.pointOutline,
                outlineWidth: config.outlineWidth,
                distanceDisplayCondition: config.pointDC,
            },
            description: describe(evt),
        });
        e._evt = evt;
        dataSource.entities.add(e);
    }
    if (hasPos) ensureCone(evt);
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

let healthBySensor = new Map(); // sensor_id -> latest status msg

// --- ws messages ---

function handleWsMessages(msgType, msg) {
    if (msgType === "gsp" || msgType === "event") {
        handleEvent(msg);
    } else if (msgType === "status") {
        handleStatus(msg);
    }
}

function handleStatus(st) {
    healthBySensor.set(st.sensor_id, st);
    // refresh detail view if it currently shows an event from this sensor
    if (selected && selected.sensor_id === st.sensor_id) {
        ui.setVarText(ui.getVarText("gsp.detail"), describe(selected));
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
    ui.setVarText(ui.getVarText("gsp.detail"), selected ? describe(selected) : null);
}

function zoomToEvent(event) {
    const lv = ui.getList(event);
    const evt = lv ? ui.getSelectedListItem(lv) : selected;
    if (evt && evt.lat !== undefined) {
        odinCesium.zoomTo(Cesium.Cartesian3.fromDegrees(evt.lon, evt.lat, config.zoomHeight));
        const e = dataSource.entities.getById(evt.event_id);
        if (e) odinCesium.setSelectedEntity(e);
    }
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

