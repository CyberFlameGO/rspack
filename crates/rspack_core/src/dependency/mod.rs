mod entry;
mod span;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::Relaxed;

pub use entry::*;
use once_cell::sync::Lazy;
use rspack_util::ext::AsAny;
use rustc_hash::FxHashSet as HashSet;
use serde::Serialize;
pub use span::SpanExt;
mod runtime_template;
pub use runtime_template::*;
mod runtime_requirements_dependency;
pub use runtime_requirements_dependency::RuntimeRequirementsDependency;
mod context_element_dependency;
mod dependency_macro;
pub use context_element_dependency::*;
use swc_core::ecma::atoms::JsWord;
mod const_dependency;
use std::{
  any::Any,
  fmt::{Debug, Display},
  hash::Hash,
};

pub use const_dependency::ConstDependency;
mod dependency_template;
pub use dependency_template::*;
use dyn_clone::{clone_trait_object, DynClone};

use crate::{
  ChunkGroupOptionsKindRef, ConnectionState, Context, ContextMode, ContextOptions, ErrorSpan,
  ExtendedReferencedExport, ModuleGraph, ModuleGraphConnection, ModuleIdentifier, ReferencedExport,
  RuntimeSpec,
};

// Used to describe dependencies' types, see webpack's `type` getter in `Dependency`
// Note: This is almost the same with the old `ResolveKind`
#[derive(Default, Clone, PartialEq, Eq, Hash, Debug)]
pub enum DependencyType {
  #[default]
  Unknown,
  ExportInfoApi,
  Entry,
  // Harmony import
  EsmImport,
  EsmImportSpecifier,
  // Harmony export
  EsmExport,
  EsmExportImportedSpecifier,
  EsmExportSpecifier,
  // import()
  DynamicImport,
  // cjs require
  CjsRequire,
  // new URL("./foo", import.meta.url)
  NewUrl,
  // new Worker()
  NewWorker,
  // import.meta.webpackHot.accept
  ImportMetaHotAccept,
  // import.meta.webpackHot.decline
  ImportMetaHotDecline,
  // module.hot.accept
  ModuleHotAccept,
  // module.hot.decline
  ModuleHotDecline,
  // css url()
  CssUrl,
  // css @import
  CssImport,
  // css modules compose
  CssCompose,
  // context element
  ContextElement,
  // import context
  ImportContext,
  // commonjs require context
  CommonJSRequireContext,
  // require.context
  RequireContext,
  // require.resolve
  RequireResolve,
  /// wasm import
  WasmImport,
  /// wasm export import
  WasmExportImported,
  /// static exports
  StaticExports,
  Custom(Box<str>), // TODO it will increase large layout size
}

impl Display for DependencyType {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      DependencyType::Unknown => write!(f, "unknown"),
      DependencyType::Entry => write!(f, "entry"),
      DependencyType::EsmImport => write!(f, "esm import"),
      DependencyType::EsmExport => write!(f, "esm export"),
      DependencyType::EsmExportSpecifier => write!(f, "esm export specifier"),
      DependencyType::EsmExportImportedSpecifier => write!(f, "esm export import specifier"),
      DependencyType::EsmImportSpecifier => write!(f, "esm import specifier"),
      DependencyType::DynamicImport => write!(f, "dynamic import"),
      DependencyType::CjsRequire => write!(f, "cjs require"),
      DependencyType::NewUrl => write!(f, "new URL()"),
      DependencyType::NewWorker => write!(f, "new Worker()"),
      DependencyType::ImportMetaHotAccept => write!(f, "import.meta.webpackHot.accept"),
      DependencyType::ImportMetaHotDecline => write!(f, "import.meta.webpackHot.decline"),
      DependencyType::ModuleHotAccept => write!(f, "module.hot.accept"),
      DependencyType::ModuleHotDecline => write!(f, "module.hot.decline"),
      DependencyType::CssUrl => write!(f, "css url"),
      DependencyType::CssImport => write!(f, "css import"),
      DependencyType::CssCompose => write!(f, "css compose"),
      DependencyType::ContextElement => write!(f, "context element"),
      DependencyType::ImportContext => write!(f, "import context"),
      DependencyType::CommonJSRequireContext => write!(f, "commonjs require context"),
      DependencyType::RequireContext => write!(f, "require.context"),
      DependencyType::RequireResolve => write!(f, "require.resolve"),
      DependencyType::WasmImport => write!(f, "wasm import"),
      DependencyType::WasmExportImported => write!(f, "wasm export imported"),
      DependencyType::StaticExports => write!(f, "static exports"),
      DependencyType::Custom(ty) => write!(f, "custom {ty}"),
      DependencyType::ExportInfoApi => write!(f, "export info api"),
    }
  }
}

#[derive(Default, Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum DependencyCategory {
  #[default]
  Unknown,
  Esm,
  CommonJS,
  Url,
  CssImport,
  CssCompose,
  Wasm,
  Worker,
}

impl From<&str> for DependencyCategory {
  fn from(value: &str) -> Self {
    match value {
      "esm" => Self::Esm,
      "commonjs" => Self::CommonJS,
      "url" => Self::Url,
      "wasm" => Self::Wasm,
      "css-import" => Self::CssImport,
      "css-compose" => Self::CssCompose,
      "worker" => Self::Worker,
      "unknown" => Self::Unknown,
      _ => unimplemented!("DependencyCategory {}", value),
    }
  }
}

impl Display for DependencyCategory {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      DependencyCategory::Unknown => write!(f, "unknown"),
      DependencyCategory::Esm => write!(f, "esm"),
      DependencyCategory::CommonJS => write!(f, "commonjs"),
      DependencyCategory::Url => write!(f, "url"),
      DependencyCategory::CssImport => write!(f, "css-import"),
      DependencyCategory::CssCompose => write!(f, "css-compose"),
      DependencyCategory::Wasm => write!(f, "wasm"),
      DependencyCategory::Worker => write!(f, "worker"),
    }
  }
}

pub trait Dependency:
  AsDependencyTemplate + AsModuleDependency + AsAny + DynClone + Send + Sync + Debug
{
  fn id(&self) -> &DependencyId;

  fn category(&self) -> &DependencyCategory {
    &DependencyCategory::Unknown
  }

  fn dependency_type(&self) -> &DependencyType {
    &DependencyType::Unknown
  }

  fn get_context(&self) -> Option<&Context> {
    None
  }

  fn get_exports(&self) -> Option<ExportsSpec> {
    None
  }
}

#[derive(Debug, Default)]
pub struct ExportSpec {
  pub name: JsWord,
  pub export: Option<Vec<JsWord>>,
  pub exports: Option<Vec<ExportNameOrSpec>>,
  pub can_mangle: Option<bool>,
  pub terminal_binding: Option<bool>,
  pub priority: Option<u8>,
  pub hidden: Option<bool>,
  pub from: Option<ModuleGraphConnection>,
  pub from_export: Option<ModuleGraphConnection>,
}

impl ExportSpec {
  pub fn new(name: String) -> Self {
    Self {
      name: JsWord::from(name),
      ..Default::default()
    }
  }
}

#[derive(Debug)]
pub enum ExportNameOrSpec {
  String(JsWord),
  ExportSpec(ExportSpec),
}

impl Default for ExportNameOrSpec {
  fn default() -> Self {
    Self::String(JsWord::default())
  }
}

#[derive(Debug, Default)]
pub enum ExportsOfExportsSpec {
  True,
  #[default]
  Null,
  Array(Vec<ExportNameOrSpec>),
}

#[derive(Debug, Default)]
#[allow(unused)]
pub struct ExportsSpec {
  pub exports: ExportsOfExportsSpec,
  pub priority: Option<u8>,
  pub can_mangle: Option<bool>,
  pub terminal_binding: Option<bool>,
  pub from: Option<ModuleGraphConnection>,
  pub dependencies: Option<Vec<ModuleIdentifier>>,
  pub hide_export: Option<Vec<JsWord>>,
  pub exclude_exports: Option<Vec<JsWord>>,
}

pub enum ExportsReferencedType {
  No,     // NO_EXPORTS_REFERENCED
  Object, // EXPORTS_OBJECT_REFERENCED
  String(Box<Vec<Vec<JsWord>>>),
  Value(Box<Vec<ReferencedExport>>),
}

impl From<JsWord> for ExportsReferencedType {
  fn from(value: JsWord) -> Self {
    ExportsReferencedType::String(Box::new(vec![vec![value]]))
  }
}

impl From<Vec<Vec<JsWord>>> for ExportsReferencedType {
  fn from(value: Vec<Vec<JsWord>>) -> Self {
    ExportsReferencedType::String(Box::new(value))
  }
}

impl From<Vec<JsWord>> for ExportsReferencedType {
  fn from(value: Vec<JsWord>) -> Self {
    ExportsReferencedType::String(Box::new(vec![value]))
  }
}

impl From<Vec<ReferencedExport>> for ExportsReferencedType {
  fn from(value: Vec<ReferencedExport>) -> Self {
    ExportsReferencedType::Value(Box::new(value))
  }
}

pub trait AsModuleDependency {
  fn as_module_dependency(&self) -> Option<&dyn ModuleDependency> {
    None
  }

  fn as_module_dependency_mut(&mut self) -> Option<&mut dyn ModuleDependency> {
    None
  }
}

impl<T: ModuleDependency> AsModuleDependency for T {
  fn as_module_dependency(&self) -> Option<&dyn ModuleDependency> {
    Some(self)
  }

  fn as_module_dependency_mut(&mut self) -> Option<&mut dyn ModuleDependency> {
    Some(self)
  }
}

pub type DependencyConditionFn = Box<dyn Function>;

pub trait Function:
  Fn(&ModuleGraphConnection, Option<&RuntimeSpec>, &ModuleGraph) -> ConnectionState + Send + Sync
{
  fn clone_boxed(&self) -> Box<dyn Function>;
}

/// Copy from https://github.com/rust-lang/rust/issues/24000#issuecomment-479425396
impl<T> Function for T
where
  T: 'static
    + Fn(&ModuleGraphConnection, Option<&RuntimeSpec>, &ModuleGraph) -> ConnectionState
    + Send
    + Sync
    + Clone,
{
  fn clone_boxed(&self) -> Box<dyn Function> {
    Box::new(self.clone())
  }
}

impl Clone for Box<dyn Function> {
  fn clone(&self) -> Self {
    self.clone_boxed()
  }
}

#[derive(Clone)]
pub enum DependencyCondition {
  False,
  Fn(DependencyConditionFn),
}

impl Debug for DependencyCondition {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      // Self::Nil => write!(f, "Nil"),
      Self::False => write!(f, "False"),
      Self::Fn(_) => write!(f, "Fn"),
    }
  }
}

pub trait ModuleDependency: Dependency {
  fn request(&self) -> &str;
  fn user_request(&self) -> &str;
  fn span(&self) -> Option<&ErrorSpan>;
  fn weak(&self) -> bool {
    false
  }
  fn set_request(&mut self, request: String);

  // TODO should split to `ModuleDependency` and `ContextDependency`
  fn options(&self) -> Option<&ContextOptions> {
    None
  }
  fn get_optional(&self) -> bool {
    false
  }

  // TODO: wired to place ChunkGroupOptions on dependency, should place on AsyncDependenciesBlock
  fn group_options(&self) -> Option<ChunkGroupOptionsKindRef> {
    None
  }

  fn get_condition(&self) -> Option<DependencyCondition> {
    None
  }

  fn get_module_evaluation_side_effects_state(
    &self,
    _module_graph: &ModuleGraph,
    _module_chain: &mut HashSet<ModuleIdentifier>,
  ) -> ConnectionState {
    ConnectionState::Bool(true)
  }

  fn get_referenced_exports(
    &self,
    _module_graph: &ModuleGraph,
    _runtime: Option<&RuntimeSpec>,
  ) -> Vec<ExtendedReferencedExport> {
    vec![ExtendedReferencedExport::Array(vec![])]
  }

  // an identifier to merge equal requests
  fn resource_identifier(&self) -> Option<&str> {
    None
  }
}

impl dyn Dependency + '_ {
  pub fn downcast_ref<D: Any>(&self) -> Option<&D> {
    self.as_any().downcast_ref::<D>()
  }

  pub fn downcast_mut<D: Any>(&mut self) -> Option<&mut D> {
    self.as_any_mut().downcast_mut::<D>()
  }
}

clone_trait_object!(Dependency);
clone_trait_object!(ModuleDependency);

pub type BoxModuleDependency = Box<dyn ModuleDependency>;
pub type BoxDependency = Box<dyn Dependency>;

pub fn is_async_dependency(dep: &dyn ModuleDependency) -> bool {
  if matches!(dep.dependency_type(), DependencyType::DynamicImport) {
    return true;
  }
  if matches!(dep.dependency_type(), DependencyType::NewWorker) {
    return true;
  }
  if matches!(dep.dependency_type(), DependencyType::ContextElement) {
    if let Some(options) = dep.options() {
      return matches!(options.mode, ContextMode::Lazy | ContextMode::LazyOnce);
    }
  }
  false
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub struct DependencyId(u32);

pub static DEPENDENCY_ID: Lazy<AtomicU32> = Lazy::new(|| AtomicU32::new(0));

impl DependencyId {
  pub fn new() -> Self {
    Self(DEPENDENCY_ID.fetch_add(1, Relaxed))
  }
}
impl Default for DependencyId {
  fn default() -> Self {
    Self::new()
  }
}

impl std::ops::Deref for DependencyId {
  type Target = u32;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl From<u32> for DependencyId {
  fn from(id: u32) -> Self {
    Self(id)
  }
}

// should move to rspack_plugin_javascript
pub mod needs_refactor {
  use once_cell::sync::Lazy;
  use regex::Regex;
  use swc_core::{
    common::{EqIgnoreSpan, Spanned, SyntaxContext, DUMMY_SP},
    ecma::{
      ast::{
        Expr, ExprOrSpread, Id, Ident, ImportDecl, Lit, MemberExpr, MemberProp, MetaPropExpr,
        MetaPropKind, ModuleExportName, NewExpr,
      },
      atoms::{js_word, JsWord},
      visit::Visit,
    },
  };

  use crate::SpanExt;

  pub fn match_new_url(new_expr: &NewExpr) -> Option<(u32, u32, String)> {
    fn is_import_meta_url(expr: &Expr) -> bool {
      static IMPORT_META: Lazy<Expr> = Lazy::new(|| {
        Expr::Member(MemberExpr {
          span: DUMMY_SP,
          obj: Box::new(Expr::MetaProp(MetaPropExpr {
            span: DUMMY_SP,
            kind: MetaPropKind::ImportMeta,
          })),
          prop: MemberProp::Ident(Ident {
            span: DUMMY_SP,
            sym: "url".into(),
            optional: false,
          }),
        })
      });
      Ident::within_ignored_ctxt(|| expr.eq_ignore_span(&IMPORT_META))
    }

    if matches!(&*new_expr.callee, Expr::Ident(Ident { sym: js_word!("URL"), .. }))
    && let Some(args) = &new_expr.args
    && let (Some(first), Some(second)) = (args.first(), args.get(1))
    && let (
      ExprOrSpread { spread: None, expr: box Expr::Lit(Lit::Str(path)) },
      ExprOrSpread { spread: None, expr: box expr },
    ) = (first, second) && is_import_meta_url(expr) {
      return Some((path.span.real_lo(), expr.span().real_hi(), path.value.to_string()))
    }
    None
  }

  static WORKER_FROM_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(.+?)(\(\))?\s+from\s+(.+)$").expect("invalid regex"));

  #[derive(Debug, Default)]
  pub struct WorkerSyntaxList {
    variables: Vec<WorkerSyntax>,
    globals: Vec<WorkerSyntax>,
  }

  impl WorkerSyntaxList {
    pub fn push(&mut self, syntax: WorkerSyntax) {
      if syntax.ctxt.is_some() {
        self.variables.push(syntax);
      } else {
        self.globals.push(syntax);
      }
    }

    fn find_worker_syntax(&self, ident: &Ident) -> Option<&WorkerSyntax> {
      (self.variables.iter().chain(self.globals.iter())).find(|s| s.matches(ident))
    }

    pub fn match_new_worker(&self, new_expr: &NewExpr) -> bool {
      matches!(&*new_expr.callee, Expr::Ident(ident) if self.find_worker_syntax(ident).is_some())
    }
  }

  impl Extend<WorkerSyntax> for WorkerSyntaxList {
    fn extend<T: IntoIterator<Item = WorkerSyntax>>(&mut self, iter: T) {
      for i in iter {
        self.push(i);
      }
    }
  }

  impl From<WorkerSyntaxScanner<'_>> for WorkerSyntaxList {
    fn from(value: WorkerSyntaxScanner) -> Self {
      value.result
    }
  }

  #[derive(Debug, PartialEq, Eq)]
  pub struct WorkerSyntax {
    word: JsWord,
    ctxt: Option<SyntaxContext>,
  }

  impl WorkerSyntax {
    pub fn new(word: JsWord, ctxt: Option<SyntaxContext>) -> Self {
      Self { word, ctxt }
    }

    pub fn matches(&self, ident: &Ident) -> bool {
      if let Some(ctxt) = self.ctxt {
        let (word, id_ctxt) = ident.to_id();
        word == self.word && id_ctxt == ctxt
      } else {
        self.word == ident.sym
      }
    }
  }

  pub struct WorkerSyntaxScanner<'a> {
    result: WorkerSyntaxList,
    caps: Vec<(&'a str, &'a str)>,
  }

  pub const DEFAULT_WORKER_SYNTAX: &[&str] =
    &["Worker", "SharedWorker", "Worker from worker_threads"];

  impl<'a> WorkerSyntaxScanner<'a> {
    pub fn new(syntax: &'a [&'a str]) -> Self {
      let mut result = WorkerSyntaxList::default();
      let mut caps = Vec::new();
      for s in syntax {
        if let Some(captures) = WORKER_FROM_REGEX.captures(s)
        && let Some(ids) = captures.get(1)
        && let Some(source) = captures.get(3) {
          caps.push((ids.as_str(), source.as_str()));
        } else {
          result.push(WorkerSyntax::new(JsWord::from(*s), None))
        }
      }
      Self { result, caps }
    }
  }

  impl Visit for WorkerSyntaxScanner<'_> {
    fn visit_import_decl(&mut self, decl: &ImportDecl) {
      let source = &*decl.src.value;
      let found = self
        .caps
        .iter()
        .filter(|cap| cap.1 == source)
        .flat_map(|cap| {
          if cap.0 == "default" {
            decl
              .specifiers
              .iter()
              .filter_map(|spec| spec.as_default())
              .map(|spec| spec.local.to_id())
              .collect::<Vec<Id>>()
          } else {
            decl
              .specifiers
              .iter()
              .filter_map(|spec| {
                spec.as_named().filter(|named| {
                  if let Some(imported) = &named.imported {
                    let s = match imported {
                      ModuleExportName::Ident(s) => &s.sym,
                      ModuleExportName::Str(s) => &s.value,
                    };
                    s == cap.0
                  } else {
                    &*named.local.sym == cap.0
                  }
                })
              })
              .map(|spec| spec.local.to_id())
              .collect::<Vec<Id>>()
          }
        })
        .map(|pair| WorkerSyntax::new(pair.0, Some(pair.1)));
      self.result.extend(found);
    }
  }
}
