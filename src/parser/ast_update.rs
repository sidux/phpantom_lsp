/// AST update orchestration and name resolution.
///
/// This module contains the `update_ast` method that performs a full
/// parse of a PHP file and updates all the backend maps (uri_classes_index,
/// use_map, namespace_map, global_functions, global_defines, fqn_uri_index,
/// symbol_maps) in a single pass.  It also contains the name resolution
/// helpers (`resolve_parent_class_names`, `resolve_name`) used to convert
/// short class names to fully-qualified names.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::ParseErrorEntry;
use crate::atom::{Atom, atom, bytes_to_str};
use crate::names::OwnedResolvedNames;
use crate::php_type::PhpType;
use crate::symbol_map::{SymbolMap, extract_symbol_map};
use crate::types::{ClassInfo, DefineInfo, FunctionInfo, NamespaceSpan, TypeAliasDef};

use bumpalo::Bump;

use mago_span::HasSpan;
use mago_syntax::ast::*;
use mago_syntax::parser::parse_file_content;

use crate::Backend;

use super::DocblockCtx;

/// Run `f` with a parsing arena, reusing a thread-local `Bump` across
/// calls instead of allocating a fresh one each time.
///
/// `update_ast_inner` is invoked on every keystroke (each `didChange`),
/// so a fresh `Bump::new()` per call returns its backing pages to the OS
/// via `munmap` on drop and re-acquires them via `mmap` on the next
/// parse. Reusing one arena and `reset()`ing it (an O(1) bump-pointer
/// rewind that keeps the pages allocated) eliminates those syscalls
/// during active editing.
///
/// Resolution can trigger a nested parse on the same thread (e.g.
/// `find_or_load_function` calls `update_ast` while the outer parse is
/// still using the arena). Such re-entrant calls fall back to a throwaway
/// `Bump` so the shared arena is never aliased — the borrow held for the
/// duration of `f` makes `try_borrow_mut` fail for the nested call.
fn with_reusable_arena<R>(f: impl FnOnce(&Bump) -> R) -> R {
    thread_local! {
        static ARENA: RefCell<Bump> = RefCell::new(Bump::with_capacity(512 * 1024));
    }

    ARENA.with(|cell| match cell.try_borrow_mut() {
        Ok(mut arena) => {
            arena.reset();
            f(&arena)
        }
        Err(_) => f(&Bump::new()),
    })
}

pub(crate) enum AstIndexParseResult {
    Update(AstIndexUpdate),
    ParseFailed {
        uri: String,
        errors: Vec<ParseErrorEntry>,
    },
}

pub(crate) struct AstIndexUpdate {
    uri: String,
    parse_errors: Vec<ParseErrorEntry>,
    classes: Vec<ClassInfo>,
    use_map: HashMap<String, String>,
    resolved_names: Arc<OwnedResolvedNames>,
    namespace_spans: Vec<NamespaceSpan>,
    functions: Vec<FunctionInfo>,
    defines: Vec<(String, DefineInfo)>,
    symbol_map: Arc<SymbolMap>,
}

fn class_info_fqn(class: &ClassInfo) -> String {
    match &class.file_namespace {
        Some(ns) if !ns.is_empty() => format!("{}\\{}", ns, class.name),
        _ => class.name.to_string(),
    }
}

impl Backend {
    /// Update the uri_classes_index, use_map, and namespace_map for a given file URI
    /// by parsing its content.
    ///
    /// Returns `true` when at least one class signature in this file
    /// changed (or a class was added/removed), meaning other open files
    /// that reference those classes may have stale diagnostics.
    pub fn update_ast(&self, uri: &str, content: &str) -> bool {
        // Invalidate thread-local mixin cache so stale ClassInfo is not
        // served after a file changes.
        crate::virtual_members::phpdoc::bump_mixin_generation();

        let content_to_parse = if self.is_blade_file(uri) {
            let (virtual_php, source_map) = crate::blade::preprocessor::preprocess(content);
            self.blade_source_maps
                .write()
                .insert(uri.to_string(), source_map);
            self.blade_virtual_content
                .write()
                .insert(uri.to_string(), virtual_php.clone());
            virtual_php
        } else {
            content.to_string()
        };

        // The mago-syntax parser contains `unreachable!()` and `.expect()`
        // calls that can panic on malformed PHP (e.g. partially-written
        // heredocs/nowdocs, which are common while editing).  Wrap the
        // entire parse + extraction in `catch_unwind` so a parser panic
        // doesn't crash the LSP server and produce a zombie process.
        //
        // On panic the file is simply skipped — no maps are updated, and
        // the user gets stale (but not missing) completions until the
        // file is saved in a parseable state.
        let content_owned = content_to_parse;
        let uri_owned = uri.to_string();

        let result = crate::util::catch_panic_unwind_safe("parse", uri, None, || {
            self.update_ast_inner(&uri_owned, &content_owned)
        });

        match result {
            Some(changed) => changed,
            None => {
                // Parser panicked — store a single "Parse failed" error
                // so the syntax-error diagnostic collector can report it.
                self.parse_errors.write().insert(
                    uri.to_string(),
                    vec![("Parse failed (internal error)".to_string(), 0, 0)],
                );
                false
            }
        }
    }

    /// Inner implementation of [`update_ast`] that performs the actual parse
    /// and publishes the resulting single-file update.
    fn update_ast_inner(&self, uri: &str, content: &str) -> bool {
        let update = self.build_ast_index_update(uri, content);
        self.apply_ast_index_updates_batch(vec![update])
    }

    pub(crate) fn parse_ast_index_update_for_index(
        &self,
        uri: &str,
        content: &str,
    ) -> AstIndexParseResult {
        let uri_owned = uri.to_string();

        match crate::util::catch_panic_unwind_safe("parse", uri, None, || {
            self.build_ast_index_update(uri, content)
        }) {
            Some(update) => AstIndexParseResult::Update(update),
            None => AstIndexParseResult::ParseFailed {
                uri: uri_owned,
                errors: vec![("Parse failed (internal error)".to_string(), 0, 0)],
            },
        }
    }

    pub(crate) fn apply_ast_index_parse_results_batch(
        &self,
        results: Vec<AstIndexParseResult>,
    ) -> bool {
        if results.is_empty() {
            return false;
        }

        let mut updates = Vec::new();
        let mut failures = Vec::new();
        for result in results {
            match result {
                AstIndexParseResult::Update(update) => updates.push(update),
                AstIndexParseResult::ParseFailed { uri, errors } => failures.push((uri, errors)),
            }
        }

        if !failures.is_empty() {
            let mut parse_errors = self.parse_errors.write();
            for (uri, errors) in failures {
                parse_errors.insert(uri, errors);
            }
        }

        self.apply_ast_index_updates_batch(updates)
    }

    fn build_ast_index_update(&self, uri: &str, content: &str) -> AstIndexUpdate {
        with_reusable_arena(|arena| {
            let file_id = mago_database::file::FileId::new(b"input.php");
            let program = parse_file_content(arena, file_id, content.as_bytes());

            // Run mago-names resolver while the arena is still alive.
            // This produces a `ResolvedNames` that maps every identifier's
            // byte offset to its fully-qualified name.  We immediately copy
            // the data into an owned `OwnedResolvedNames` so it survives
            // the arena drop.
            let name_resolver = mago_names::resolver::NameResolver::new(arena);
            let mago_resolved = name_resolver.resolve(program);
            let owned_resolved = OwnedResolvedNames::from_resolved(&mago_resolved);

            let parse_errors: Vec<ParseErrorEntry> = program
                .errors
                .iter()
                .map(|e| {
                    let span = e.span();
                    (
                        super::error_format::format_parse_error(e),
                        span.start.offset,
                        span.end.offset,
                    )
                })
                .collect();

            let doc_ctx = DocblockCtx {
                trivias: program.trivia.as_slice(),
                content,
                php_version: Some(self.php_version()),
                use_map: HashMap::new(),
                namespace: None,
            };

            // Extract all three in a single parse pass.
            //
            // `classes_with_ns` tracks each extracted class together with the
            // namespace block it was declared in.  This is critical for files
            // that contain multiple `namespace { }` blocks (e.g. example.php
            // places demo classes in `Demo` and Illuminate stubs in their own
            // namespace blocks).  The per-class namespace is used later when
            // building the `fqn_uri_index` and when resolving parent/trait names.
            let mut classes_with_ns: Vec<(ClassInfo, Option<String>)> = Vec::new();
            let mut use_map = HashMap::new();
            let mut namespace: Option<String> = None;
            let mut namespace_spans: Vec<NamespaceSpan> = Vec::new();

            for statement in program.statements.iter() {
                match statement {
                    Statement::Use(use_stmt) => {
                        Self::extract_use_items(&use_stmt.items, &mut use_map);
                    }
                    Statement::Namespace(ns) => {
                        // Determine the namespace for this block.
                        let block_ns: Option<String> = ns
                            .name
                            .as_ref()
                            .map(|ident| bytes_to_str(ident.value()).to_string())
                            .filter(|n| !n.is_empty());

                        // Record the byte span of this namespace block.
                        let ns_span = ns.span();
                        namespace_spans.push(NamespaceSpan {
                            namespace: block_ns.clone(),
                            start: ns_span.start.offset,
                            end: ns_span.end.offset,
                        });

                        // The file-level namespace is the FIRST non-empty one.
                        if namespace.is_none() {
                            namespace = block_ns.clone();
                        }

                        // Collect classes from this namespace block, tagging
                        // each with the block's namespace.
                        let mut block_classes = Vec::new();
                        // Recurse into namespace body for classes and use statements
                        for inner in ns.statements().iter() {
                            match inner {
                                Statement::Use(use_stmt) => {
                                    Self::extract_use_items(&use_stmt.items, &mut use_map);
                                }
                                Statement::Class(_)
                                | Statement::Interface(_)
                                | Statement::Trait(_)
                                | Statement::Enum(_)
                                // Class-likes declared inside conditional /
                                // control-flow blocks (e.g. Doctrine's
                                // `ServiceEntityRepository` version guard) —
                                // the extractor descends into the bodies.
                                | Statement::If(_)
                                | Statement::Block(_)
                                | Statement::Try(_)
                                | Statement::Switch(_)
                                | Statement::While(_)
                                | Statement::DoWhile(_)
                                | Statement::For(_)
                                | Statement::Foreach(_) => {
                                    Self::extract_classes_from_statements(
                                        std::iter::once(inner),
                                        &mut block_classes,
                                        Some(&doc_ctx),
                                    );
                                }
                                Statement::Namespace(inner_ns) => {
                                    // Nested namespaces (rare but valid)
                                    Self::extract_use_statements_from_statements(
                                        inner_ns.statements().iter(),
                                        &mut use_map,
                                    );
                                    Self::extract_classes_from_statements(
                                        inner_ns.statements().iter(),
                                        &mut block_classes,
                                        Some(&doc_ctx),
                                    );
                                }
                                _ => {
                                    // Walk other statements (expression statements,
                                    // control flow, etc.) for anonymous classes.
                                    Self::find_anonymous_classes_in_statement(
                                        inner,
                                        &mut block_classes,
                                        Some(&doc_ctx),
                                    );
                                }
                            }
                        }

                        // Tag each class with the namespace of this block.
                        for cls in block_classes {
                            classes_with_ns.push((cls, block_ns.clone()));
                        }
                    }
                    Statement::Class(_)
                    | Statement::Interface(_)
                    | Statement::Trait(_)
                    | Statement::Enum(_)
                    // Class-likes declared inside top-level conditional /
                    // control-flow blocks — the extractor descends into the
                    // bodies (and still collects anonymous classes within).
                    | Statement::If(_)
                    | Statement::Block(_)
                    | Statement::Try(_)
                    | Statement::Switch(_)
                    | Statement::While(_)
                    | Statement::DoWhile(_)
                    | Statement::For(_)
                    | Statement::Foreach(_) => {
                        let mut top_classes = Vec::new();
                        Self::extract_classes_from_statements(
                            std::iter::once(statement),
                            &mut top_classes,
                            Some(&doc_ctx),
                        );
                        for cls in top_classes {
                            classes_with_ns.push((cls, None));
                        }
                    }
                    _ => {
                        // Walk other top-level statements (expression statements,
                        // function declarations, etc.) for anonymous classes.
                        let mut anon_classes = Vec::new();
                        Self::find_anonymous_classes_in_statement(
                            statement,
                            &mut anon_classes,
                            Some(&doc_ctx),
                        );
                        for cls in anon_classes {
                            classes_with_ns.push((cls, None));
                        }
                    }
                }
            }

            // A class-like declared in two branches of a conditional yields
            // one entry per branch; keep the first so resolution is
            // deterministic (see `dedup_class_likes_first_wins`).
            Self::dedup_class_likes_first_wins(&mut classes_with_ns);

            // Extract standalone functions (including those inside if-guards
            // like `if (! function_exists('...'))`) using the shared helper
            // which recurses into if/block statements.
            let mut functions = Vec::new();
            // Update doc_ctx with the file's use-map and namespace so that
            // parameter default values (e.g. `Application::class`) can be
            // resolved to FQNs during extraction.
            let func_doc_ctx = DocblockCtx {
                trivias: doc_ctx.trivias,
                content: doc_ctx.content,
                php_version: doc_ctx.php_version,
                use_map: use_map.clone(),
                namespace: namespace.clone(),
            };
            Self::extract_functions_from_statements(
                program.statements.iter(),
                &mut functions,
                &namespace,
                Some(&func_doc_ctx),
            );
            if !functions.is_empty() {
                // Resolve class-like names in function return types and
                // parameter type hints to FQNs so that cross-file consumers
                // can resolve them without the declaring file's use map.
                // This mirrors the resolution done for class method return
                // types and parameter hints in `resolve_parent_class_names`.
                for func in &mut functions {
                    let skip_names: Vec<String> =
                        func.template_params.iter().map(|a| a.to_string()).collect();
                    // Use the function's own namespace (not the file-level one)
                    // so that multi-namespace files resolve return types
                    // against the correct namespace block.
                    let func_ns = func.namespace.clone().or_else(|| namespace.clone());
                    let resolver = Self::build_type_resolver(&use_map, &func_ns, &skip_names);

                    if let Some(ref ret) = func.return_type {
                        let resolved = ret.resolve_names(&resolver);
                        if resolved != *ret {
                            func.return_type = Some(resolved);
                        }
                    }
                    if let Some(ref ret) = func.native_return_type {
                        let resolved = ret.resolve_names(&resolver);
                        if resolved != *ret {
                            func.native_return_type = Some(resolved);
                        }
                    }
                    if let Some(ref cond) = func.conditional_return {
                        let resolved = cond.resolve_names(&resolver);
                        if resolved != *cond {
                            func.conditional_return = Some(resolved);
                        }
                    }
                    for param in &mut func.parameters {
                        if let Some(ref hint) = param.type_hint {
                            let resolved = hint.resolve_names(&resolver);
                            if resolved != *hint {
                                param.type_hint = Some(resolved);
                            }
                        }
                    }
                    // Resolve exception class names in @throws tags.
                    for throw in &mut func.throws {
                        let resolved = throw.resolve_names(&resolver);
                        if resolved != *throw {
                            *throw = resolved;
                        }
                    }
                }
            }

            // Extract define() constants from the already-parsed AST and
            // store them in the global_defines map so they appear in
            // completions.  This reuses the parse pass above rather than
            // doing a separate regex scan over the raw content.
            let mut define_entries = Vec::new();
            Self::extract_defines_from_statements(
                program.statements.iter(),
                &mut define_entries,
                content,
            );
            let defines: Vec<(String, DefineInfo)> = define_entries
                .into_iter()
                .map(|(name, offset, value)| {
                    (
                        name,
                        DefineInfo {
                            file_uri: uri.to_string(),
                            name_offset: offset,
                            value,
                        },
                    )
                })
                .collect();

            // Post-process: resolve parent_class short names to fully-qualified
            // names using the file's use_map and each class's own namespace so
            // that cross-file inheritance resolution can find parent classes via
            // PSR-4.
            //
            // For files with multiple namespace blocks, each class's names are
            // resolved against its own namespace rather than the file-level
            // default.  This is done by grouping classes by namespace and
            // calling resolve_parent_class_names once per group.
            {
                // Gather distinct namespaces used in this file.
                let mut ns_groups: HashMap<Option<String>, Vec<usize>> = HashMap::new();
                for (i, (_cls, ns)) in classes_with_ns.iter().enumerate() {
                    ns_groups.entry(ns.clone()).or_default().push(i);
                }

                // When all classes share the same namespace, take the fast
                // path (single call, no extra allocation).
                if ns_groups.len() <= 1 {
                    let mut classes: Vec<ClassInfo> =
                        classes_with_ns.iter().map(|(c, _)| c.clone()).collect();
                    Self::resolve_parent_class_names(&mut classes, &use_map, &namespace);
                    // Write back
                    for (i, cls) in classes.into_iter().enumerate() {
                        classes_with_ns[i].0 = cls;
                    }
                } else {
                    // Multi-namespace file: resolve each group with its own
                    // namespace context.
                    for (group_ns, indices) in &ns_groups {
                        let mut group: Vec<ClassInfo> = indices
                            .iter()
                            .map(|&i| classes_with_ns[i].0.clone())
                            .collect();
                        Self::resolve_parent_class_names(&mut group, &use_map, group_ns);
                        for (j, &idx) in indices.iter().enumerate() {
                            classes_with_ns[idx].0 = group[j].clone();
                        }
                    }
                }
            }

            // Separate the classes from their namespace tags for storage,
            // stamping each ClassInfo with its namespace so that
            // `find_class_in_uri_classes_index` can distinguish classes with the same
            // short name in different namespace blocks.
            let classes: Vec<ClassInfo> = classes_with_ns
                .iter()
                .map(|(c, ns)| {
                    let mut cls = c.clone();
                    cls.file_namespace = ns.as_deref().map(atom);
                    cls
                })
                .collect();

            // Build the precomputed symbol map while the AST is still alive.
            // This must happen before the `Program` (and its arena) are dropped.
            let symbol_map = Arc::new(extract_symbol_map(program, content));

            // For files without any explicit namespace blocks, synthesize a
            // single span covering the entire file with the detected namespace
            // (which will be None for files without namespace declarations).
            if namespace_spans.is_empty() {
                namespace_spans.push(NamespaceSpan {
                    namespace: namespace.clone(),
                    start: 0,
                    end: content.len() as u32,
                });
            }

            AstIndexUpdate {
                uri: uri.to_string(),
                parse_errors,
                classes,
                use_map,
                resolved_names: Arc::new(owned_resolved),
                namespace_spans,
                functions,
                defines,
                symbol_map,
            }
        })
    }

    pub(crate) fn apply_ast_index_updates_batch(&self, updates: Vec<AstIndexUpdate>) -> bool {
        if updates.is_empty() {
            return false;
        }

        struct PreparedAstIndexUpdate {
            uri: String,
            parse_errors: Vec<ParseErrorEntry>,
            old_classes: Vec<ClassInfo>,
            old_fqns: Vec<String>,
            new_fqns: Vec<String>,
            classes: Vec<Arc<ClassInfo>>,
            use_map: HashMap<String, String>,
            resolved_names: Arc<OwnedResolvedNames>,
            namespace_spans: Vec<NamespaceSpan>,
            functions: Vec<FunctionInfo>,
            defines: Vec<(String, DefineInfo)>,
            symbol_map: Arc<SymbolMap>,
        }

        let old_classes_by_update: Vec<Vec<ClassInfo>> = {
            let uri_classes = self.uri_classes_index.read();
            updates
                .iter()
                .map(|update| {
                    uri_classes
                        .get(&update.uri)
                        .map(|classes| {
                            classes
                                .iter()
                                .map(|class| ClassInfo::clone(class))
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        };

        let mut prepared = Vec::with_capacity(updates.len());
        let mut all_old_fqns = Vec::new();
        let mut all_new_fqns = Vec::new();
        let mut all_classes = Vec::new();

        for (update, old_classes) in updates.into_iter().zip(old_classes_by_update) {
            let old_fqns: Vec<String> = old_classes
                .iter()
                .filter(|class| !class.name.starts_with("__anonymous@"))
                .map(class_info_fqn)
                .collect();
            let classes: Vec<Arc<ClassInfo>> = update.classes.into_iter().map(Arc::new).collect();
            let new_fqns: Vec<String> = classes
                .iter()
                .filter(|class| !class.name.starts_with("__anonymous@"))
                .map(|class| class.fqn().to_string())
                .collect();

            all_old_fqns.extend(old_fqns.iter().cloned());
            all_new_fqns.extend(new_fqns.iter().cloned());
            all_classes.extend(classes.iter().cloned());

            prepared.push(PreparedAstIndexUpdate {
                uri: update.uri,
                parse_errors: update.parse_errors,
                old_classes,
                old_fqns,
                new_fqns,
                classes,
                use_map: update.use_map,
                resolved_names: update.resolved_names,
                namespace_spans: update.namespace_spans,
                functions: update.functions,
                defines: update.defines,
                symbol_map: update.symbol_map,
            });
        }

        all_old_fqns.sort();
        all_old_fqns.dedup();
        all_new_fqns.sort();
        all_new_fqns.dedup();

        {
            let mut parse_errors = self.parse_errors.write();
            for update in &mut prepared {
                parse_errors.insert(update.uri.clone(), std::mem::take(&mut update.parse_errors));
            }
        }

        {
            let mut idx = self.fqn_uri_index.write();
            let mut fqn_idx = self.fqn_class_index.write();

            for old_fqn in &all_old_fqns {
                idx.remove(old_fqn);
                fqn_idx.remove(old_fqn);
            }

            for update in &prepared {
                for class in &update.classes {
                    if class.name.starts_with("__anonymous@") {
                        continue;
                    }
                    let fqn = class.fqn().to_string();
                    idx.insert(fqn.clone(), update.uri.clone());
                    fqn_idx.insert(fqn, Arc::clone(class));
                }
            }
        }

        {
            let nf_cache = self.class_not_found_cache.read();
            if !nf_cache.is_empty() {
                drop(nf_cache);
                let mut nf_cache = self.class_not_found_cache.write();
                for fqn in &all_new_fqns {
                    nf_cache.remove(fqn);
                }
            }
        }

        {
            let mut fmap = self.global_functions.write();
            for update in &mut prepared {
                for func_info in std::mem::take(&mut update.functions) {
                    let fqn = if let Some(ref ns) = func_info.namespace {
                        format!("{}\\{}", ns, &func_info.name)
                    } else {
                        func_info.name.to_string()
                    };

                    if func_info.is_polyfill
                        && self.stub_function_index.read().contains_key(fqn.as_str())
                    {
                        continue;
                    }

                    fmap.insert(fqn, (update.uri.clone(), func_info));
                }
            }
        }

        {
            let mut dmap = self.global_defines.write();
            for update in &mut prepared {
                for (name, define) in std::mem::take(&mut update.defines) {
                    dmap.entry(name).or_insert(define);
                }
            }
        }

        self.evict_methods_for_fqns(&all_old_fqns);
        self.evict_gti_for_fqns(&all_old_fqns);
        self.populate_method_store(&all_classes);
        self.populate_gti_index(&all_classes);

        // Selectively invalidate the resolved-class cache with
        // signature-level granularity.  Full indexing usually hits the
        // first-parse fast path (`old_fqns` is empty), so this stays cheap
        // during background indexing while preserving edit-time semantics.
        let mut any_signature_changed = false;
        let mut evicted_fqns = Vec::new();
        {
            let mut cache = self.resolved_class_cache.write();
            for update in &prepared {
                if update.old_fqns.is_empty() {
                    continue;
                }

                for fqn in &update.old_fqns {
                    let old_cls = update
                        .old_classes
                        .iter()
                        .find(|class| class_info_fqn(class) == *fqn);
                    let new_cls = update
                        .classes
                        .iter()
                        .find(|class| class.fqn().as_str() == fqn);

                    match (old_cls, new_cls) {
                        (Some(old), Some(new)) if old.signature_eq(new) => {}
                        _ => {
                            evicted_fqns.extend(crate::virtual_members::evict_fqn(&mut cache, fqn));
                            any_signature_changed = true;
                        }
                    }
                }

                for fqn in &update.new_fqns {
                    if !update.old_fqns.contains(fqn) {
                        evicted_fqns.extend(crate::virtual_members::evict_fqn(&mut cache, fqn));
                        any_signature_changed = true;
                    }
                }
            }
        }
        evicted_fqns.sort();
        evicted_fqns.dedup();

        {
            let mut uri_classes = self.uri_classes_index.write();
            let mut parsed_uris = self.parsed_uris.write();
            for update in &mut prepared {
                uri_classes.insert(update.uri.clone(), std::mem::take(&mut update.classes));
                parsed_uris.insert(update.uri.clone());
            }
        }

        {
            let mut imports = self.file_imports.write();
            let mut resolved_names = self.resolved_names.write();
            let mut namespaces = self.file_namespaces.write();
            for update in &mut prepared {
                imports.insert(update.uri.clone(), std::mem::take(&mut update.use_map));
                resolved_names.insert(update.uri.clone(), Arc::clone(&update.resolved_names));
                namespaces.insert(
                    update.uri.clone(),
                    std::mem::take(&mut update.namespace_spans),
                );
            }
        }

        if !evicted_fqns.is_empty() {
            let sorted = {
                let uri_classes = self.uri_classes_index.read();
                let iter = uri_classes
                    .values()
                    .flat_map(|classes| classes.iter())
                    .filter(|class| evicted_fqns.contains(&class.fqn().to_string()))
                    .map(|class| (class.fqn().to_string(), class.as_ref()));
                crate::toposort::toposort_classes(iter)
            };

            let class_loader =
                |name: &str| -> Option<Arc<ClassInfo>> { self.find_or_load_class(name) };
            crate::virtual_members::populate_from_sorted(
                &sorted,
                &self.resolved_class_cache,
                &class_loader,
            );
        }

        if any_signature_changed {
            self.member_completion_cache.lock().clear();
        }

        let reference_items: Vec<(String, Arc<SymbolMap>)> = prepared
            .iter()
            .map(|update| (update.uri.clone(), Arc::clone(&update.symbol_map)))
            .collect();
        self.reindex_references_for_symbol_maps_batch(reference_items);

        {
            let mut symbol_maps = self.symbol_maps.write();
            for update in prepared {
                symbol_maps.insert(update.uri, update.symbol_map);
            }
        }

        any_signature_changed
    }

    /// Resolve `parent_class` short names in a list of `ClassInfo` to
    /// fully-qualified names using the file's `use_map` and `namespace`.
    ///
    /// Rules (matching PHP name resolution):
    ///   1. Already fully-qualified (`\Foo\Bar`) → strip leading `\`
    ///   2. Qualified (`Foo\Bar`) → if first segment is in use_map, expand it;
    ///      otherwise prepend current namespace
    ///   3. Unqualified (`Bar`) → check use_map; otherwise prepend namespace
    ///   4. No namespace and not in use_map → keep as-is
    pub fn resolve_parent_class_names(
        classes: &mut [ClassInfo],
        use_map: &HashMap<String, String>,
        namespace: &Option<String>,
    ) {
        // Collect type alias names from ALL classes in the file up-front.
        // A type alias defined on one class can be referenced from methods
        // in a different class in the same file, so we must skip all of
        // them to avoid mangling alias names into FQN form.
        let all_alias_names: Vec<Atom> = classes
            .iter()
            .flat_map(|c| c.type_aliases.keys().copied())
            .collect();

        for class in classes.iter_mut() {
            if let Some(ref parent) = class.parent_class {
                let resolved = Self::resolve_name(parent, use_map, namespace);
                class.parent_class = Some(atom(&resolved));
            }
            // Resolve trait names to fully-qualified names
            class.used_traits = class
                .used_traits
                .iter()
                .map(|t| atom(&Self::resolve_name(t, use_map, namespace)))
                .collect();

            // Resolve interface names to fully-qualified names
            class.interfaces = class
                .interfaces
                .iter()
                .map(|i| atom(&Self::resolve_name(i, use_map, namespace)))
                .collect();

            // Resolve trait names in `insteadof` precedence adaptations
            for prec in &mut class.trait_precedences {
                prec.trait_name = atom(&Self::resolve_name(&prec.trait_name, use_map, namespace));
                prec.insteadof = prec
                    .insteadof
                    .iter()
                    .map(|t| atom(&Self::resolve_name(t, use_map, namespace)))
                    .collect();
            }

            // Resolve trait names in `as` alias adaptations
            for alias in &mut class.trait_aliases {
                if let Some(ref t) = alias.trait_name {
                    alias.trait_name = Some(atom(&Self::resolve_name(t, use_map, namespace)));
                }
            }

            // Resolve mixin names to fully-qualified names.
            // Skip names that match a template parameter — these are
            // not class names but placeholders that will be substituted
            // with concrete types when the generic class is instantiated
            // (e.g. `@template TWraps` + `@mixin TWraps`).
            class.mixins = class
                .mixins
                .iter()
                .map(|m| {
                    if class.template_params.contains(m) {
                        *m
                    } else {
                        atom(&Self::resolve_name(m, use_map, namespace))
                    }
                })
                .collect();

            // Resolve custom collection class name to FQN
            if let Some(coll) = class.laravel().and_then(|l| l.custom_collection.clone()) {
                let resolver =
                    |name: &str| -> String { Self::resolve_name(name, use_map, namespace) };
                class.laravel_mut().custom_collection = Some(coll.resolve_names(&resolver));
            }

            // Resolve custom builder class name to FQN
            if let Some(builder) = class.laravel().and_then(|l| l.custom_builder.clone()) {
                let resolver =
                    |name: &str| -> String { Self::resolve_name(name, use_map, namespace) };
                class.laravel_mut().custom_builder = Some(builder.resolve_names(&resolver));
            }

            // Resolve cast class names to FQN so that custom cast
            // classes like `DecimalCast` (imported via `use`) are
            // loadable cross-file when `cast_type_to_php_type` calls
            // the class loader.
            {
                let casts: Vec<(String, String)> = class
                    .laravel()
                    .map(|l| l.casts_definitions.clone())
                    .unwrap_or_default();
                if !casts.is_empty() {
                    let resolved: Vec<(String, String)> = casts
                        .into_iter()
                        .map(|(col, cast_type)| {
                            // Only resolve class-like cast types (not
                            // built-in strings like "boolean", "datetime",
                            // etc.).  A simple heuristic: if the value
                            // contains an uppercase letter and is not a
                            // known built-in, treat it as a class name.
                            //
                            // Skip names that already contain a `\` — they
                            // are already qualified (e.g. the string literal
                            // `'App\Casts\HtmlCast'`).  Passing them through
                            // `resolve_name` would prepend the file's
                            // namespace, producing a broken FQN like
                            // `App\Models\App\Casts\HtmlCast`.
                            let first_segment = cast_type.split(':').next().unwrap_or(&cast_type);
                            if first_segment.contains('\\') || first_segment.starts_with('\\') {
                                // Already qualified — strip leading `\` if present to produce canonical FQN.
                                let canonical = cast_type
                                    .strip_prefix('\\')
                                    .map_or(cast_type.clone(), |s| s.to_string());
                                (col, canonical)
                            } else if first_segment.chars().any(|c| c.is_ascii_uppercase()) {
                                let resolved_class =
                                    Self::resolve_name(first_segment, use_map, namespace);
                                if resolved_class != first_segment {
                                    // Re-attach any `:argument` suffix.
                                    let suffix = &cast_type[first_segment.len()..];
                                    (col, format!("{resolved_class}{suffix}"))
                                } else {
                                    (col, cast_type)
                                }
                            } else {
                                (col, cast_type)
                            }
                        })
                        .collect();
                    class.laravel_mut().casts_definitions = resolved;
                }
            }

            // Resolve type arguments in @extends, @implements, and @use
            // generics so that after generic substitution, return types
            // and property types are fully-qualified and can be resolved
            // across files via PSR-4.
            //
            // Template params of the current class must be skipped so
            // that forwarded params (e.g. `@use BuildsQueries<TModel>`
            // where TModel is a class-level template) remain as bare
            // names and match substitution map keys later.
            let tpl_params: Vec<String> = class
                .template_params
                .iter()
                .map(|a| a.to_string())
                .collect();
            Self::resolve_generics_type_args(
                &mut class.extends_generics,
                use_map,
                namespace,
                &tpl_params,
            );
            Self::resolve_generics_type_args(
                &mut class.implements_generics,
                use_map,
                namespace,
                &tpl_params,
            );
            Self::resolve_generics_type_args(
                &mut class.use_generics,
                use_map,
                namespace,
                &tpl_params,
            );
            Self::resolve_generics_type_args(
                &mut class.mixin_generics,
                use_map,
                namespace,
                &tpl_params,
            );

            // Resolve template parameter bounds (`@template T of Bound`)
            // so that short names like `PDependNode` become FQNs like
            // `PDepend\Source\AST\ASTNode`.  Without this, mixin
            // resolution that falls back to bounds gets unresolvable
            // short names.
            {
                let bound_resolver = Self::build_type_resolver(use_map, namespace, &tpl_params);
                for bound in class.template_param_bounds.values_mut() {
                    let resolved = bound.resolve_names(&bound_resolver);
                    if resolved != *bound {
                        *bound = resolved;
                    }
                }
            }

            // Resolve class-like names in method return types and property
            // type hints so that cross-file resolution works correctly.
            // For example, if a method returns `Country` and the file has
            // `use Luxplus\Core\Enums\Country`, the return type becomes
            // the FQN `Luxplus\Core\Enums\Country`.
            //
            // Template params and type alias names are excluded to avoid
            // mangling generic types and locally-defined type aliases.
            // We collect alias names from ALL classes in the file because
            // a type alias defined on one class may be referenced from a
            // method in a different class in the same file.
            let template_params = &class.template_params;
            let skip_names: Vec<String> = template_params
                .iter()
                .map(|a| a.to_string())
                .chain(all_alias_names.iter().map(|a| a.to_string()))
                .collect();
            let resolver = Self::build_type_resolver(use_map, namespace, &skip_names);

            // Also resolve class-like names inside type alias definitions
            // so that `@phpstan-type ActiveUser User` where `User` is
            // imported via `use App\Models\User` becomes `App\Models\User`.
            for def in class.type_aliases.values_mut() {
                match def {
                    TypeAliasDef::Import { source_class, .. } => {
                        // Imported alias — resolve the source class name.
                        let resolved_class = Self::resolve_name(source_class, use_map, namespace);
                        if resolved_class != *source_class {
                            *source_class = resolved_class;
                        }
                    }
                    TypeAliasDef::Local(php_type) => {
                        // Local alias — resolve class names within the type.
                        let resolved = php_type.resolve_names(&resolver);
                        *php_type = resolved;
                    }
                }
            }

            for method in class.methods.make_mut() {
                let method = Arc::make_mut(method);
                // Build a per-method skip list that includes both class-level
                // and method-level template params so that names like `T` in
                // `@return Collection<T>` are not namespace-resolved.
                //
                // When the method has its own template params, build a
                // per-method resolver that skips them in addition to the
                // class-level skip names.  Otherwise reuse the class-level
                // resolver.
                let method_skip: Vec<String>;
                let method_resolver: &dyn Fn(&str) -> String = if method.template_params.is_empty()
                {
                    &resolver
                } else {
                    method_skip = skip_names
                        .iter()
                        .cloned()
                        .chain(method.template_params.iter().map(|a| a.to_string()))
                        .collect();
                    // SAFETY: `method_skip` lives until end of this
                    // `for method` iteration, so the closure is valid.
                    &Self::build_type_resolver(use_map, namespace, &method_skip)
                };

                if let Some(ref ret) = method.return_type {
                    let resolved = ret.resolve_names(method_resolver);
                    if resolved != *ret {
                        method.return_type = Some(resolved);
                    }
                }
                if let Some(ref cond) = method.conditional_return {
                    let resolved = cond.resolve_names(method_resolver);
                    if resolved != *cond {
                        method.conditional_return = Some(resolved);
                    }
                }
                for param in &mut method.parameters {
                    if let Some(ref hint) = param.type_hint {
                        let resolved = hint.resolve_names(method_resolver);
                        if resolved != *hint {
                            param.type_hint = Some(resolved);
                        }
                    }
                }
                // Resolve exception class names in @throws tags.
                for throw in &mut method.throws {
                    let resolved = throw.resolve_names(method_resolver);
                    if resolved != *throw {
                        *throw = resolved;
                    }
                }
            }
            for prop in class.properties.make_mut() {
                if let Some(ref hint) = prop.type_hint {
                    let resolved = hint.resolve_names(&resolver);
                    if resolved != *hint {
                        prop.type_hint = Some(resolved);
                    }
                }
            }

            // Resolve type names inside `@property` / `@property-read` /
            // `@property-write` and `@method` tags in the raw class
            // docblock.  These tags are parsed lazily by the
            // `PHPDocProvider`, but their type strings use short names
            // relative to the declaring file's imports.  Without
            // resolving them here, cross-file consumers whose own
            // use-map does not import the same names would fail to
            // resolve the types.
            if let Some(ref docblock) = class.class_docblock {
                let resolved_docblock = Self::resolve_docblock_tag_types(docblock, &resolver);
                if resolved_docblock != *docblock {
                    class.class_docblock = Some(resolved_docblock);
                }
            }
        }
    }

    /// Resolve type names in `@property`, `@property-read`, `@property-write`,
    /// and `@method` tags inside a raw class-level docblock.
    ///
    /// These tags are parsed lazily by the `PHPDocProvider`, but their type
    /// strings use short names relative to the declaring file's imports.
    /// This method rewrites those type portions to fully-qualified names
    /// so that cross-file consumers can resolve them without access to the
    /// declaring file's use-map.
    fn resolve_docblock_tag_types(docblock: &str, resolver: &dyn Fn(&str) -> String) -> String {
        let mut result = String::with_capacity(docblock.len());

        for line in docblock.split('\n') {
            if !result.is_empty() {
                result.push('\n');
            }

            let trimmed = line.trim().trim_start_matches('*').trim();

            // ── @property[-read|-write] Type $name ──────────────────
            let prop_rest = trimmed
                .strip_prefix("@property-read")
                .or_else(|| trimmed.strip_prefix("@property-write"))
                .or_else(|| trimmed.strip_prefix("@property"));

            if let Some(rest) = prop_rest {
                let rest_trimmed = rest.trim_start();
                // Must have content after the tag
                if !rest_trimmed.is_empty() && !rest_trimmed.starts_with('$') {
                    // Extract the type token (everything before `$name`).
                    // The type may contain generics like `Collection<int, Model>`
                    // so we use `split_type_token` for correct parsing.
                    let (type_token, _remainder) =
                        crate::docblock::types::split_type_token(rest_trimmed);
                    let resolved_type =
                        Self::resolve_type_string_via_php_type(type_token, resolver);
                    if resolved_type != type_token
                        && let Some(type_start) = line.find(type_token)
                    {
                        let type_end = type_start + type_token.len();
                        result.push_str(&line[..type_start]);
                        result.push_str(&resolved_type);
                        result.push_str(&line[type_end..]);
                        continue;
                    }
                }
            }

            // ── @method [static] ReturnType methodName(…) ───────────
            if let Some(rest) = trimmed.strip_prefix("@method") {
                let rest_trimmed = rest.trim_start();
                if !rest_trimmed.is_empty() {
                    // Skip optional `static` keyword
                    let after_static = if let Some(after) = rest_trimmed.strip_prefix("static") {
                        if after.is_empty()
                            || after.starts_with(char::is_whitespace)
                            || after.starts_with('(')
                        {
                            after.trim_start()
                        } else {
                            rest_trimmed
                        }
                    } else {
                        rest_trimmed
                    };

                    // Find the opening paren — the return type is between
                    // the tag (after optional `static`) and the last
                    // whitespace-delimited token before `(`.
                    if let Some(paren_pos) = after_static.find('(') {
                        let before_paren = after_static[..paren_pos].trim();
                        // Split into optional return type + method name.
                        if let Some(last_space) = before_paren.rfind(|c: char| c.is_whitespace()) {
                            let ret_type = before_paren[..last_space].trim();
                            if !ret_type.is_empty() {
                                let resolved_ret =
                                    Self::resolve_type_string_via_php_type(ret_type, resolver);
                                if resolved_ret != ret_type
                                    && let Some(type_start) = line.find(ret_type)
                                {
                                    let type_end = type_start + ret_type.len();
                                    result.push_str(&line[..type_start]);
                                    result.push_str(&resolved_ret);
                                    result.push_str(&line[type_end..]);
                                    continue;
                                }
                            }
                        }
                    }
                }
            }

            // No tag matched or no rewriting needed — keep line as-is.
            result.push_str(line);
        }

        result
    }

    /// Resolve type arguments in a generics list (e.g. `@extends`, `@implements`,
    /// `@use`) to fully-qualified names.
    ///
    /// Each entry is `(ClassName, [TypeArg1, TypeArg2, …])`.  The class name
    /// itself is resolved (e.g. `HasFactory` → `App\Concerns\HasFactory`),
    /// and each type argument that looks like a class name (i.e. not a scalar
    /// like `int`, `string`, etc.) is also resolved.
    ///
    /// `skip_names` contains template parameter names that must NOT be
    /// resolved.  Without this, a forwarded template param like `TModel`
    /// in `@use BuildsQueries<TModel>` would be namespace-qualified to
    /// e.g. `Illuminate\Database\Eloquent\TModel`, preventing it from
    /// matching substitution map keys during generic resolution.
    fn resolve_generics_type_args(
        generics: &mut [(Atom, Vec<PhpType>)],
        use_map: &HashMap<String, String>,
        namespace: &Option<String>,
        skip_names: &[String],
    ) {
        let resolver = Self::build_type_resolver(use_map, namespace, skip_names);
        for (class_name, type_args) in generics.iter_mut() {
            // Resolve the base class/trait/interface name
            let resolved: String = Self::resolve_name(class_name, use_map, namespace);
            *class_name = atom(&resolved);

            // Resolve each type argument (now PhpType) via resolve_names
            for arg in type_args.iter_mut() {
                let resolved = arg.resolve_names(&resolver);
                if resolved != *arg {
                    *arg = resolved;
                }
            }
        }
    }

    /// Build a resolver closure that resolves class-like names to FQNs,
    /// skipping template parameters, type aliases, and keyword types.
    ///
    /// The returned closure is suitable for passing to
    /// `PhpType::resolve_names()`.  `is_keyword_type` inside `resolve_names`
    /// already handles scalar and keyword types; this closure additionally
    /// skips names in `skip_names` (template params and type alias names).
    fn build_type_resolver<'a>(
        use_map: &'a HashMap<String, String>,
        namespace: &'a Option<String>,
        skip_names: &'a [String],
    ) -> impl Fn(&str) -> String + 'a {
        move |name: &str| {
            if skip_names.iter().any(|s| s == name) {
                return name.to_string();
            }
            Self::resolve_name(name, use_map, namespace)
        }
    }

    /// Resolve class-like identifiers within a [`PhpType`] to their
    /// fully-qualified forms, using `PhpType::resolve_names()`.
    ///
    /// This is for callers that already have a parsed `PhpType`, avoiding
    /// a redundant parse→stringify→parse cycle.
    fn resolve_type_via_php_type(ty: &PhpType, resolver: &dyn Fn(&str) -> String) -> PhpType {
        ty.resolve_names(resolver)
    }

    /// Resolve class-like identifiers within a type string to their
    /// fully-qualified forms, using `PhpType::resolve_names()`.
    ///
    /// Parses the string into a `PhpType`, resolves names via the given
    /// resolver, and converts back to a string.  This is used for
    /// string-typed fields (e.g. `native_return_type`,
    /// type alias definitions) where the caller does not have a `PhpType`.
    fn resolve_type_string_via_php_type(
        type_str: &str,
        resolver: &dyn Fn(&str) -> String,
    ) -> String {
        Self::resolve_type_via_php_type(&PhpType::parse(type_str), resolver).to_string()
    }

    /// Resolve a class name to its fully-qualified form given a use_map and
    /// namespace context.
    ///
    /// The returned name is **always without a leading `\`**.  This is the
    /// canonical FQN representation used throughout the codebase.  For
    /// example, `\RuntimeException` is returned as `RuntimeException`, and
    /// `\App\Models\User` as `App\Models\User`.
    fn resolve_name(
        name: &str,
        use_map: &HashMap<String, String>,
        namespace: &Option<String>,
    ) -> String {
        // 1. Already fully-qualified — strip the leading `\`.
        if let Some(stripped) = name.strip_prefix('\\') {
            return stripped.to_string();
        }

        // 2/3. Check if the (first segment of the) name is in the use_map
        if let Some(pos) = name.find('\\') {
            // Qualified name — check first segment
            let first = &name[..pos];
            let rest = &name[pos..]; // includes leading '\'
            if let Some(fqn) = use_map.get(first) {
                return format!("{}{}", fqn, rest);
            }
        } else {
            // Unqualified name — check directly
            if let Some(fqn) = use_map.get(name) {
                return fqn.clone();
            }
        }

        // 4. Prepend current namespace if available.
        if let Some(ns) = namespace {
            format!("{}\\{}", ns, name)
        } else {
            name.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;

    #[test]
    fn ast_index_parse_result_batch_records_failures_and_empty_noops() {
        let backend = Backend::new_test();
        assert!(!backend.apply_ast_index_parse_results_batch(Vec::new()));

        let uri = "file:///project/src/Broken.php";
        let changed =
            backend.apply_ast_index_parse_results_batch(vec![AstIndexParseResult::ParseFailed {
                uri: uri.to_string(),
                errors: vec![("Parse failed (internal error)".to_string(), 10, 20)],
            }]);

        assert!(!changed);
        assert_eq!(
            backend.parse_errors.read().get(uri).cloned(),
            Some(vec![("Parse failed (internal error)".to_string(), 10, 20)])
        );
    }
}
