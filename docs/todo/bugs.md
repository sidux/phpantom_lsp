# PHPantom — Bug Fixes

Every bug below must be fixed at its root cause. "Detect the
symptom and suppress the diagnostic" is not an acceptable fix.
If the type resolution pipeline produces wrong data, fix the
pipeline so it produces correct data. Downstream consumers
(diagnostics, hover, completion, definition) should never need
to second-guess upstream output.

## B1. `number` pseudo-type shadows the real `BcMath\Number` class
**Impact: Medium · Effort: Low-Medium**

`number` is listed in `is_builtin_non_class_type` (in `php_type.rs`),
and the type helpers that consult it (`PhpType::base_name`,
`resolve_names`, and the scalar checks in `is_type_compatible`) match
type names case-insensitively. As a result any class named `Number` —
most importantly PHP 8.4's built-in `BcMath\Number` — is misclassified
as a scalar pseudo-type rather than a class.

Concretely, for a property or parameter typed `Number` (imported via
`use BcMath\Number;`):

- `resolve_names` refuses to expand the short name to its FQN because
  it treats `Number` as the scalar `number`, so the declared type stays
  `Number` while the assigned/returned value resolves to the FQN
  `BcMath\Number`.
- `base_name()` returns `None` for the `Number` side, so
  `is_subtype_of_typed` never runs the class-hierarchy check that would
  unify `BcMath\Number` with `Number`.

The net effect is a false-positive `type_mismatch_argument` on
completely valid BC Math code (and the same root cause will produce
false positives in any other type-mismatch collector that compares a
`Number`-typed declaration against a resolved `BcMath\Number` value):

```php
use BcMath\Number;

function scale(Number $n): void {}

function test(string $v): void {
    scale(new Number($v)); // ← wrongly flagged
}
```

`number` is not a real PHP type and neither PHPStan nor Psalm recognise
it (they use `numeric`). The fix is to stop treating `number` as a
builtin non-class type (or, at minimum, make the classification
namespace/case aware so a real class named `Number` is never shadowed).
This is a resolution-layer bug; fixing it at the root removes the false
positive from every consumer (arguments, hover, completion) at once.
