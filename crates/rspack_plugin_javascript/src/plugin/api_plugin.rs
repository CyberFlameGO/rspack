use rspack_core::{
  InitFragmentStage, NormalInitFragment, Plugin, PluginContext, PluginRenderModuleContentOutput,
  RenderModuleContentArgs,
};

#[derive(Debug)]
pub struct APIPlugin;

impl Plugin for APIPlugin {
  fn render_module_content<'a>(
    &'a self,
    _ctx: PluginContext,
    mut args: RenderModuleContentArgs<'a>,
  ) -> PluginRenderModuleContentOutput<'a> {
    if let Some(build_info) = &args.module_graph_module.build_info && build_info.need_create_require {
      args.chunk_init_fragments
        .entry("external module node-commonjs".to_string())
        .or_insert(NormalInitFragment::new(
          "import { createRequire as __WEBPACK_EXTERNAL_createRequire } from 'module';\n"
            .to_string(),
          InitFragmentStage::StageHarmonyImports,
          None,
        ));
    }
    Ok(args)
  }
}
