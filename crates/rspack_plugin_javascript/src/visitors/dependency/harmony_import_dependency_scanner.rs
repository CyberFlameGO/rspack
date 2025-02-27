use indexmap::IndexMap;
use rspack_core::{
  tree_shaking::symbol::DEFAULT_JS_WORD, BoxDependency, BoxDependencyTemplate, BuildInfo,
  ConstDependency, DependencyType, SpanExt,
};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use swc_core::{
  common::Span,
  ecma::{
    ast::{
      AssignExpr, AssignOp, Callee, ExportAll, ExportSpecifier, Expr, Id, Ident, ImportDecl,
      ImportSpecifier, Lit, MemberExpr, MemberProp, ModuleExportName, NamedExport, Pat, PatOrExpr,
      Program, Prop,
    },
    atoms::JsWord,
    visit::{noop_visit_type, Visit, VisitWith},
  },
};

use super::collect_destructuring_assignment_properties;
use crate::dependency::{
  HarmonyExportImportedSpecifierDependency, HarmonyImportDependency,
  HarmonyImportSpecifierDependency, Specifier,
};

pub struct ImporterReferenceInfo {
  pub request: JsWord,
  pub specifier: Specifier,
  pub names: Option<JsWord>,
}

impl ImporterReferenceInfo {
  pub fn new(request: JsWord, specifier: Specifier, names: Option<JsWord>) -> Self {
    Self {
      request,
      specifier,
      names,
    }
  }
}

pub type ImportMap = HashMap<Id, ImporterReferenceInfo>;

pub struct ImporterInfo {
  pub span: Span,
  pub specifiers: Vec<Specifier>,
  pub exports_all: bool,
}

impl ImporterInfo {
  pub fn new(span: Span, specifiers: Vec<Specifier>, exports_all: bool) -> Self {
    Self {
      span,
      specifiers,
      exports_all,
    }
  }
}

pub type Imports = IndexMap<(JsWord, DependencyType), ImporterInfo>;

pub struct HarmonyImportDependencyScanner<'a> {
  pub dependencies: &'a mut Vec<BoxDependency>,
  pub presentational_dependencies: &'a mut Vec<BoxDependencyTemplate>,
  pub import_map: &'a mut ImportMap,
  pub imports: Imports,
  pub build_info: &'a mut BuildInfo,
}

impl<'a> HarmonyImportDependencyScanner<'a> {
  pub fn new(
    dependencies: &'a mut Vec<BoxDependency>,
    presentational_dependencies: &'a mut Vec<BoxDependencyTemplate>,
    import_map: &'a mut ImportMap,
    build_info: &'a mut BuildInfo,
  ) -> Self {
    Self {
      dependencies,
      presentational_dependencies,
      import_map,
      imports: Default::default(),
      build_info,
    }
  }
}

impl Visit for HarmonyImportDependencyScanner<'_> {
  noop_visit_type!();

  fn visit_program(&mut self, program: &Program) {
    // collect import map info
    program.visit_children_with(self);
    for ((request, dependency_type), importer_info) in std::mem::take(&mut self.imports).into_iter()
    {
      if matches!(dependency_type, DependencyType::EsmExport)
        && !importer_info.specifiers.is_empty()
      {
        importer_info
          .specifiers
          .iter()
          .for_each(|specifier| match specifier {
            Specifier::Namespace(n) => {
              self
                .dependencies
                .push(Box::new(HarmonyExportImportedSpecifierDependency::new(
                  request.clone(),
                  vec![(n.clone(), None)],
                  Some(n.clone()),
                )));
              self.build_info.harmony_named_exports.insert(n.clone());
            }
            Specifier::Default(_) => {
              unreachable!()
            }
            Specifier::Named(orig, exported) => {
              let name = exported.clone().unwrap_or(orig.clone());
              self
                .dependencies
                .push(Box::new(HarmonyExportImportedSpecifierDependency::new(
                  request.clone(),
                  vec![(name.clone(), Some(orig.clone()))],
                  Some(name.clone()),
                )));
              self.build_info.harmony_named_exports.insert(name);
            }
          });
      }
      let dependency = HarmonyImportDependency::new(
        request.clone(),
        Some(importer_info.span.into()),
        importer_info.specifiers,
        dependency_type,
        importer_info.exports_all,
      );
      if importer_info.exports_all {
        self.build_info.all_star_exports.push(dependency.id);
      }
      self.dependencies.push(Box::new(dependency));
    }

    // collect import reference info
    program.visit_children_with(&mut HarmonyImportRefDependencyScanner::new(
      self.import_map,
      self.dependencies,
    ));
  }

  fn visit_import_decl(&mut self, import_decl: &ImportDecl) {
    let mut specifiers = vec![];
    import_decl.specifiers.iter().for_each(|s| match s {
      ImportSpecifier::Named(n) => {
        let specifier = Specifier::Named(
          n.local.sym.clone(),
          match &n.imported {
            Some(ModuleExportName::Ident(ident)) => Some(ident.sym.clone()),
            _ => None,
          },
        );
        self.import_map.insert(
          n.local.to_id(),
          ImporterReferenceInfo::new(
            import_decl.src.value.clone(),
            specifier.clone(),
            Some(match &n.imported {
              Some(ModuleExportName::Ident(ident)) => ident.sym.clone(),
              _ => n.local.sym.clone(),
            }),
          ),
        );

        specifiers.push(specifier);
      }
      ImportSpecifier::Default(d) => {
        let specifier = Specifier::Default(d.local.sym.clone());
        self.import_map.insert(
          d.local.to_id(),
          ImporterReferenceInfo::new(
            import_decl.src.value.clone(),
            specifier.clone(),
            Some(DEFAULT_JS_WORD.clone()),
          ),
        );
        specifiers.push(specifier);
      }
      ImportSpecifier::Namespace(n) => {
        let specifier = Specifier::Namespace(n.local.sym.clone());
        self.import_map.insert(
          n.local.to_id(),
          ImporterReferenceInfo::new(import_decl.src.value.clone(), specifier.clone(), None),
        );
        specifiers.push(specifier);
      }
    });

    let key = (import_decl.src.value.clone(), DependencyType::EsmImport);
    if let Some(importer_info) = self.imports.get_mut(&key) {
      importer_info.specifiers.extend(specifiers);
    } else {
      self
        .imports
        .insert(key, ImporterInfo::new(import_decl.span, specifiers, false));
    }
    self
      .presentational_dependencies
      .push(Box::new(ConstDependency::new(
        import_decl.span.real_lo(),
        import_decl.span.real_hi(),
        "".into(),
        None,
      )));
  }

  fn visit_named_export(&mut self, named_export: &NamedExport) {
    if let Some(src) = &named_export.src {
      let mut specifiers = vec![];
      named_export
        .specifiers
        .iter()
        .for_each(|specifier| match specifier {
          ExportSpecifier::Namespace(n) => {
            if let ModuleExportName::Ident(export) = &n.name {
              specifiers.push(Specifier::Namespace(export.sym.clone()));
            }
          }
          ExportSpecifier::Default(_) => {
            unreachable!()
          }
          ExportSpecifier::Named(named) => {
            if let ModuleExportName::Ident(orig) = &named.orig {
              specifiers.push(Specifier::Named(
                orig.sym.clone(),
                match &named.exported {
                  Some(ModuleExportName::Ident(export)) => Some(export.sym.clone()),
                  None => None,
                  _ => unreachable!(),
                },
              ));
            }
          }
        });
      let key = (src.value.clone(), DependencyType::EsmExport);
      if let Some(importer_info) = self.imports.get_mut(&key) {
        importer_info.specifiers.extend(specifiers);
      } else {
        self
          .imports
          .insert(key, ImporterInfo::new(named_export.span, specifiers, false));
      }
      self
        .presentational_dependencies
        .push(Box::new(ConstDependency::new(
          named_export.span.real_lo(),
          named_export.span.real_hi(),
          "".into(),
          None,
        )));
    }
  }

  fn visit_export_all(&mut self, export_all: &ExportAll) {
    let key = (export_all.src.value.clone(), DependencyType::EsmExport);

    // self
    //   .dependencies
    //   .push(Box::new(HarmonyExportImportedSpecifierDependency::new(
    //     key.0.clone(),
    //     vec![],
    //     None,
    //   )));
    if let Some(importer_info) = self.imports.get_mut(&key) {
      importer_info.exports_all = true;
    } else {
      self
        .imports
        .insert(key, ImporterInfo::new(export_all.span, vec![], true));
    }

    self
      .presentational_dependencies
      .push(Box::new(ConstDependency::new(
        export_all.span.real_lo(),
        export_all.span.real_hi(),
        "".into(),
        None,
      )));
  }
}

pub struct HarmonyImportRefDependencyScanner<'a> {
  pub enter_callee: bool,
  pub import_map: &'a ImportMap,
  pub dependencies: &'a mut Vec<BoxDependency>,
  pub properties_in_destructuring: HashMap<JsWord, HashSet<JsWord>>,
}

impl<'a> HarmonyImportRefDependencyScanner<'a> {
  pub fn new(import_map: &'a ImportMap, dependencies: &'a mut Vec<BoxDependency>) -> Self {
    Self {
      import_map,
      dependencies,
      enter_callee: false,
      properties_in_destructuring: HashMap::default(),
    }
  }
}

impl Visit for HarmonyImportRefDependencyScanner<'_> {
  noop_visit_type!();

  // collect referenced properties in destructuring
  // import * as a from 'a';
  // const { value } = a;
  fn visit_assign_expr(&mut self, assign_expr: &AssignExpr) {
    if let PatOrExpr::Pat(box Pat::Object(object_pat)) = &assign_expr.left && assign_expr.op == AssignOp::Assign && let box Expr::Ident(ident) = &assign_expr.right && let Some(reference) = self.import_map.get(&ident.to_id()) && matches!(reference.specifier, Specifier::Namespace(_))  {
      if let Some(value) = collect_destructuring_assignment_properties(object_pat) {
        self.properties_in_destructuring.entry(ident.sym.clone()).and_modify(|v| v.extend(value.clone())).or_insert(value);
      }
    }
    assign_expr.visit_children_with(self);
  }

  fn visit_prop(&mut self, n: &Prop) {
    match n {
      Prop::Shorthand(shorthand) => {
        if let Some(reference) = self.import_map.get(&shorthand.to_id()) {
          self
            .dependencies
            .push(Box::new(HarmonyImportSpecifierDependency::new(
              reference.request.clone(),
              true,
              shorthand.span.real_lo(),
              shorthand.span.real_hi(),
              reference.names.clone().map(|f| vec![f]).unwrap_or_default(),
              false,
              false,
              reference.specifier.clone(),
              None,
            )));
        }
      }
      _ => n.visit_children_with(self),
    }
  }

  fn visit_ident(&mut self, ident: &Ident) {
    if let Some(reference) = self.import_map.get(&ident.to_id()) {
      self
        .dependencies
        .push(Box::new(HarmonyImportSpecifierDependency::new(
          reference.request.clone(),
          false,
          ident.span.real_lo(),
          ident.span.real_hi(),
          reference.names.clone().map(|f| vec![f]).unwrap_or_default(),
          self.enter_callee,
          true, // x()
          reference.specifier.clone(),
          self.properties_in_destructuring.remove(&ident.sym),
        )));
    }
  }

  fn visit_member_expr(&mut self, member_expr: &MemberExpr) {
    if let Expr::Ident(ident) = &*member_expr.obj {
      if let Some(reference) = self.import_map.get(&ident.to_id()) {
        let prop = match &member_expr.prop {
          MemberProp::Ident(ident) => Some(ident.sym.clone()),
          MemberProp::Computed(c) => {
            if let Expr::Lit(Lit::Str(str)) = &*c.expr {
              Some(str.value.clone())
            } else {
              None
            }
          }
          _ => None,
        };
        if let Some(prop) = prop {
          let mut ids = reference.names.clone().map(|f| vec![f]).unwrap_or_default();
          ids.push(prop);
          self
            .dependencies
            .push(Box::new(HarmonyImportSpecifierDependency::new(
              reference.request.clone(),
              false,
              member_expr.span.real_lo(),
              member_expr.span.real_hi(),
              ids,
              self.enter_callee,
              !self.enter_callee, // x.xx()
              reference.specifier.clone(),
              None,
            )));
          return;
        }
      }
    }
    member_expr.visit_children_with(self);
  }

  fn visit_callee(&mut self, callee: &Callee) {
    self.enter_callee = true;
    callee.visit_children_with(self);
    self.enter_callee = false;
  }

  fn visit_import_decl(&mut self, _decl: &ImportDecl) {}

  fn visit_named_export(&mut self, _named_export: &NamedExport) {}
}
