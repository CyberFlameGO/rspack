use std::{
  collections::HashMap,
  sync::{mpsc, Arc, Mutex},
};

use async_recursion::async_recursion;
use regex::Regex;
use rspack_core::{
  rspack_sources::{RawSource, SourceExt},
  ModuleType,
};
use rspack_error::{internal_error, DiagnosticKind, Error, Result, TraceableError};
use rspack_plugin_javascript::ast::parse_js;
use rspack_plugin_javascript::ast::{print, SourceMapConfig};
use rspack_plugin_javascript::{
  utils::ecma_parse_error_to_rspack_error, ExtractedCommentsInfo, IsModule, SourceMapsConfig,
  TransformOutput,
};
use swc_core::{
  common::{
    collections::AHashMap,
    comments::{Comment, CommentKind, Comments, SingleThreadedComments},
    errors::{Emitter, Handler, HANDLER},
    BytePos, FileName, Mark, SourceMap, GLOBALS,
  },
  ecma::{
    ast::Ident,
    atoms::JsWord,
    parser::{EsConfig, Syntax},
    transforms::base::{
      fixer::fixer,
      helpers::{self, Helpers},
      hygiene::hygiene,
      resolver,
    },
    visit::{noop_visit_type, FoldWith, Visit, VisitMutWith, VisitWith},
  },
};
use swc_ecma_minifier::{
  self,
  option::{MinifyOptions, TopLevelOptions},
};

use crate::{JsMinifyCommentOption, JsMinifyOptions, SwcJsMinimizerRspackPluginOptions};

#[async_recursion]
pub async fn match_object(obj: &SwcJsMinimizerRspackPluginOptions, str: &str) -> Result<bool> {
  if let Some(condition) = &obj.test {
    if !condition.try_match(str).await? {
      return Ok(false);
    }
  }
  if let Some(condition) = &obj.include {
    if !condition.try_match(str).await? {
      return Ok(false);
    }
  }
  if let Some(condition) = &obj.exclude {
    if condition.try_match(str).await? {
      return Ok(false);
    }
  }
  Ok(true)
}

/**
 * Some code is modified based on
 * https://github.com/swc-project/swc/blob/6e5d8b3cf1af74d614d5c073d966da543c26e302/crates/swc/src/lib.rs#L689
 * Apache-2.0 licensed
 * Author Donny/강동윤
 * Copyright (c)
 */
pub(crate) fn minify_file_comments(
  comments: &SingleThreadedComments,
  preserve_comments: JsMinifyCommentOption,
) {
  match preserve_comments {
    JsMinifyCommentOption::PreserveAllComments => {}

    JsMinifyCommentOption::PreserveSomeComments => {
      let preserve_excl = |_: &BytePos, vc: &mut Vec<Comment>| -> bool {
        // Preserve license comments.
        vc.retain(|c: &Comment| c.text.contains("@license") || c.text.starts_with('!'));
        !vc.is_empty()
      };
      let (mut l, mut t) = comments.borrow_all_mut();

      l.retain(preserve_excl);
      t.retain(preserve_excl);
    }

    JsMinifyCommentOption::False => {
      let (mut l, mut t) = comments.borrow_all_mut();
      l.clear();
      t.clear();
    }
  }
}

pub fn minify(
  opts: &JsMinifyOptions,
  input: String,
  filename: &str,
  all_extract_comments: &Mutex<HashMap<String, ExtractedCommentsInfo>>,
  extract_comments: &Option<String>,
) -> Result<TransformOutput> {
  let cm: Arc<SourceMap> = Default::default();
  GLOBALS.set(&Default::default(), || -> Result<TransformOutput> {
    with_rspack_error_handler(
      "Minify Error".to_string(),
      DiagnosticKind::JavaScript,
      cm.clone(),
      |handler| {
        let fm = cm.new_source_file(FileName::Custom(filename.to_string()), input);
        let target = opts.ecma.clone().into();

        let source_map = opts
          .source_map
          .as_ref()
          .map(|_| SourceMapsConfig::Bool(true))
          .unwrap_as_option(|v| {
            Some(match v {
              Some(true) => SourceMapsConfig::Bool(true),
              _ => SourceMapsConfig::Bool(false),
            })
          })
          .expect("TODO:");

        let mut min_opts = MinifyOptions {
          compress: opts
            .compress
            .clone()
            .unwrap_as_option(|default| match default {
              Some(true) | None => Some(Default::default()),
              _ => None,
            })
            .map(|v| v.into_config(cm.clone())),
          mangle: opts
            .mangle
            .clone()
            .unwrap_as_option(|default| match default {
              Some(true) | None => Some(Default::default()),
              _ => None,
            }),
          ..Default::default()
        };

        // top_level defaults to true if module is true

        // https://github.com/swc-project/swc/issues/2254
        if opts.module {
          if let Some(opts) = &mut min_opts.compress {
            if opts.top_level.is_none() {
              opts.top_level = Some(TopLevelOptions { functions: true });
            }
          }

          if let Some(opts) = &mut min_opts.mangle {
            opts.top_level = Some(true);
          }
        }

        let comments = SingleThreadedComments::default();

        let program = parse_js(
          fm.clone(),
          target,
          Syntax::Es(EsConfig {
            jsx: true,
            decorators: true,
            decorators_before_export: true,
            ..Default::default()
          }),
          IsModule::Bool(opts.module),
          Some(&comments),
        )
        .map_err(|errs| {
          Error::BatchErrors(
            errs
              .into_iter()
              .map(|err| ecma_parse_error_to_rspack_error(err, &fm, &ModuleType::Js))
              .collect::<Vec<_>>(),
          )
        })?;

        let source_map_names = if source_map.enabled() {
          let mut v = IdentCollector {
            names: Default::default(),
          };

          program.visit_with(&mut v);

          v.names
        } else {
          Default::default()
        };

        let unresolved_mark = Mark::new();
        let top_level_mark = Mark::new();

        let is_mangler_enabled = min_opts.mangle.is_some();

        let program = helpers::HELPERS.set(&Helpers::new(false), || {
          HANDLER.set(handler, || {
            let program = program.fold_with(&mut resolver(unresolved_mark, top_level_mark, false));

            let mut program = swc_ecma_minifier::optimize(
              program,
              cm.clone(),
              Some(&comments),
              None,
              &min_opts,
              &swc_ecma_minifier::option::ExtraOptions {
                unresolved_mark,
                top_level_mark,
              },
            );

            if !is_mangler_enabled {
              program.visit_mut_with(&mut hygiene())
            }
            program.fold_with(&mut fixer(Some(&comments as &dyn Comments)))
          })
        });

        if let Some(extract_comments) = extract_comments {
          let comments_file_name = filename.to_string() + ".LICENSE.txt";
          let reg = if extract_comments.eq("true") {
            // copied from terser-webpack-plugin
            Regex::new(r"@preserve|@lic|@cc_on|^\**!")
          } else {
            Regex::new(&extract_comments[1..extract_comments.len() - 2])
          }
          .expect("Invalid extractComments");
          let mut extracted_comments = vec![];
          // add all matched comments to source

          let (leading_trivial, trailing_trivial) = comments.borrow_all();

          leading_trivial.iter().for_each(|(_, comments)| {
            comments.iter().for_each(|c| {
              if reg.is_match(&c.text) {
                extracted_comments.push(match c.kind {
                  CommentKind::Line => {
                    format!("// {}", c.text)
                  }
                  CommentKind::Block => {
                    format!("/*{}*/", c.text)
                  }
                });
              }
            });
          });
          trailing_trivial.iter().for_each(|(_, comments)| {
            comments.iter().for_each(|c| {
              if reg.is_match(&c.text) {
                extracted_comments.push(match c.kind {
                  CommentKind::Line => {
                    format!("// {}", c.text)
                  }
                  CommentKind::Block => {
                    format!("/*{}*/", c.text)
                  }
                });
              }
            });
          });

          // if not matched comments, we don't need to emit .License.txt file
          if !extracted_comments.is_empty() {
            all_extract_comments
              .lock()
              .expect("all_extract_comments lock failed")
              .insert(
                filename.to_string(),
                ExtractedCommentsInfo {
                  source: RawSource::Source(extracted_comments.join("\n\n")).boxed(),
                  comments_file_name,
                },
              );
          }
        }

        minify_file_comments(&comments, opts.format.comments.to_owned());

        print(
          &program,
          cm.clone(),
          target,
          SourceMapConfig {
            enable: source_map.enabled(),
            inline_sources_content: opts.inline_sources_content,
            emit_columns: opts.emit_source_map_columns,
            names: source_map_names,
          },
          true,
          Some(&comments),
          opts.format.ascii_only,
        )
      },
    )
  })
}

pub struct IdentCollector {
  names: AHashMap<BytePos, JsWord>,
}

impl Visit for IdentCollector {
  noop_visit_type!();

  fn visit_ident(&mut self, ident: &Ident) {
    self.names.insert(ident.span.lo, ident.sym.clone());
  }
}

// keep this private to make sure with_rspack_error_handler is safety
struct RspackErrorEmitter {
  tx: mpsc::Sender<rspack_error::Error>,
  source_map: Arc<SourceMap>,
  title: String,
  kind: DiagnosticKind,
}

impl Emitter for RspackErrorEmitter {
  fn emit(&mut self, db: &swc_core::common::errors::DiagnosticBuilder<'_>) {
    let source_file_and_byte_pos = db
      .span
      .primary_span()
      .map(|s| self.source_map.lookup_byte_offset(s.lo()));
    if let Some(source_file_and_byte_pos) = source_file_and_byte_pos {
      self
        .tx
        .send(Error::TraceableError(
          TraceableError::from_source_file(
            &source_file_and_byte_pos.sf,
            source_file_and_byte_pos.pos.0 as usize,
            source_file_and_byte_pos.pos.0 as usize,
            self.title.to_string(),
            db.message(),
          )
          .with_kind(self.kind),
        ))
        .expect("Sender should drop after emit called");
    } else {
      self
        .tx
        .send(internal_error!(db.message()))
        .expect("Sender should drop after emit called");
    }
  }
}

pub fn with_rspack_error_handler<F, Ret>(
  title: String,
  kind: DiagnosticKind,
  cm: Arc<SourceMap>,
  op: F,
) -> Result<Ret>
where
  F: FnOnce(&Handler) -> Result<Ret>,
{
  let (tx, rx) = mpsc::channel();
  let emitter = RspackErrorEmitter {
    title,
    kind,
    source_map: cm,
    tx,
  };
  let handler = Handler::with_emitter(true, false, Box::new(emitter));

  let ret = HANDLER.set(&handler, || op(&handler));

  if handler.has_errors() {
    drop(handler);
    Err(rspack_error::Error::BatchErrors(rx.into_iter().collect()))
  } else {
    ret
  }
}
