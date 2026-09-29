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

createIcon();
createWindow();
const eventView = initEventView();
odinCesium.setEntitySelectionHandler(gspSelection);
odinCesium.initLayerPanel("gsp", config, showGsp);
console.log("gsp initialized");

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
    return evt.confidence >= config.minConfidence ? config.highConfColor : config.lowConfColor;
}

function upsertEntity(evt) {
    const id = evt.event_id;
    let e = dataSource.entities.getById(id);
    const hasPos = evt.lat !== undefined && evt.lon !== undefined;
    const pos = hasPos
        ? Cesium.Cartesian3.fromDegrees(evt.lon, evt.lat, 0)
        : Cesium.Cartesian3.fromDegrees(0, 0, 0); // unlocated: hidden until solved

    if (e) {
        e.position = pos;
        e.point.color = confColor(evt);
        e.show = hasPos ? true : false;
        e._evt = evt;
    } else {
        e = new Cesium.Entity({
            id,
            position: pos,
            show: hasPos,
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
    odinCesium.requestRender();
}

function describe(evt) {
    const t = new Date(evt.t_v * 1000).toISOString();
    return `sensor ${evt.sensor_id}<br>Tv ${t}<br>dt ${evt.delta_ms}ms<br>pix ${evt.pixel_col} conf ${evt.confidence}`;
}

// --- ws messages ---

function handleWsMessages(msgType, msg) {
    if (msgType === "gsp" || msgType === "event") {
        handleEvent(msg);
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
