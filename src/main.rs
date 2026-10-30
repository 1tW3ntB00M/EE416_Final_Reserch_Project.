mod bridge;
mod forecast;
mod ingest;
mod gsp_service;
mod store;

use gsp_service::GspService;
use odin_actor::prelude::*;
use odin_server::prelude::*;
use odin_build::{define_load_asset, define_load_config};

define_load_config! {}
define_load_asset! {}

run_actor_system!(actor_system => {
    let lora: ingest::LoraConfig = crate::load_config("lora.ron")?;
    std::fs::create_dir_all("data")?;
    let pool = store::init_db("sqlite://data/gsp.db?mode=rwc").await?;

    let pre_server = PreActorHandle::new(&actor_system, "server", 64);
    let hserver = pre_server.to_actor_handle();
    let svc_list = SpaServiceList::new();
    let pool_svc = pool.clone();
    let lora_svc = lora.clone();
    let svc_list = svc_list.add(build_service!(=> GspService::new(pool_svc, lora_svc)));
    let (tx_bridge, rx_bridge) = create_mpsc_sender_receiver::<(String, Vec<u8>)>(64);

    let _hserver = spawn_pre_actor!(actor_system, pre_server, SpaServer::new(
        odin_server::load_config("spa_server.ron")?,
        "live",
        svc_list
    ))?;

    let h_ingest = spawn_actor!(actor_system, "ingest", ingest::IngestActor::new(hserver, pool.clone(), lora.clone(), tx_bridge))?;
    let h_forecast = h_ingest.clone();

    // LoRa gateway bridge: uplink (ChirpStack -> IngestActor) + downlink (IF-002).
    tokio::spawn(bridge::run_mqtt(h_ingest, lora.clone(), rx_bridge));

    // Forecast-driven arming (FUN-F4-002/003, IF-003): open-meteo CAPE -> Arm/Disarm.
    let forecast_cfg: forecast::ForecastConfig = crate::load_config("forecast.ron")?;
    tokio::spawn(forecast::run_forecast(h_forecast, lora, forecast_cfg));
    Ok(())
});