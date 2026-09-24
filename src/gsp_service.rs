use async_trait::async_trait;
use odin_server::prelude::*;
use odin_cesium::ImgLayerService;

pub struct GspService;
impl GspService {
    pub fn new() -> Self { GspService }
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
}