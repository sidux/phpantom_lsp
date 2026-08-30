use std::collections::HashMap;
use std::sync::Arc;

use tower_lsp::lsp_types::{Location, Range, Url};

use crate::Backend;
use crate::config::{
    SymfonyExpressionAttributeConfig, SymfonyExpressionConstructorConfig,
    SymfonyExpressionLanguageConfig,
};
use crate::symbol_map::VarDefKind;
use crate::text_position::offset_to_position;
use crate::types::{ClassInfo, FileContext};

use super::{
    FrameworkReference, FrameworkReferenceKind, PhpCallContext, PhpStringLiteral,
    SymfonyExpressionAccessKind, SymfonyExpressionContext, SymfonyExpressionSegment,
    is_php_identifier_char, is_php_name_char, matching_delimiter, normalize_framework_fqn,
    php_call_context, php_named_argument_before, skip_ascii_whitespace,
    skip_ascii_whitespace_backwards,
};

pub(super) fn scan_php_expression_references(
    uri: &str,
    content: &str,
    literals: &[PhpStringLiteral<'_>],
    use_map: &HashMap<String, String>,
    namespace: &Option<String>,
    config: &SymfonyExpressionLanguageConfig,
    refs: &mut Vec<FrameworkReference>,
) {
    for literal in literals {
        let Some(call) = php_call_context(content, literal.quote_start) else {
            continue;
        };
        if let Some(rule) =
            expression_constructor_rule(content, call, use_map, namespace, &config.constructors)
            && rule_matches_argument(
                content,
                call,
                rule.argument.as_deref(),
                rule.position,
                literal.quote_start,
            )
        {
            scan_expression(uri, literal, expression_context(rule), refs);
            continue;
        }
        let Some(rule) =
            expression_attribute_rule(content, call, use_map, namespace, &config.attributes)
        else {
            continue;
        };
        if rule_matches_argument(
            content,
            call,
            rule.argument.as_deref(),
            rule.position,
            literal.quote_start,
        ) {
            scan_expression(uri, literal, expression_context(rule), refs);
        }
    }
}

fn expression_context(rule: &(impl ExpressionRule + ?Sized)) -> SymfonyExpressionContext {
    SymfonyExpressionContext {
        method_parameters: rule.method_parameters(),
        bindings: rule.bindings().clone(),
    }
}

trait ExpressionRule {
    fn method_parameters(&self) -> bool;
    fn bindings(&self) -> &HashMap<String, String>;
}

impl ExpressionRule for SymfonyExpressionAttributeConfig {
    fn method_parameters(&self) -> bool {
        self.method_parameters
    }

    fn bindings(&self) -> &HashMap<String, String> {
        &self.bindings
    }
}

impl ExpressionRule for SymfonyExpressionConstructorConfig {
    fn method_parameters(&self) -> bool {
        self.method_parameters
    }

    fn bindings(&self) -> &HashMap<String, String> {
        &self.bindings
    }
}

fn rule_matches_argument(
    content: &str,
    call: PhpCallContext<'_>,
    argument: Option<&str>,
    position: Option<usize>,
    offset: usize,
) -> bool {
    argument.is_some_and(|name| named_argument_contains_offset(content, call, name, offset))
        || (position.is_some_and(|position| call.argument_index == position)
            && php_named_argument_before(content, call.args_start, offset).is_none())
}

fn expression_attribute_rule<'a>(
    content: &str,
    call: PhpCallContext<'_>,
    use_map: &HashMap<String, String>,
    namespace: &Option<String>,
    rules: &'a [SymfonyExpressionAttributeConfig],
) -> Option<&'a SymfonyExpressionAttributeConfig> {
    let raw_name = attribute_call_name(content, call)?;
    let fqn = normalize_framework_fqn(&crate::util::resolve_to_fqn(raw_name, use_map, namespace));
    rules
        .iter()
        .find(|rule| normalize_framework_fqn(&rule.attribute) == fqn)
}

fn expression_constructor_rule<'a>(
    content: &str,
    call: PhpCallContext<'_>,
    use_map: &HashMap<String, String>,
    namespace: &Option<String>,
    rules: &'a [SymfonyExpressionConstructorConfig],
) -> Option<&'a SymfonyExpressionConstructorConfig> {
    let name_offset = call.name.as_ptr() as usize - content.as_ptr() as usize;
    let mut raw_start = name_offset;
    while raw_start > 0 && is_php_name_char(content.as_bytes()[raw_start - 1]) {
        raw_start -= 1;
    }
    let before = content[..raw_start].trim_end();
    if !before.strip_suffix("new").is_some_and(|prefix| {
        prefix
            .as_bytes()
            .last()
            .is_none_or(|byte| !is_php_identifier_char(*byte))
    }) {
        return None;
    }
    let attribute_start = content[..raw_start].rfind("#[")?;
    if !matching_delimiter(content, attribute_start + 1, b'[', b']')
        .is_some_and(|end| end >= call.args_start)
    {
        return None;
    }
    let attribute_name = enclosing_attribute_name(content, attribute_start, raw_start)?;
    let attribute_fqn = normalize_framework_fqn(&crate::util::resolve_to_fqn(
        attribute_name,
        use_map,
        namespace,
    ));
    let raw_name = &content[raw_start..name_offset + call.name.len()];
    let constructor_fqn =
        normalize_framework_fqn(&crate::util::resolve_to_fqn(raw_name, use_map, namespace));
    rules.iter().find(|rule| {
        normalize_framework_fqn(&rule.class) == constructor_fqn
            && (rule.inside_attribute_prefixes.is_empty()
                || rule
                    .inside_attribute_prefixes
                    .iter()
                    .any(|prefix| attribute_fqn.starts_with(&normalize_framework_fqn(prefix))))
    })
}

fn enclosing_attribute_name(
    content: &str,
    attribute_start: usize,
    nested_start: usize,
) -> Option<&str> {
    let bytes = content.as_bytes();
    let mut segment_start = attribute_start + 2;
    let mut cursor = segment_start;
    let mut paren_depth = 0u32;
    let mut bracket_depth = 0u32;
    let mut brace_depth = 0u32;
    let mut quote = None;
    while cursor < nested_start {
        let byte = bytes[cursor];
        if let Some(active_quote) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(nested_start);
                continue;
            }
            if byte == active_quote {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else {
            match byte {
                b'(' => paren_depth += 1,
                b')' => paren_depth = paren_depth.saturating_sub(1),
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b'{' => brace_depth += 1,
                b'}' => brace_depth = brace_depth.saturating_sub(1),
                b',' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                    segment_start = cursor + 1;
                }
                _ => {}
            }
        }
        cursor += 1;
    }
    skip_ascii_whitespace(bytes, &mut segment_start);
    let mut name_end = segment_start;
    while name_end < nested_start && is_php_name_char(bytes[name_end]) {
        name_end += 1;
    }
    (name_end > segment_start).then(|| &content[segment_start..name_end])
}

fn attribute_call_name<'a>(content: &'a str, call: PhpCallContext<'a>) -> Option<&'a str> {
    let open = call.args_start.checked_sub(1)?;
    let bytes = content.as_bytes();
    let mut name_end = open;
    skip_ascii_whitespace_backwards(bytes, &mut name_end);
    let mut name_start = name_end;
    while name_start > 0 && is_php_name_char(bytes[name_start - 1]) {
        name_start -= 1;
    }
    if name_start == name_end {
        return None;
    }

    let attribute_start = content[..name_start].rfind("#[")?;
    let attribute_end = matching_delimiter(content, attribute_start + 1, b'[', b']')?;
    if attribute_end < open
        || !is_top_level_attribute_name(&content[attribute_start + 2..name_start])
    {
        return None;
    }
    content.get(name_start..name_end)
}

fn is_top_level_attribute_name(prefix: &str) -> bool {
    if prefix.trim().is_empty() {
        return true;
    }

    let bytes = prefix.as_bytes();
    let mut paren_depth = 0u32;
    let mut bracket_depth = 0u32;
    let mut brace_depth = 0u32;
    let mut quote = None;
    let mut escaped = false;
    let mut last_top_level_comma = None;
    for (idx, byte) in bytes.iter().copied().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        if byte == b'\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            if quote == Some(byte) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(byte);
            }
            continue;
        }
        if quote.is_some() {
            continue;
        }
        match byte {
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b',' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                last_top_level_comma = Some(idx);
            }
            _ => {}
        }
    }

    last_top_level_comma.is_some_and(|idx| prefix[idx + 1..].trim().is_empty())
}

fn named_argument_contains_offset(
    content: &str,
    call: PhpCallContext<'_>,
    target_name: &str,
    offset: usize,
) -> bool {
    let Some(open) = call.args_start.checked_sub(1) else {
        return false;
    };
    let Some(args_end) = matching_delimiter(content, open, b'(', b')') else {
        return false;
    };
    let bytes = content.as_bytes();
    let mut segment_start = call.args_start;
    let mut cursor = call.args_start;
    let mut paren_depth = 0u32;
    let mut bracket_depth = 0u32;
    let mut brace_depth = 0u32;
    let mut quote = None;
    while cursor <= args_end {
        let byte = bytes.get(cursor).copied().unwrap_or(b',');
        if let Some(active_quote) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(args_end + 1);
                continue;
            }
            if byte == active_quote {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else {
            match byte {
                b'(' => paren_depth += 1,
                b')' if cursor < args_end => paren_depth = paren_depth.saturating_sub(1),
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b'{' => brace_depth += 1,
                b'}' => brace_depth = brace_depth.saturating_sub(1),
                b',' | b')' if paren_depth == 0 && bracket_depth == 0 && brace_depth == 0 => {
                    if named_segment_contains_offset(
                        content,
                        segment_start,
                        cursor,
                        target_name,
                        offset,
                    ) {
                        return true;
                    }
                    segment_start = cursor + 1;
                }
                _ => {}
            }
        }
        cursor += 1;
    }
    false
}

fn named_segment_contains_offset(
    content: &str,
    start: usize,
    end: usize,
    target_name: &str,
    offset: usize,
) -> bool {
    if offset < start || offset >= end {
        return false;
    }
    let bytes = content.as_bytes();
    let mut cursor = start;
    skip_ascii_whitespace(bytes, &mut cursor);
    let name_start = cursor;
    while cursor < end && is_php_identifier_char(bytes[cursor]) {
        cursor += 1;
    }
    let name_end = cursor;
    skip_ascii_whitespace(bytes, &mut cursor);
    bytes.get(cursor) == Some(&b':')
        && content[name_start..name_end].eq_ignore_ascii_case(target_name)
        && offset > cursor
}

fn scan_expression(
    uri: &str,
    literal: &PhpStringLiteral<'_>,
    context: SymfonyExpressionContext,
    refs: &mut Vec<FrameworkReference>,
) {
    let bytes = literal.value.as_bytes();
    let mut cursor = 0usize;
    let mut quote = None;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(active_quote) = quote {
            if byte == b'\\' {
                cursor = (cursor + 2).min(bytes.len());
                continue;
            }
            if byte == active_quote {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            cursor += 1;
            continue;
        }
        if !is_expression_identifier_start(byte) {
            cursor += 1;
            continue;
        }

        let root_start = cursor;
        cursor += 1;
        while cursor < bytes.len() && is_php_identifier_char(bytes[cursor]) {
            cursor += 1;
        }
        let root_end = cursor;
        if previous_non_whitespace(bytes, root_start) == Some(b'.') {
            continue;
        }
        let variable = literal.value[root_start..root_end].to_string();
        refs.push(FrameworkReference {
            uri: uri.to_string(),
            start: (literal.start + root_start) as u32,
            end: (literal.start + root_end) as u32,
            kind: FrameworkReferenceKind::SymfonyExpression {
                variable: variable.clone(),
                path: Vec::new(),
                expression_start: literal.quote_start as u32,
                context: context.clone(),
            },
        });

        let mut path = Vec::new();
        let mut chain_cursor = root_end;
        loop {
            skip_ascii_whitespace(bytes, &mut chain_cursor);
            if bytes.get(chain_cursor..chain_cursor + 2) == Some(b"?.") {
                chain_cursor += 2;
            } else if bytes.get(chain_cursor) == Some(&b'.') {
                chain_cursor += 1;
            } else {
                break;
            }
            skip_ascii_whitespace(bytes, &mut chain_cursor);
            if !bytes
                .get(chain_cursor)
                .is_some_and(|byte| is_expression_identifier_start(*byte))
            {
                break;
            }
            let member_start = chain_cursor;
            chain_cursor += 1;
            while chain_cursor < bytes.len() && is_php_identifier_char(bytes[chain_cursor]) {
                chain_cursor += 1;
            }
            let member_end = chain_cursor;
            let mut after_member = chain_cursor;
            skip_ascii_whitespace(bytes, &mut after_member);
            let kind = if bytes.get(after_member) == Some(&b'(') {
                SymfonyExpressionAccessKind::Method
            } else {
                SymfonyExpressionAccessKind::Property
            };
            path.push(SymfonyExpressionSegment {
                name: literal.value[member_start..member_end].to_string(),
                kind,
            });
            refs.push(FrameworkReference {
                uri: uri.to_string(),
                start: (literal.start + member_start) as u32,
                end: (literal.start + member_end) as u32,
                kind: FrameworkReferenceKind::SymfonyExpression {
                    variable: variable.clone(),
                    path: path.clone(),
                    expression_start: literal.quote_start as u32,
                    context: context.clone(),
                },
            });
            chain_cursor = if kind == SymfonyExpressionAccessKind::Method {
                matching_delimiter(literal.value, after_member, b'(', b')')
                    .map_or(member_end, |close| close + 1)
            } else {
                member_end
            };
        }
    }
}

fn is_expression_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn previous_non_whitespace(bytes: &[u8], start: usize) -> Option<u8> {
    bytes[..start]
        .iter()
        .rev()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
}

struct ExpressionRoot {
    file: FileContext,
    definition_offset: u32,
    definition_len: usize,
    classes: Vec<Arc<ClassInfo>>,
}

enum ExpressionMemberStatus {
    Valid,
    Missing(Vec<String>),
    Unresolved,
    BrokenEarlier,
}

impl Backend {
    /// Resolve an ExpressionLanguage variable or member to its PHP declaration.
    pub(crate) fn resolve_symfony_expression_definition(
        &self,
        uri: &str,
        content: &str,
        variable: &str,
        path: &[SymfonyExpressionSegment],
        expression_start: u32,
        context: &SymfonyExpressionContext,
    ) -> Vec<Location> {
        let Some(root) = self.resolve_expression_root(uri, variable, expression_start, context)
        else {
            return Vec::new();
        };
        if path.is_empty() {
            let Ok(uri) = Url::parse(uri) else {
                return Vec::new();
            };
            let start = root.definition_offset as usize;
            return vec![Location {
                uri,
                range: Range {
                    start: offset_to_position(content, start),
                    end: offset_to_position(content, start + root.definition_len),
                },
            }];
        }

        let class_loader = self.class_loader(&root.file);
        let Some(classes) = expression_path_target_classes(
            root.classes,
            path,
            &class_loader,
            Some(&self.resolved_class_cache),
        ) else {
            return Vec::new();
        };
        let Some(target) = path.last() else {
            return Vec::new();
        };
        let mut locations = Vec::new();
        for class in classes {
            let location = match target.kind {
                SymfonyExpressionAccessKind::Property => self
                    .resolve_framework_property_definition(
                        uri,
                        content,
                        &class.fqn(),
                        &target.name,
                    ),
                SymfonyExpressionAccessKind::Method => self.resolve_framework_member_definition(
                    uri,
                    content,
                    &class.fqn(),
                    &target.name,
                ),
            };
            if let Some(location) = location
                && !locations.iter().any(|existing: &Location| {
                    existing.uri == location.uri && existing.range == location.range
                })
            {
                locations.push(location);
            }
        }
        locations
    }

    fn resolve_expression_root(
        &self,
        uri: &str,
        variable: &str,
        expression_start: u32,
        context: &SymfonyExpressionContext,
    ) -> Option<ExpressionRoot> {
        let file = self.file_context_at(uri, expression_start);
        let (owner, method) = file
            .classes
            .iter()
            .flat_map(|class| {
                class
                    .methods
                    .iter()
                    .map(move |method| (Arc::clone(class), Arc::clone(method)))
            })
            .filter(|(_, method)| method.name_offset > expression_start)
            .min_by_key(|(_, method)| method.name_offset)?;
        let binding = context.bindings.get(variable).map(String::as_str);
        if let Some(class_fqn) = binding.and_then(|source| source.strip_prefix("class:")) {
            let class = {
                let class_loader = self.class_loader(&file);
                class_loader(class_fqn)
            }?;
            return Some(ExpressionRoot {
                file,
                definition_offset: expression_start.saturating_add(1),
                definition_len: variable.len(),
                classes: vec![class],
            });
        }

        let return_source = binding == Some("return");
        let parameter = if return_source {
            None
        } else if let Some(selector) = binding.and_then(|source| source.strip_prefix("parameter:"))
        {
            selector
                .parse::<usize>()
                .ok()
                .and_then(|index| method.parameters.get(index))
                .or_else(|| {
                    method.parameters.iter().find(|parameter| {
                        parameter.name.strip_prefix('$').unwrap_or(&parameter.name) == selector
                    })
                })
        } else if context.method_parameters {
            method
                .parameters
                .iter()
                .find(|parameter| parameter.name.strip_prefix('$') == Some(variable))
        } else {
            None
        };

        let (definition_offset, definition_len, type_hint) = if return_source {
            (
                method.name_offset,
                method.name.len(),
                method.return_type.as_ref(),
            )
        } else {
            let parameter = parameter?;
            let parameter_name = parameter.name.strip_prefix('$').unwrap_or(&parameter.name);
            let parameter_offset = self
                .symbol_maps
                .read()
                .get(uri)?
                .var_defs
                .iter()
                .filter(|site| {
                    site.kind == VarDefKind::Parameter
                        && site.name == parameter_name
                        && site.offset > method.name_offset
                        && (owner.end_offset == 0 || site.offset < owner.end_offset)
                })
                .map(|site| site.offset)
                .min()?;
            (
                parameter_offset + 1,
                parameter_name.len(),
                parameter.type_hint.as_ref(),
            )
        };
        let classes = {
            let class_loader = self.class_loader(&file);
            type_hint.map_or_else(Vec::new, |type_hint| {
                crate::type_engine::type_resolution::type_hint_to_classes_typed(
                    type_hint,
                    &owner.fqn(),
                    &file.classes,
                    &class_loader,
                )
            })
        };
        Some(ExpressionRoot {
            file,
            definition_offset,
            definition_len,
            classes,
        })
    }

    /// Return the candidate class names when the final path member is missing.
    pub(crate) fn symfony_expression_missing_classes(
        &self,
        uri: &str,
        variable: &str,
        path: &[SymfonyExpressionSegment],
        expression_start: u32,
        context: &SymfonyExpressionContext,
    ) -> Option<Vec<String>> {
        let root = self.resolve_expression_root(uri, variable, expression_start, context)?;
        if root.classes.is_empty() {
            return None;
        }
        let class_loader = self.class_loader(&root.file);
        match expression_path_status(
            root.classes,
            path,
            &class_loader,
            Some(&self.resolved_class_cache),
        ) {
            ExpressionMemberStatus::Missing(classes) => Some(classes),
            ExpressionMemberStatus::Valid
            | ExpressionMemberStatus::Unresolved
            | ExpressionMemberStatus::BrokenEarlier => None,
        }
    }
}

fn expression_path_target_classes(
    mut classes: Vec<Arc<ClassInfo>>,
    path: &[SymfonyExpressionSegment],
    class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
    cache: Option<&crate::virtual_members::ResolvedClassCache>,
) -> Option<Vec<Arc<ClassInfo>>> {
    for (idx, segment) in path.iter().enumerate() {
        let (matching, _) = matching_member_classes(&classes, segment, class_loader, cache);
        if matching.is_empty() {
            return None;
        }
        if idx + 1 == path.len() {
            return Some(matching);
        }
        classes = next_expression_classes(&matching, segment, class_loader);
        if classes.is_empty() {
            return None;
        }
    }
    None
}

fn expression_path_status(
    mut classes: Vec<Arc<ClassInfo>>,
    path: &[SymfonyExpressionSegment],
    class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
    cache: Option<&crate::virtual_members::ResolvedClassCache>,
) -> ExpressionMemberStatus {
    for (idx, segment) in path.iter().enumerate() {
        let (matching, dynamic) = matching_member_classes(&classes, segment, class_loader, cache);
        if matching.is_empty() {
            if dynamic {
                return ExpressionMemberStatus::Unresolved;
            }
            if idx + 1 != path.len() {
                return ExpressionMemberStatus::BrokenEarlier;
            }
            let mut names = classes
                .iter()
                .map(|class| class.fqn().to_string())
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            return ExpressionMemberStatus::Missing(names);
        }
        if idx + 1 == path.len() {
            return ExpressionMemberStatus::Valid;
        }
        classes = next_expression_classes(&matching, segment, class_loader);
        if classes.is_empty() {
            return ExpressionMemberStatus::Unresolved;
        }
    }
    ExpressionMemberStatus::Valid
}

fn matching_member_classes(
    classes: &[Arc<ClassInfo>],
    segment: &SymfonyExpressionSegment,
    class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
    cache: Option<&crate::virtual_members::ResolvedClassCache>,
) -> (Vec<Arc<ClassInfo>>, bool) {
    let mut matching = Vec::new();
    let mut dynamic = false;
    for class in classes {
        let resolved = if class.name == "__object_shape" {
            Arc::clone(class)
        } else {
            crate::virtual_members::resolve_class_fully_maybe_cached(class, class_loader, cache)
        };
        let exists = match segment.kind {
            SymfonyExpressionAccessKind::Property => resolved.has_property(&segment.name),
            SymfonyExpressionAccessKind::Method => resolved.has_method(&segment.name),
        };
        if exists {
            matching.push(Arc::clone(class));
            continue;
        }
        dynamic |= match segment.kind {
            SymfonyExpressionAccessKind::Property => {
                resolved.name.eq_ignore_ascii_case("stdClass") || resolved.has_method("__get")
            }
            SymfonyExpressionAccessKind::Method => resolved.has_method("__call"),
        };
    }
    (matching, dynamic)
}

fn next_expression_classes(
    classes: &[Arc<ClassInfo>],
    segment: &SymfonyExpressionSegment,
    class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
) -> Vec<Arc<ClassInfo>> {
    let mut next = Vec::new();
    for class in classes {
        match segment.kind {
            SymfonyExpressionAccessKind::Property => {
                next.extend(crate::type_engine::type_resolution::resolve_property_types(
                    &segment.name,
                    class,
                    classes,
                    class_loader,
                ))
            }
            SymfonyExpressionAccessKind::Method => {
                if let Some(return_type) = crate::inheritance::resolve_method_return_type(
                    class,
                    &segment.name,
                    class_loader,
                ) {
                    next.extend(
                        crate::type_engine::type_resolution::type_hint_to_classes_typed(
                            &return_type,
                            &class.fqn(),
                            classes,
                            class_loader,
                        ),
                    );
                }
            }
        }
    }
    next
}
