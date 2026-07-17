# PHPantom — Bug Fixes

Every bug below must be fixed at its root cause. "Detect the
symptom and suppress the diagnostic" is not an acceptable fix.
If the type resolution pipeline produces wrong data, fix the
pipeline so it produces correct data. Downstream consumers
(diagnostics, hover, completion, definition) should never need
to second-guess upstream output.

The three entries below come from the 2026-07-18 analyze triage
refresh over the sample projects (see `projects/analyze-triage.md`).

## B110. Container string alias resolution (`app('x')` / `resolve('x')`) does not apply once the call result is assigned to a variable

**Severity: Low (1 error, bladestan) · Reproduced with fixture**

```php
$compiler = resolve('blade.compiler');
$compiler->component('dynamic-component', DynamicComponent::class); // "type of '$compiler' could not be resolved"
```

`resolve('blade.compiler')->component(...)` (no intermediate variable)
resolves correctly — the direct-call-subject path
(`completion/call_resolution.rs`) intercepts `app`/`resolve` with a
literal string argument and looks it up in Laravel's container alias
table. The variable-assignment RHS resolver
(`completion/variable/rhs_resolution.rs::resolve_rhs_function_call`)
has no equivalent check, so `$var = resolve('blade.compiler');` loses
the binding. Bladestan's `BladeCompilerFactory.php:17-18` is the only
sample-project occurrence.

## B111. `assertInstanceOf()` narrowing requires a literal `X::class` argument; a variable holding the same class-string does not narrow

**Severity: Low-Medium (4 errors, pdepend) · Reproduced with fixture**

```php
$expectedTypeClass = Wanted::class; // held in a variable, not inlined
static::assertInstanceOf($expectedTypeClass, $type);
$type->getImage(); // "type of '$type' could not be resolved"
```

`static::assertInstanceOf(Wanted::class, $type)` (the class inlined
directly in the call) narrows `$type` fine. Replacing only the inline
class with a variable holding the exact same `Wanted::class` value,
with everything else unchanged, reintroduces the error — confirmed by
toggling a single line in a fixture copied from the real file. The
narrowing logic doesn't fold the variable back to its literal value,
so the assert is treated as an unknown-target instanceof check and no
narrowing happens. Accounts for all 4 remaining `getImage()` errors in
PDepend's PHP 8.1/8.2 parser tests
(`AllowNullAndFalseAsStandAloneTypesTest.php`, `TrueTypeTest.php`,
`PHPParserVersion81Test.php` ×2), each using
`$expectedTypeClass = ASTScalarType::class;` or
`$expected[2] ?? ASTScalarType::class;` one line before the assert.
