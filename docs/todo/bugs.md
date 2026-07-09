# PHPantom — Bug Fixes

Every bug below must be fixed at its root cause. "Detect the
symptom and suppress the diagnostic" is not an acceptable fix.
If the type resolution pipeline produces wrong data, fix the
pipeline so it produces correct data. Downstream consumers
(diagnostics, hover, completion, definition) should never need
to second-guess upstream output.

## B27. String literal type comparison is quote-style sensitive

**Severity: Low (false-positive argument diagnostic) · Confirmed**

`literal_is_subtype_of` (`src/php_type.rs:3297-3369`) compares two
`PhpType::Literal` string values with plain `lit == other_lit`. Both
the argument-narrowing path
(`src/diagnostics/type_errors.rs`, argument literal narrowing) and
the docblock literal-type parser
(`src/php_type.rs:4095`, `ast::Type::LiteralString`) build the
`Literal` payload from the **raw source text including quote
characters**, so a double-quoted argument literal never equals a
single-quoted docblock literal even when their unquoted contents are
identical.

**Trigger:**

```php
/** @param 'asc'|'desc' $direction */
function orderBy(string $column, string $direction): void {}

function test(): void {
    orderBy('id', "desc"); // flagged: "desc" (double-quoted) != 'desc' (single-quoted)
}
```

Single-quoted usage (`orderBy('id', 'desc')`) is unaffected, since
both sides happen to agree on quote style in that case.

**Fix:** compare the two literals by their unquoted content instead
of raw source text. Both `LiteralString` (source) and
`LiteralStringType` (docblock) already carry a parsed, unquoted
`value` field alongside `raw` — normalise through that (or strip
quotes consistently) before the equality check in
`literal_is_subtype_of`, and make sure the `PhpType::Literal`
constructors that currently pass through `raw` for strings do the
same.
