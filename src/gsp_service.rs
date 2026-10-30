use async_trait::async_trait;
use odin_actor::prelude::*;
use odin_server::prelude::*;
use odin_cesium::ImgLayerService;

pub struct GspService{
      pool: sqlx::SqlitePool,
      lora: crate::ingest::LoraConfig,
}

impl GspService {
      pub fn new(pool: sqlx::SqlitePool, lora: crate::ingest::LoraConfig) -> Self {
      GspService { pool, lora }
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
    // 1. registered sensors first so the browser can draw persistent blue
    // dots + grey baseline FOV cones before any event arrives.
    let sensors = self.lora.sensor_list();
    let past = crate::store::recent_events(&self.pool, 200).await.unwrap_or_default();
    println!("gsp init_connection: sending {} sensors + {} history events", sensors.len(), past.len());
    let msg = WsMsg::json(Self::mod_path(), "sensors", &sensors)?;
    let _ = conn.send(msg).await;
    // 2. replay recent events so the webpage shows points immediately
    // (broadcasts sent before a client connects are lost, direct sends here are not)
    for evt in past {
        let msg = WsMsg::json(Self::mod_path(), "gsp", &evt)?;
        let _ = conn.send(msg).await;
    }
    Ok(())
  }
}