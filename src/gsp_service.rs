use async_trait::async_trait;
use odin_actor::prelude::*;
use odin_server::prelude::*;
use odin_cesium::ImgLayerService;
use std::sync::{Arc, Mutex};

pub struct GspService{
      history: Arc<Mutex<Vec<crate::ingest::LightningEvent>>>,
}

impl GspService {
      pub fn new(history: Arc<Mutex<Vec<crate::ingest::LightningEvent>>>) -> Self {
      GspService { history }
  }
}
#[async_trait]
impl SpaService for GspService {
  fn add_dependencies(&self, b: SpaServiceList) -> SpaServiceList {
    b.add(build_service!(=> ImgLayerService::new())) // gets you Cesium globe
  }
  fn add_components(&self, spa: &mut SpaComponents) -> OdinServerResult<()> {
    spa.add_assets(self_crate!(), crate::load_asset);
    spa.add_module(asset_uri!("gsp_config.js"));
    spa.add_module(asset_uri!("gsp.js"));
    Ok(())
  }
  fn is_websocket(&self)->bool {
    true
  }
  async fn init_connection (&mut self, _hself: &ActorHandle<SpaServerMsg>, _is_data_available: bool, conn: &mut WsConnection) -> OdinServerResult<()> {
    // send hardcoded test data to each newly connected browser so the
    // webpage shows points immediately (broadcasts sent before a client
    // connects are lost, direct sends here are not)
    let past = self.history.lock().unwrap().clone();
    for evt in past {
        let msg = WsMsg::json(Self::mod_path(), "gsp", &evt)?;
        let _ = conn.send(msg).await;
    }
    Ok(())
  }
}