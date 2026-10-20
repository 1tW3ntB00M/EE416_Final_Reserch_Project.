mod bridge;
mod ingest;
mod gsp_service;

use gsp_service::GspService;
use odin_actor::prelude::*;
use odin_server::prelude::*;
use odin_build::{define_load_asset, define_load_config};
use std::sync::{Arc, Mutex};

define_load_config! {}
define_load_asset! {}

run_actor_system!(actor_system => {
    let lora: ingest::LoraConfig = crate::load_config("lora.ron")?;
    let history: Arc<Mutex<Vec<ingest::LightningEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let pre_server = PreActorHandle::new(&actor_system, "server", 64);
    let hserver = pre_server.to_actor_handle();
    let svc_list = SpaServiceList::new();
    let svc_history = history.clone();
    let gsp_lora = lora.clone();
    let svc_list = svc_list.add(build_service!(=> GspService::new(svc_history, gsp_lora)));
    let (tx_bridge, rx_bridge) = create_mpsc_sender_receiver::<(String, Vec<u8>)>(64);

    let _hserver = spawn_pre_actor!(actor_system, pre_server, SpaServer::new(
        odin_server::load_config("spa_server.ron")?,
        "live",
        svc_list
    ))?;

    let h_ingest = spawn_actor!(actor_system, "ingest", ingest::IngestActor::new(hserver, history.clone(), lora.clone(), tx_bridge))?;

    // LoRa gateway bridge. Simulated feed for now (no hardware needed) —
    // swap for bridge::run_mqtt once the field gateway + lora.ron exist.
    tokio::spawn(bridge::run_mqtt(h_ingest, lora));
    Ok(())
});