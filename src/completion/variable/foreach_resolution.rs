/// Foreach and destructuring variable type resolution.
///
/// This submodule handles resolving types for variables that appear as:
///
///   - **Foreach value/key variables:** `foreach ($items as $key => $item)`
///     where the iterated expression has a generic iterable type annotation.
///   - **Array/list destructuring:** `[$a, $b] = getUsers()` or
///     `['name' => $name] = $data` where the RHS has a generic iterable
///     or array shape type annotation.
///
/// These functions are self-contained: they receive a [`VarResolutionCtx`]
/// and push resolved [`ResolvedType`] values into a results vector.
use std::collections::HashMap;
use std::sync::Arc;

use crate::php_type::PhpType;
use crate::types::{ClassInfo, ResolvedType};
use crate::util::short_name;

use crate::completion::resolver::VarResolutionCtx;

/// Resolve an expression's structured type via the unified pipeline.
///
/// Wraps `resolve_rhs_expression` + `types_joined` into a single
/// `Option<PhpType>`.  Returns `None` when the unified pipeline
/// produces no results or an empty type string.
pub(crate) fn resolve_expression_type<'b>(
    expr: &'b mago_syntax::ast::Expression<'b>,
    ctx: &VarResolutionCtx<'_>,
) -> Option<PhpType> {
    let resolved = super::rhs_resolution::resolve_rhs_expression(expr, ctx);
    if resolved.is_empty() {
        return None;
    }
    Some(ResolvedType::types_joined(&resolved))
}

// ─── Helpers ────────────────────────────────────────────────────────

// ─── Foreach Resolution ─────────────────────────────────────────────

/// Known interface/class names whose generic parameters describe
/// iteration types in PHP's `foreach`.
const ITERABLE_IFACE_NAMES: &[&str] = &[
    "Iterator",
    "IteratorAggregate",
    "Traversable",
    "ArrayAccess",
    "Enumerable",
];

/// Extract the iterable **value** (element) type from a class's generic
/// annotations.
///
/// When a collection class like `PaymentOptionLocaleCollection` has
/// `@extends Collection<int, PaymentOptionLocale>` or
/// `@implements IteratorAggregate<int, PaymentOptionLocale>`, this
/// function returns `Some("PaymentOptionLocale")`.
///
/// Checks (in order of priority):
/// 1. `implements_generics` for known iterable interfaces
/// 2. `extends_generics` for any parent with generic type args
///
/// Returns `None` when no generic iterable annotation is found or
/// when the element type is a scalar (scalars have no completable
/// members).
pub(in crate::completion) fn extract_iterable_element_type_from_class(
    class: &ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
) -> Option<PhpType> {
    // 1. Check implements_generics for known iterable interfaces.
    for (name, args) in &class.implements_generics {
        let short = short_name(name);
        if ITERABLE_IFACE_NAMES.contains(&short) && !args.is_empty() {
            let value = resolve_own_template_arg(args.last().unwrap(), class);
            if !is_unbounded_template_placeholder(&value) {
                return Some(value);
            }
        }
    }

    // 1b. Check implements_generics for interfaces that transitively
    //     extend a known iterable interface (e.g. `TypedCollection`
    //     extends `IteratorAggregate`).
    for (name, args) in &class.implements_generics {
        let short = short_name(name);
        if !ITERABLE_IFACE_NAMES.contains(&short)
            && !args.is_empty()
            && let Some(iface) = class_loader(name)
            && is_transitive_iterable(&iface, class_loader)
        {
            let value = resolve_own_template_arg(args.last().unwrap(), class);
            if !is_unbounded_template_placeholder(&value) {
                return Some(value);
            }
        }
    }

    // 2. Check extends_generics — common for collection subclasses
    //    like `@extends Collection<int, User>`.
    for (_, args) in &class.extends_generics {
        if !args.is_empty() {
            let value = resolve_own_template_arg(args.last().unwrap(), class);
            if !is_unbounded_template_placeholder(&value) {
                return Some(value);
            }
        }
    }

    // 3. Fall back to the `current()` return type when the class
    //    implements `Iterator` directly (not `IteratorAggregate`) without
    //    a generic annotation. `SimpleXMLElement` is the prototypical
    //    example: it implements `Iterator` with `current(): static`, so
    //    iterating it yields instances of the iterated class itself.
    if class_directly_implements(class, class_loader, "Iterator")
        && let Some(method) = class.get_method("current")
        && let Some(return_type) = &method.return_type
    {
        return Some(return_type.replace_self(&class.fqn()));
    }

    // 4. Fall back to the `offsetGet()` return type when the class
    //    implements `ArrayAccess` directly without a usable generic
    //    annotation (e.g. an unbound `@template` self-reference, or no
    //    docblock generics at all). Mirrors the `current()` fallback
    //    above: `$obj[$k]` invokes `offsetGet`, so its declared return
    //    type is the most precise answer available.
    if class_directly_implements(class, class_loader, "ArrayAccess")
        && let Some(method) = class.get_method("offsetGet")
        && let Some(return_type) = &method.return_type
    {
        return Some(return_type.replace_self(&class.fqn()));
    }

    None
}

/// Resolve a generic argument that references the class's own `@template`
/// parameter (e.g. `T` in `@implements ArrayAccess<int, T>` declared on a
/// class with `@template T of SomeBound`) to its upper bound.
///
/// `implements_generics` / `extends_generics` store a class's generic
/// annotations exactly as written; when an annotation references the same
/// class's own template parameter (rather than a concrete type or a
/// parent's template parameter, which are substituted elsewhere), nothing
/// else resolves it. Without this, the raw template name (e.g. `"T"`)
/// would leak through as if it were a real, unrelated class name.
fn resolve_own_template_arg(value: &PhpType, class: &ClassInfo) -> PhpType {
    if class.template_params.is_empty() {
        return value.clone();
    }
    let subs: HashMap<String, PhpType> = class
        .template_params
        .iter()
        .map(|param| {
            let bound = class
                .template_param_bounds
                .get(param)
                .cloned()
                .unwrap_or_else(PhpType::mixed);
            (param.to_string(), bound)
        })
        .collect();
    value.substitute(&subs)
}

/// Check whether a generic argument is an unbounded template parameter
/// that was substituted with `mixed` as a fallback (no explicit
/// `@implements`/`@extends` generic annotation was given).
///
/// Interfaces like `Iterator<TKey, TValue>` propagate their template
/// params through `@template-extends Traversable<TKey, TValue>` even when
/// the implementing class never annotates concrete types; in that case
/// the merge falls back to substituting each param with `mixed` (see
/// `resolve_class_fully_inner` in `virtual_members/mod.rs`). Treating that
/// placeholder as a "found" element type would shadow the more precise
/// `current()`/`key()` fallback below.
fn is_unbounded_template_placeholder(ty: &PhpType) -> bool {
    matches!(ty, PhpType::Named(name) if name.eq_ignore_ascii_case("mixed"))
}

/// Check whether `class`, or an ancestor reached by walking the `extends`
/// chain, directly declares `implements <iface_name>`.
///
/// Only the `extends` chain is walked (not the interface-extends-interface
/// chain used by [`is_transitive_iterable`]) because `Iterator` and
/// `ArrayAccess` are always implemented by a concrete class, never
/// re-declared through another interface.
fn class_directly_implements(
    class: &ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
    iface_name: &str,
) -> bool {
    if class
        .interfaces
        .iter()
        .any(|i| short_name(i).eq_ignore_ascii_case(iface_name))
    {
        return true;
    }

    let mut visited = std::collections::HashSet::new();
    visited.insert(class.name.to_string());
    let mut parent_name = class.parent_class.as_ref().map(|a| a.to_string());
    while let Some(name) = parent_name {
        if !visited.insert(name.clone()) {
            break;
        }
        let Some(parent) = class_loader(&name) else {
            break;
        };
        if parent
            .interfaces
            .iter()
            .any(|i| short_name(i).eq_ignore_ascii_case(iface_name))
        {
            return true;
        }
        parent_name = parent.parent_class.as_ref().map(|a| a.to_string());
    }
    false
}

/// Extract the iterable **key** type from a class's generic annotations.
///
/// Mirrors `extract_iterable_element_type_from_class` but returns the
/// first generic parameter (key) instead of the last (value).  Only
/// returns a key type when the iterable interface has 2+ generic
/// parameters (so `list<User>` returns `None` → fallback to `int`).
pub(in crate::completion) fn extract_iterable_key_type_from_class(
    class: &ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
) -> Option<PhpType> {
    // 1. Check implements_generics for known iterable interfaces.
    for (name, args) in &class.implements_generics {
        let short = short_name(name);
        if ITERABLE_IFACE_NAMES.contains(&short) && args.len() >= 2 {
            let key = resolve_own_template_arg(&args[0], class);
            if !is_unbounded_template_placeholder(&key) {
                return Some(key);
            }
        }
    }

    // 1b. Transitive iterable interfaces.
    for (name, args) in &class.implements_generics {
        let short = short_name(name);
        if !ITERABLE_IFACE_NAMES.contains(&short)
            && args.len() >= 2
            && let Some(iface) = class_loader(name)
            && is_transitive_iterable(&iface, class_loader)
        {
            let key = resolve_own_template_arg(&args[0], class);
            if !is_unbounded_template_placeholder(&key) {
                return Some(key);
            }
        }
    }

    // 2. Check extends_generics.
    for (_, args) in &class.extends_generics {
        if args.len() >= 2 {
            let key = resolve_own_template_arg(&args[0], class);
            if !is_unbounded_template_placeholder(&key) {
                return Some(key);
            }
        }
    }

    // 3. Fall back to the `key()` return type when the class implements
    //    `Iterator` directly without a generic annotation. Mirrors the
    //    `current()` fallback in `extract_iterable_element_type_from_class`.
    if class_directly_implements(class, class_loader, "Iterator")
        && let Some(method) = class.get_method("key")
        && let Some(return_type) = &method.return_type
    {
        return Some(return_type.replace_self(&class.fqn()));
    }

    None
}

/// Check whether an interface transitively extends a known iterable
/// interface (e.g. `TypedCollection extends IteratorAggregate`).
fn is_transitive_iterable(
    iface: &ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
) -> bool {
    let mut visited = std::collections::HashSet::new();
    is_transitive_iterable_inner(iface, class_loader, &mut visited)
}

fn is_transitive_iterable_inner(
    iface: &ClassInfo,
    class_loader: &dyn Fn(&str) -> Option<Arc<ClassInfo>>,
    visited: &mut std::collections::HashSet<String>,
) -> bool {
    // Recurse through a parent name, guarding against cyclic hierarchies.
    let recurse = |name: &str, visited: &mut std::collections::HashSet<String>| -> bool {
        if !visited.insert(name.to_string()) {
            return false;
        }
        class_loader(name)
            .is_some_and(|parent| is_transitive_iterable_inner(&parent, class_loader, visited))
    };

    // Check direct interfaces, then recurse into any that are not
    // themselves a known iterable so a two-hop ancestor is still found.
    for parent in &iface.interfaces {
        if ITERABLE_IFACE_NAMES.contains(&short_name(parent)) {
            return true;
        }
        if recurse(parent, visited) {
            return true;
        }
    }
    // Check extends_generics for the interface-extends-interface pattern.
    for (name, _) in &iface.extends_generics {
        if ITERABLE_IFACE_NAMES.contains(&short_name(name)) {
            return true;
        }
        if recurse(name, visited) {
            return true;
        }
    }
    // Check parent class (interfaces use `parent_class` for extends).
    if let Some(ref parent_name) = iface.parent_class {
        if ITERABLE_IFACE_NAMES.contains(&short_name(parent_name)) {
            return true;
        }
        if recurse(parent_name, visited) {
            return true;
        }
    }
    false
}
