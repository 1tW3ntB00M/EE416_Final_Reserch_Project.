use odin_actor::prelude::*;
use odin_server::prelude::*;

run_actor_system!(actor_system => {
    let pre_server = PreActorHandle::new( &actor_system, "server", 64);
    let svc_list = SpaServiceList::new();

    let _hserver = spawn_pre_actor!( actor_system, pre_server, SpaServer::new(
        odin_server::load_config("spa_server.ron")?,
        "live",
        svc_list
    ))?;
    Ok(())
});
