/// Folding range handler for `textDocument/foldingRange`.
///
/// Parses the PHP source, walks the AST to collect foldable regions
/// (class bodies, function/method bodies, closures, arrays, control-flow
/// blocks, argument/parameter lists), and scans trivia for doc-block and
/// consecutive single-line comment ranges.
use bumpalo::Bump;
use mago_span::HasSpan;
use mago_syntax::ast::*;
use tower_lsp::lsp_types::{FoldingRange, FoldingRangeKind};

use crate::Backend;
use crate::util::offset_to_position;

// ─── Public entry point ─────────────────────────────────────────────────────

impl Backend {
    /// Compute folding ranges for the given file content.
    ///
    /// Re-parses the source with `mago_syntax` (the raw AST is not cached)
    /// and walks every statement/expression to emit `FoldingRange` entries.
    pub fn handle_folding_range(&self, content: &str) -> Option<Vec<FoldingRange>> {
        let arena = Bump::new();
        let file_id = mago_database::file::FileId::new(b"input.php");
        let program = mago_syntax::parser::parse_file_content(&arena, file_id, content.as_bytes());

        let mut ranges: Vec<FoldingRange> = Vec::new();

        // ── AST walk ──
        for stmt in program.statements.iter() {
            collect_from_statement(stmt, content, &mut ranges);
        }

        // ── Trivia (comments) ──
        collect_comment_ranges(&program.trivia, content, &mut ranges);

        // Filter out single-line ranges and sort by start position.
        ranges.retain(|r| r.start_line < r.end_line);
        ranges.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then(a.end_line.cmp(&b.end_line))
        });

        Some(ranges)
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Build a `FoldingRange` from byte-offset start/end (of the opening/closing
/// delimiters).  `kind` is `None` for code regions.
fn range_from_offsets(
    content: &str,
    start_offset: u32,
    end_offset: u32,
    kind: Option<FoldingRangeKind>,
) -> FoldingRange {
    let start_pos = offset_to_position(content, start_offset as usize);
    let end_pos = offset_to_position(content, end_offset as usize);
    FoldingRange {
        start_line: start_pos.line,
        start_character: Some(start_pos.character),
        end_line: end_pos.line,
        end_character: Some(end_pos.character),
        kind,
        collapsed_text: None,
    }
}

/// Emit a folding range for a `Block` (left-brace … right-brace).
fn emit_block(block: &Block<'_>, content: &str, ranges: &mut Vec<FoldingRange>) {
    ranges.push(range_from_offsets(
        content,
        block.left_brace.start.offset,
        block.right_brace.end.offset,
        None,
    ));
}

/// Emit a folding range from two `mago_span::Span` values representing the
/// opening and closing delimiters.
fn emit_brace_pair(
    left: mago_span::Span,
    right: mago_span::Span,
    content: &str,
    ranges: &mut Vec<FoldingRange>,
) {
    ranges.push(range_from_offsets(
        content,
        left.start.offset,
        right.end.offset,
        None,
    ));
}

/// Emit a folding range for a parenthesised list (argument list, parameter
/// list) only when it spans multiple lines.
fn emit_paren_pair(
    left: mago_span::Span,
    right: mago_span::Span,
    content: &str,
    ranges: &mut Vec<FoldingRange>,
) {
    let start_pos = offset_to_position(content, left.start.offset as usize);
    let end_pos = offset_to_position(content, right.end.offset as usize);
    if start_pos.line < end_pos.line {
        ranges.push(FoldingRange {
            start_line: start_pos.line,
            start_character: Some(start_pos.character),
            end_line: end_pos.line,
            end_character: Some(end_pos.character),
            kind: None,
            collapsed_text: None,
        });
    }
}

// ─── Statement walker ───────────────────────────────────────────────────────

fn collect_from_statement(stmt: &Statement<'_>, content: &str, ranges: &mut Vec<FoldingRange>) {
    match stmt {
        Statement::Namespace(ns) => {
            // Brace-delimited namespace body.
            if let NamespaceBody::BraceDelimited(block) = &ns.body {
                emit_block(block, content, ranges);
                for inner in block.statements.iter() {
                    collect_from_statement(inner, content, ranges);
                }
            } else {
                for inner in ns.statements().iter() {
                    collect_from_statement(inner, content, ranges);
                }
            }
        }

        Statement::Class(class) => {
            emit_brace_pair(class.left_brace, class.right_brace, content, ranges);
            for member in class.members.iter() {
                collect_from_class_member(member, content, ranges);
            }
        }

        Statement::Interface(iface) => {
            emit_brace_pair(iface.left_brace, iface.right_brace, content, ranges);
            for member in iface.members.iter() {
                collect_from_class_member(member, content, ranges);
            }
        }

        Statement::Trait(trait_def) => {
            emit_brace_pair(trait_def.left_brace, trait_def.right_brace, content, ranges);
            for member in trait_def.members.iter() {
                collect_from_class_member(member, content, ranges);
            }
        }

        Statement::Enum(enum_def) => {
            emit_brace_pair(enum_def.left_brace, enum_def.right_brace, content, ranges);
            for member in enum_def.members.iter() {
                collect_from_class_member(member, content, ranges);
            }
        }

        Statement::Function(func) => {
            emit_block(&func.body, content, ranges);
            // Parameter list.
            emit_paren_pair(
                func.parameter_list.left_parenthesis,
                func.parameter_list.right_parenthesis,
                content,
                ranges,
            );
            for inner in func.body.statements.iter() {
                collect_from_statement(inner, content, ranges);
            }
        }

        Statement::If(if_stmt) => {
            collect_from_if(if_stmt, content, ranges);
        }

        Statement::Switch(switch_stmt) => {
            collect_from_expression(switch_stmt.expression, content, ranges);
            match &switch_stmt.body {
                SwitchBody::BraceDelimited(body) => {
                    emit_brace_pair(body.left_brace, body.right_brace, content, ranges);
                    for case in body.cases.iter() {
                        for inner in case.statements().iter() {
                            collect_from_statement(inner, content, ranges);
                        }
                    }
                }
                SwitchBody::ColonDelimited(body) => {
                    for case in body.cases.iter() {
                        for inner in case.statements().iter() {
                            collect_from_statement(inner, content, ranges);
                        }
                    }
                }
            }
        }

        Statement::While(while_stmt) => {
            collect_from_expression(while_stmt.condition, content, ranges);
            match &while_stmt.body {
                WhileBody::Statement(inner) => {
                    collect_from_block_statement(inner, content, ranges);
                }
                WhileBody::ColonDelimited(_body) => {
                    // Colon-delimited while loops don't have braces.
                    for inner in while_stmt.body.statements().iter() {
                        collect_from_statement(inner, content, ranges);
                    }
                }
            }
        }

        Statement::For(for_stmt) => {
            for expr in for_stmt.initializations.iter() {
                collect_from_expression(expr, content, ranges);
            }
            for expr in for_stmt.conditions.iter() {
                collect_from_expression(expr, content, ranges);
            }
            for expr in for_stmt.increments.iter() {
                collect_from_expression(expr, content, ranges);
            }
            match &for_stmt.body {
                ForBody::Statement(inner) => {
                    collect_from_block_statement(inner, content, ranges);
                }
                ForBody::ColonDelimited(_body) => {
                    for inner in for_stmt.body.statements().iter() {
                        collect_from_statement(inner, content, ranges);
                    }
                }
            }
        }

        Statement::Foreach(foreach_stmt) => {
            collect_from_expression(foreach_stmt.expression, content, ranges);
            match &foreach_stmt.body {
                ForeachBody::Statement(inner) => {
                    collect_from_block_statement(inner, content, ranges);
                }
                ForeachBody::ColonDelimited(_body) => {
                    for inner in foreach_stmt.body.statements().iter() {
                        collect_from_statement(inner, content, ranges);
                    }
                }
            }
        }

        Statement::DoWhile(do_while) => {
            collect_from_block_statement(do_while.statement, content, ranges);
            collect_from_expression(do_while.condition, content, ranges);
        }

        Statement::Try(try_stmt) => {
            emit_block(&try_stmt.block, content, ranges);
            for inner in try_stmt.block.statements.iter() {
                collect_from_statement(inner, content, ranges);
            }
            for catch in try_stmt.catch_clauses.iter() {
                emit_block(&catch.block, content, ranges);
                for inner in catch.block.statements.iter() {
                    collect_from_statement(inner, content, ranges);
                }
            }
            if let Some(ref finally) = try_stmt.finally_clause {
                emit_block(&finally.block, content, ranges);
                for inner in finally.block.statements.iter() {
                    collect_from_statement(inner, content, ranges);
                }
            }
        }

        Statement::Block(block) => {
            emit_block(block, content, ranges);
            for inner in block.statements.iter() {
                collect_from_statement(inner, content, ranges);
            }
        }

        Statement::Expression(expr_stmt) => {
            collect_from_expression(expr_stmt.expression, content, ranges);
        }

        Statement::Return(ret) => {
            if let Some(val) = ret.value {
                collect_from_expression(val, content, ranges);
            }
        }

        Statement::Echo(echo) => {
            for expr in echo.values.iter() {
                collect_from_expression(expr, content, ranges);
            }
        }

        Statement::Declare(declare) => match &declare.body {
            DeclareBody::Statement(inner) => {
                collect_from_statement(inner, content, ranges);
            }
            DeclareBody::ColonDelimited(body) => {
                for s in body.statements.iter() {
                    collect_from_statement(s, content, ranges);
                }
            }
        },

        Statement::Constant(constant) => {
            for item in constant.items.iter() {
                collect_from_expression(item.value, content, ranges);
            }
        }

        Statement::Unset(unset_stmt) => {
            for val in unset_stmt.values.iter() {
                collect_from_expression(val, content, ranges);
            }
        }

        Statement::EchoTag(echo_tag) => {
            for expr in echo_tag.values.iter() {
                collect_from_expression(expr, content, ranges);
            }
        }

        // Leaves or constructs that don't produce folding ranges.
        _ => {}
    }
}

/// If the statement is a `Block`, emit it and recurse; otherwise just recurse.
fn collect_from_block_statement(
    stmt: &Statement<'_>,
    content: &str,
    ranges: &mut Vec<FoldingRange>,
) {
    if let Statement::Block(block) = stmt {
        emit_block(block, content, ranges);
        for inner in block.statements.iter() {
            collect_from_statement(inner, content, ranges);
        }
    } else {
        collect_from_statement(stmt, content, ranges);
    }
}

// ─── If statement ───────────────────────────────────────────────────────────

fn collect_from_if(if_stmt: &If<'_>, content: &str, ranges: &mut Vec<FoldingRange>) {
    collect_from_expression(if_stmt.condition, content, ranges);
    match &if_stmt.body {
        IfBody::Statement(body) => {
            collect_from_block_statement(body.statement, content, ranges);
            for else_if in body.else_if_clauses.iter() {
                collect_from_expression(else_if.condition, content, ranges);
                collect_from_block_statement(else_if.statement, content, ranges);
            }
            if let Some(ref else_clause) = body.else_clause {
                collect_from_block_statement(else_clause.statement, content, ranges);
            }
        }
        IfBody::ColonDelimited(body) => {
            for inner in body.statements.iter() {
                collect_from_statement(inner, content, ranges);
            }
            for else_if in body.else_if_clauses.iter() {
                collect_from_expression(else_if.condition, content, ranges);
                for inner in else_if.statements.iter() {
                    collect_from_statement(inner, content, ranges);
                }
            }
            if let Some(ref else_clause) = body.else_clause {
                for inner in else_clause.statements.iter() {
                    collect_from_statement(inner, content, ranges);
                }
            }
        }
    }
}

// ─── Class member walker ────────────────────────────────────────────────────

fn collect_from_class_member(
    member: &class_like::member::ClassLikeMember<'_>,
    content: &str,
    ranges: &mut Vec<FoldingRange>,
) {
    match member {
        class_like::member::ClassLikeMember::Method(method) => {
            if let class_like::method::MethodBody::Concrete(block) = &method.body {
                emit_block(block, content, ranges);
                for inner in block.statements.iter() {
                    collect_from_statement(inner, content, ranges);
                }
            }
            // Parameter list.
            emit_paren_pair(
                method.parameter_list.left_parenthesis,
                method.parameter_list.right_parenthesis,
                content,
                ranges,
            );
        }
        class_like::member::ClassLikeMember::TraitUse(trait_use) => {
            if let class_like::trait_use::TraitUseSpecification::Concrete(concrete) =
                &trait_use.specification
            {
                emit_brace_pair(concrete.left_brace, concrete.right_brace, content, ranges);
            }
        }
        class_like::member::ClassLikeMember::Property(prop) => {
            // Property hooks (PHP 8.4) can have bodies.
            collect_from_property_hooks(prop, content, ranges);
        }
        // Constants, enum cases — no sub-blocks to fold.
        _ => {}
    }
}

/// Collect folding ranges from property hooks if present.
fn collect_from_property_hooks(
    prop: &class_like::property::Property<'_>,
    content: &str,
    ranges: &mut Vec<FoldingRange>,
) {
    let hook_list = match prop {
        class_like::property::Property::Hooked(h) => &h.hook_list,
        class_like::property::Property::Plain(_) => return,
    };
    emit_brace_pair(hook_list.left_brace, hook_list.right_brace, content, ranges);
    for hook in hook_list.hooks.iter() {
        if let class_like::property::PropertyHookBody::Concrete(concrete) = &hook.body
            && let class_like::property::PropertyHookConcreteBody::Block(block) = concrete
        {
            emit_block(block, content, ranges);
            for inner in block.statements.iter() {
                collect_from_statement(inner, content, ranges);
            }
        }
    }
}

// ─── Expression walker ──────────────────────────────────────────────────────

fn collect_from_expression(expr: &Expression<'_>, content: &str, ranges: &mut Vec<FoldingRange>) {
    match expr {
        Expression::Closure(closure) => {
            emit_block(&closure.body, content, ranges);
            emit_paren_pair(
                closure.parameter_list.left_parenthesis,
                closure.parameter_list.right_parenthesis,
                content,
                ranges,
            );
            for inner in closure.body.statements.iter() {
                collect_from_statement(inner, content, ranges);
            }
        }

        Expression::ArrowFunction(arrow) => {
            // Arrow functions don't have braces, but the expression may
            // span multiple lines.
            let arrow_span = arrow.span();
            let start_pos = offset_to_position(content, arrow_span.start.offset as usize);
            let end_pos = offset_to_position(content, arrow_span.end.offset as usize);
            if start_pos.line < end_pos.line {
                ranges.push(FoldingRange {
                    start_line: start_pos.line,
                    start_character: Some(start_pos.character),
                    end_line: end_pos.line,
                    end_character: Some(end_pos.character),
                    kind: None,
                    collapsed_text: None,
                });
            }
            collect_from_expression(arrow.expression, content, ranges);
        }

        Expression::Array(array) => {
            emit_brace_pair(array.left_bracket, array.right_bracket, content, ranges);
            for elem in array.elements.iter() {
                collect_from_array_element(elem, content, ranges);
            }
        }

        Expression::LegacyArray(array) => {
            emit_paren_pair(
                array.left_parenthesis,
                array.right_parenthesis,
                content,
                ranges,
            );
            for elem in array.elements.iter() {
                collect_from_array_element(elem, content, ranges);
            }
        }

        Expression::Match(match_expr) => {
            emit_brace_pair(
                match_expr.left_brace,
                match_expr.right_brace,
                content,
                ranges,
            );
            collect_from_expression(match_expr.expression, content, ranges);
            for arm in match_expr.arms.iter() {
                match arm {
                    MatchArm::Expression(arm) => {
                        for cond in arm.conditions.iter() {
                            collect_from_expression(cond, content, ranges);
                        }
                        collect_from_expression(arm.expression, content, ranges);
                    }
                    MatchArm::Default(arm) => {
                        collect_from_expression(arm.expression, content, ranges);
                    }
                }
            }
        }

        Expression::Call(call) => {
            collect_from_call(call, content, ranges);
        }

        Expression::Instantiation(inst) => {
            collect_from_expression(inst.class, content, ranges);
            if let Some(ref arg_list) = inst.argument_list {
                emit_paren_pair(
                    arg_list.left_parenthesis,
                    arg_list.right_parenthesis,
                    content,
                    ranges,
                );
                for arg in arg_list.arguments.iter() {
                    collect_from_expression(arg.value(), content, ranges);
                }
            }
        }

        Expression::AnonymousClass(anon) => {
            emit_brace_pair(anon.left_brace, anon.right_brace, content, ranges);
            if let Some(ref arg_list) = anon.argument_list {
                emit_paren_pair(
                    arg_list.left_parenthesis,
                    arg_list.right_parenthesis,
                    content,
                    ranges,
                );
                for arg in arg_list.arguments.iter() {
                    collect_from_expression(arg.value(), content, ranges);
                }
            }
            for member in anon.members.iter() {
                collect_from_class_member(member, content, ranges);
            }
        }

        Expression::Assignment(assign) => {
            collect_from_expression(assign.lhs, content, ranges);
            collect_from_expression(assign.rhs, content, ranges);
        }

        Expression::Binary(bin) => {
            collect_from_expression(bin.lhs, content, ranges);
            collect_from_expression(bin.rhs, content, ranges);
        }

        Expression::UnaryPrefix(u) => {
            collect_from_expression(u.operand, content, ranges);
        }

        Expression::UnaryPostfix(u) => {
            collect_from_expression(u.operand, content, ranges);
        }

        Expression::Parenthesized(p) => {
            collect_from_expression(p.expression, content, ranges);
        }

        Expression::Conditional(cond) => {
            collect_from_expression(cond.condition, content, ranges);
            if let Some(then_expr) = cond.then {
                collect_from_expression(then_expr, content, ranges);
            }
            collect_from_expression(cond.r#else, content, ranges);
        }

        Expression::ArrayAccess(access) => {
            collect_from_expression(access.array, content, ranges);
            collect_from_expression(access.index, content, ranges);
        }

        Expression::Access(access) => match access {
            Access::Property(pa) => collect_from_expression(pa.object, content, ranges),
            Access::NullSafeProperty(pa) => collect_from_expression(pa.object, content, ranges),
            Access::StaticProperty(spa) => collect_from_expression(spa.class, content, ranges),
            Access::ClassConstant(cca) => collect_from_expression(cca.class, content, ranges),
        },

        Expression::Throw(throw_expr) => {
            collect_from_expression(throw_expr.exception, content, ranges);
        }

        Expression::Clone(clone) => {
            collect_from_expression(clone.object, content, ranges);
        }

        Expression::Yield(yield_expr) => match yield_expr {
            Yield::Value(yv) => {
                if let Some(value) = yv.value {
                    collect_from_expression(value, content, ranges);
                }
            }
            Yield::Pair(yp) => {
                collect_from_expression(yp.key, content, ranges);
                collect_from_expression(yp.value, content, ranges);
            }
            Yield::From(yf) => {
                collect_from_expression(yf.iterator, content, ranges);
            }
        },

        Expression::List(list) => {
            for elem in list.elements.iter() {
                collect_from_array_element(elem, content, ranges);
            }
        }

        Expression::Construct(construct) => {
            collect_from_construct(construct, content, ranges);
        }

        Expression::Pipe(pipe) => {
            collect_from_expression(pipe.input, content, ranges);
            collect_from_expression(pipe.callable, content, ranges);
        }

        Expression::CompositeString(cs) => {
            for part in cs.parts() {
                match part {
                    StringPart::Expression(expr) => {
                        collect_from_expression(expr, content, ranges);
                    }
                    StringPart::BracedExpression(braced) => {
                        collect_from_expression(braced.expression, content, ranges);
                    }
                    StringPart::Literal(_) => {}
                }
            }
        }

        // Leaves: literals, variables, identifiers, magic constants, etc.
        _ => {}
    }
}

/// Process call expressions and emit folding ranges for multi-line argument
/// lists.
fn collect_from_call(call: &Call<'_>, content: &str, ranges: &mut Vec<FoldingRange>) {
    match call {
        Call::Function(fc) => {
            collect_from_expression(fc.function, content, ranges);
            emit_paren_pair(
                fc.argument_list.left_parenthesis,
                fc.argument_list.right_parenthesis,
                content,
                ranges,
            );
            for arg in fc.argument_list.arguments.iter() {
                collect_from_expression(arg.value(), content, ranges);
            }
        }
        Call::Method(mc) => {
            collect_from_expression(mc.object, content, ranges);
            emit_paren_pair(
                mc.argument_list.left_parenthesis,
                mc.argument_list.right_parenthesis,
                content,
                ranges,
            );
            for arg in mc.argument_list.arguments.iter() {
                collect_from_expression(arg.value(), content, ranges);
            }
        }
        Call::NullSafeMethod(mc) => {
            collect_from_expression(mc.object, content, ranges);
            emit_paren_pair(
                mc.argument_list.left_parenthesis,
                mc.argument_list.right_parenthesis,
                content,
                ranges,
            );
            for arg in mc.argument_list.arguments.iter() {
                collect_from_expression(arg.value(), content, ranges);
            }
        }
        Call::StaticMethod(mc) => {
            collect_from_expression(mc.class, content, ranges);
            emit_paren_pair(
                mc.argument_list.left_parenthesis,
                mc.argument_list.right_parenthesis,
                content,
                ranges,
            );
            for arg in mc.argument_list.arguments.iter() {
                collect_from_expression(arg.value(), content, ranges);
            }
        }
    }
}

fn collect_from_array_element(
    elem: &array::ArrayElement<'_>,
    content: &str,
    ranges: &mut Vec<FoldingRange>,
) {
    match elem {
        array::ArrayElement::KeyValue(kv) => {
            collect_from_expression(kv.key, content, ranges);
            collect_from_expression(kv.value, content, ranges);
        }
        array::ArrayElement::Value(v) => {
            collect_from_expression(v.value, content, ranges);
        }
        array::ArrayElement::Variadic(v) => {
            collect_from_expression(v.value, content, ranges);
        }
        array::ArrayElement::Missing(_) => {}
    }
}

fn collect_from_construct(
    construct: &construct::Construct<'_>,
    content: &str,
    ranges: &mut Vec<FoldingRange>,
) {
    match construct {
        construct::Construct::Print(print) => {
            collect_from_expression(print.value, content, ranges);
        }
        construct::Construct::Exit(exit) => {
            if let Some(ref args) = exit.arguments {
                for arg in args.arguments.iter() {
                    collect_from_expression(arg.value(), content, ranges);
                }
            }
        }
        construct::Construct::Die(die) => {
            if let Some(ref args) = die.arguments {
                for arg in args.arguments.iter() {
                    collect_from_expression(arg.value(), content, ranges);
                }
            }
        }
        construct::Construct::Isset(isset) => {
            for val in isset.values.iter() {
                collect_from_expression(val, content, ranges);
            }
        }
        construct::Construct::Empty(empty) => {
            collect_from_expression(empty.value, content, ranges);
        }
        construct::Construct::Eval(eval) => {
            collect_from_expression(eval.value, content, ranges);
        }
        construct::Construct::Include(include) => {
            collect_from_expression(include.value, content, ranges);
        }
        construct::Construct::IncludeOnce(include) => {
            collect_from_expression(include.value, content, ranges);
        }
        construct::Construct::Require(require) => {
            collect_from_expression(require.value, content, ranges);
        }
        construct::Construct::RequireOnce(require) => {
            collect_from_expression(require.value, content, ranges);
        }
    }
}

// ─── Comment folding ────────────────────────────────────────────────────────

/// Scan trivia for doc-block comments and groups of consecutive single-line
/// comments, emitting `FoldingRange` entries with `FoldingRangeKind::Comment`.
fn collect_comment_ranges(
    trivia: &sequence::Sequence<'_, Trivia<'_>>,
    content: &str,
    ranges: &mut Vec<FoldingRange>,
) {
    // ── Doc-block and multi-line comments ──
    for t in trivia.iter() {
        if matches!(
            t.kind,
            TriviaKind::DocBlockComment | TriviaKind::MultiLineComment
        ) {
            let start_pos = offset_to_position(content, t.span.start.offset as usize);
            let end_pos = offset_to_position(content, t.span.end.offset as usize);
            if start_pos.line < end_pos.line {
                ranges.push(FoldingRange {
                    start_line: start_pos.line,
                    start_character: Some(start_pos.character),
                    end_line: end_pos.line,
                    end_character: Some(end_pos.character),
                    kind: Some(FoldingRangeKind::Comment),
                    collapsed_text: None,
                });
            }
        }
    }

    // ── Consecutive single-line comments (`//` or `#`) ──
    // Group adjacent single-line comment trivia whose lines differ by
    // exactly 1 (allowing interleaved whitespace trivia).
    let single_line_comments: Vec<&Trivia<'_>> = trivia
        .iter()
        .filter(|t| t.kind.is_single_line_comment())
        .collect();

    if single_line_comments.is_empty() {
        return;
    }

    let mut group_start_offset = single_line_comments[0].span.start.offset;
    let mut group_end_offset = single_line_comments[0].span.end.offset;
    let mut group_start_line = offset_to_position(content, group_start_offset as usize).line;
    let mut group_end_line = offset_to_position(content, group_end_offset as usize).line;

    for t in single_line_comments.iter().skip(1) {
        let cur_line = offset_to_position(content, t.span.start.offset as usize).line;
        if cur_line == group_end_line + 1 {
            // Extend the current group.
            group_end_offset = t.span.end.offset;
            group_end_line = offset_to_position(content, group_end_offset as usize).line;
        } else {
            // Flush the previous group if it spans multiple lines.
            if group_start_line < group_end_line {
                let start_char = offset_to_position(content, group_start_offset as usize).character;
                let end_char = offset_to_position(content, group_end_offset as usize).character;
                ranges.push(FoldingRange {
                    start_line: group_start_line,
                    start_character: Some(start_char),
                    end_line: group_end_line,
                    end_character: Some(end_char),
                    kind: Some(FoldingRangeKind::Comment),
                    collapsed_text: None,
                });
            }
            // Start a new group.
            group_start_offset = t.span.start.offset;
            group_end_offset = t.span.end.offset;
            group_start_line = cur_line;
            group_end_line = offset_to_position(content, group_end_offset as usize).line;
        }
    }

    // Flush the last group.
    if group_start_line < group_end_line {
        let start_char = offset_to_position(content, group_start_offset as usize).character;
        let end_char = offset_to_position(content, group_end_offset as usize).character;
        ranges.push(FoldingRange {
            start_line: group_start_line,
            start_character: Some(start_char),
            end_line: group_end_line,
            end_character: Some(end_char),
            kind: Some(FoldingRangeKind::Comment),
            collapsed_text: None,
        });
    }
}
