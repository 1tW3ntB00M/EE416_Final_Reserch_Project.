mod ingest;
mod gsp_service;

use gsp_service::GspService;
use odin_actor::prelude::*;
use odin_server::prelude::*;
use odin_build::{define_load_asset, define_load_config};

define_load_config! {}
define_load_asset! {}

run_actor_system!(actor_system => {
    let pre_server = PreActorHandle::new(&actor_system, "server", 64);
    let hserver = pre_server.to_actor_handle();
    let svc_list = SpaServiceList::new();
    let svc_list = svc_list.add(build_service!(=> GspService::new()));

    let _hserver = spawn_pre_actor!(actor_system, pre_server, SpaServer::new(
        odin_server::load_config("spa_server.ron")?,
        "live",
        svc_list
    ))?;

    let _h_ingest = spawn_actor!(actor_system, "ingest", ingest::IngestActor::new(hserver))?;
    Ok(())
});