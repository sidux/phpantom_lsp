//! Static reader for Laravel `config/*.php` **values**.
//!
//! The sibling [`config_keys`](super::config_keys) module walks a config
//! file to record *key* spans (powering go-to-definition and references on
//! `config('a.b.c')` strings).  This module answers a different question:
//! *what value sits at a given dotted config path?*  It parses the `return
//! [...]` array literal into an owned [`ConfigNode`] tree, then lets callers
//! navigate by path and classify leaf expressions.
//!
//! Values are deliberately kept as a small, honest set of shapes rather than
//! being evaluated.  A config value can be a string literal, a `::class`
//! constant, a ternary over several literals, an `env('KEY', <default>)` read
//! whose default is known but which a runtime environment variable may
//! override, or something we cannot resolve statically at all.  Consumers
//! decide how much uncertainty they can tolerate (see the auth-user model
//! resolver, which anchors on `env()` defaults but records that the value
//! could be overridden so it can widen the result to the framework contract).
//!
//! This is the first consumer of static config-value reading; it is written
//! to be reusable (e.g. `Storage::disk()` reading `config/filesystems.php`).

use bumpalo::Bump;
use mago_database::file::FileId;
use mago_syntax::ast::*;

use crate::atom::bytes_to_str;

/// A statically-classified Laravel config value.
///
/// Values are never evaluated.  `env()` conditions, variables, and arbitrary
/// function calls are opaque at analysis time, so anything we cannot pin to a
/// literal collapses to [`ConfigValue::Dynamic`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigValue {
    /// A string literal, e.g. `'web'` or `'users'`.
    Str(String),
    /// A `::class` constant, e.g. `App\Models\User::class`.  The stored name
    /// is exactly as written in the config file (short or fully-qualified);
    /// callers resolve it against use-statements / the classmap.
    ClassString(String),
    /// A ternary or short-ternary over two or more sub-values, e.g.
    /// `env('is_admin') ? User::class : Admin::class`.  We do not evaluate the
    /// condition; the honest value is *one of* the arms.
    OneOf(Vec<ConfigValue>),
    /// `env('KEY', <default>)`: the default argument is statically known, but
    /// an environment variable may override it at runtime.
    EnvDefault(Box<ConfigValue>),
    /// Not statically resolvable (bare `env('KEY')`, a variable, a call, or a
    /// shape we do not recognize).
    Dynamic,
}

impl ConfigValue {
    /// Flatten to the set of string literals this value may take, plus a flag
    /// indicating whether a runtime-dynamic branch was encountered (an
    /// `env()` override, a bare `env()`, or an unrecognized expression).
    ///
    /// Used for scalar config such as guard and provider names.
    pub(crate) fn as_strings(&self) -> (Vec<String>, bool) {
        let mut out = Vec::new();
        let mut dynamic = false;
        self.collect_strings(&mut out, &mut dynamic);
        (out, dynamic)
    }

    /// Flatten to the set of class names this value may take, plus a flag
    /// indicating whether a runtime-dynamic branch was encountered.
    ///
    /// Used for the `providers.*.model` config.  A string literal that looks
    /// like a class reference (contains a namespace separator) is accepted as
    /// a class too, since a handful of configs write the model as a string.
    pub(crate) fn as_classes(&self) -> (Vec<String>, bool) {
        let mut out = Vec::new();
        let mut dynamic = false;
        self.collect_classes(&mut out, &mut dynamic);
        (out, dynamic)
    }

    fn collect_strings(&self, out: &mut Vec<String>, dynamic: &mut bool) {
        match self {
            ConfigValue::Str(s) => push_unique(out, s),
            ConfigValue::ClassString(_) => *dynamic = true,
            ConfigValue::OneOf(arms) => {
                for arm in arms {
                    arm.collect_strings(out, dynamic);
                }
            }
            ConfigValue::EnvDefault(inner) => {
                inner.collect_strings(out, dynamic);
                *dynamic = true;
            }
            ConfigValue::Dynamic => *dynamic = true,
        }
    }

    fn collect_classes(&self, out: &mut Vec<String>, dynamic: &mut bool) {
        match self {
            ConfigValue::ClassString(name) => push_unique(out, name),
            ConfigValue::Str(s) if s.contains('\\') => push_unique(out, s),
            ConfigValue::Str(_) => *dynamic = true,
            ConfigValue::OneOf(arms) => {
                for arm in arms {
                    arm.collect_classes(out, dynamic);
                }
            }
            ConfigValue::EnvDefault(inner) => {
                inner.collect_classes(out, dynamic);
                *dynamic = true;
            }
            ConfigValue::Dynamic => *dynamic = true,
        }
    }
}

fn push_unique(out: &mut Vec<String>, value: &str) {
    if !out.iter().any(|existing| existing == value) {
        out.push(value.to_string());
    }
}

/// An owned tree of a parsed `config/*.php` array literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigNode {
    /// A nested array, ordered by declaration so callers can enumerate keys
    /// for fan-out (e.g. every configured guard).
    Array(Vec<(String, ConfigNode)>),
    /// A leaf value.
    Leaf(ConfigValue),
}

impl ConfigNode {
    /// Navigate to a nested node by dotted path segments.
    pub(crate) fn get(&self, path: &[&str]) -> Option<&ConfigNode> {
        let mut node = self;
        for segment in path {
            let ConfigNode::Array(entries) = node else {
                return None;
            };
            node = entries
                .iter()
                .find(|(key, _)| key == segment)
                .map(|(_, child)| child)?;
        }
        Some(node)
    }

    /// The immediate child keys of an array node (empty for leaves).  Used to
    /// fan out over every configured guard or provider when an intermediate
    /// hop cannot be resolved to a single choice.
    pub(crate) fn child_keys(&self) -> Vec<String> {
        match self {
            ConfigNode::Array(entries) => entries.iter().map(|(key, _)| key.clone()).collect(),
            ConfigNode::Leaf(_) => Vec::new(),
        }
    }

    /// The [`ConfigValue`] at a path, if it resolves to a leaf.
    pub(crate) fn value_at(&self, path: &[&str]) -> Option<&ConfigValue> {
        match self.get(path)? {
            ConfigNode::Leaf(value) => Some(value),
            ConfigNode::Array(_) => None,
        }
    }
}

/// Parse a `config/*.php` file's returned array into an owned [`ConfigNode`].
///
/// Handles both `return [...]` and the `$config = [...]; return $config;`
/// pattern, mirroring [`config_keys`](super::config_keys)'s declaration
/// walker.
pub(crate) fn parse_config_tree(content: &str) -> Option<ConfigNode> {
    let arena = Bump::new();
    let file_id = FileId::new(b"input.php");
    let program = mago_syntax::parser::parse_file_content(&arena, file_id, content.as_bytes());

    let mut returned_var_name: Option<String> = None;
    let mut return_expr: Option<&Expression<'_>> = None;

    for stmt in program.statements.iter() {
        if let Statement::Return(ret) = stmt {
            if let Some(val) = ret.value {
                match val {
                    Expression::Variable(Variable::Direct(dv)) => {
                        returned_var_name = Some(bytes_to_str(dv.name).to_string());
                    }
                    _ => return_expr = Some(val),
                }
            }
            break;
        }
    }

    if let Some(expr) = return_expr {
        return Some(node_from_expr(expr, content));
    }

    let var_name = returned_var_name?;
    for stmt in program.statements.iter() {
        if let Statement::Expression(expr_stmt) = stmt
            && let Expression::Assignment(assign) = expr_stmt.expression
            && let Expression::Variable(Variable::Direct(dv)) = assign.lhs
            && dv.name == var_name.as_bytes()
        {
            return Some(node_from_expr(assign.rhs, content));
        }
    }

    None
}

/// Build a [`ConfigNode`] from an arbitrary expression: arrays become
/// [`ConfigNode::Array`], everything else is classified as a leaf value.
fn node_from_expr(expr: &Expression<'_>, content: &str) -> ConfigNode {
    match expr {
        Expression::Parenthesized(p) => node_from_expr(p.expression, content),
        Expression::Array(arr) => array_node(arr.elements.iter(), content),
        Expression::LegacyArray(arr) => array_node(arr.elements.iter(), content),
        other => ConfigNode::Leaf(classify_value(other, content)),
    }
}

fn array_node<'a>(
    elements: impl Iterator<Item = &'a ArrayElement<'a>>,
    content: &str,
) -> ConfigNode {
    let mut entries = Vec::new();
    for element in elements {
        let ArrayElement::KeyValue(kv) = element else {
            continue;
        };
        let Some((key_text, _, _)) = super::helpers::extract_string_literal(kv.key, content) else {
            continue;
        };
        entries.push((key_text.to_string(), node_from_expr(kv.value, content)));
    }
    ConfigNode::Array(entries)
}

/// Classify a leaf value expression into a [`ConfigValue`].
fn classify_value(expr: &Expression<'_>, content: &str) -> ConfigValue {
    match expr {
        Expression::Parenthesized(p) => classify_value(p.expression, content),
        Expression::Literal(literal::Literal::String(_)) => {
            match super::helpers::extract_string_literal(expr, content) {
                Some((text, _, _)) => ConfigValue::Str(text.to_string()),
                None => ConfigValue::Dynamic,
            }
        }
        Expression::Access(Access::ClassConstant(cca)) => classify_class_constant(cca),
        Expression::Conditional(cond) => {
            // `a ? b : c` and short `a ?: c`.  We never evaluate the
            // condition; the value is one of the branches.  For `?:` the
            // "then" branch is the condition itself.
            let then_expr = cond.then.unwrap_or(cond.condition);
            let mut arms = Vec::new();
            flatten_one_of(classify_value(then_expr, content), &mut arms);
            flatten_one_of(classify_value(cond.r#else, content), &mut arms);
            ConfigValue::OneOf(arms)
        }
        Expression::Call(Call::Function(fc)) => classify_call(fc, content),
        _ => ConfigValue::Dynamic,
    }
}

fn classify_class_constant(cca: &ClassConstantAccess<'_>) -> ConfigValue {
    let is_class = matches!(
        &cca.constant,
        ClassLikeConstantSelector::Identifier(ident)
            if bytes_to_str(ident.value).eq_ignore_ascii_case("class")
    );
    if is_class && let Expression::Identifier(id) = cca.class {
        let name = bytes_to_str(id.value()).to_string();
        if !name.is_empty() {
            return ConfigValue::ClassString(name);
        }
    }
    ConfigValue::Dynamic
}

fn classify_call(fc: &FunctionCall<'_>, content: &str) -> ConfigValue {
    let Expression::Identifier(ident) = fc.function else {
        return ConfigValue::Dynamic;
    };
    if !ident.value().eq_ignore_ascii_case(b"env") {
        return ConfigValue::Dynamic;
    }
    // `env('KEY', <default>)` anchors on the default argument; a bare
    // `env('KEY')` has no static value.
    let default_arg = fc.argument_list.arguments.iter().nth(1);
    match default_arg {
        Some(arg) => ConfigValue::EnvDefault(Box::new(classify_value(arg.value(), content))),
        None => ConfigValue::Dynamic,
    }
}

/// Append a classified value into a `OneOf` accumulator, flattening nested
/// `OneOf`s so ternary chains stay a single flat set of arms.
fn flatten_one_of(value: ConfigValue, out: &mut Vec<ConfigValue>) {
    match value {
        ConfigValue::OneOf(arms) => {
            for arm in arms {
                flatten_one_of(arm, out);
            }
        }
        other => out.push(other),
    }
}

#[cfg(test)]
#[path = "config_values_tests.rs"]
mod tests;
