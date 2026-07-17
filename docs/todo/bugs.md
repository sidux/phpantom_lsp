# PHPantom — Bug Fixes

Every bug below must be fixed at its root cause. "Detect the
symptom and suppress the diagnostic" is not an acceptable fix.
If the type resolution pipeline produces wrong data, fix the
pipeline so it produces correct data. Downstream consumers
(diagnostics, hover, completion, definition) should never need
to second-guess upstream output.

All entries below come from the 2026-07 analyze triage sweep over
the sample projects (see `projects/analyze-triage.md`). Except
where noted, each was reproduced in isolation with a minimal
fixture against a debug build. Counts are the number of analyze
errors the bug accounts for across the sample projects and are
approximate — fixing an upstream bug often clears cascading
errors attributed to other buckets.

Laravel-specific items from the same sweep are in
`docs/todo/laravel.md` (L21 alias parsing); ~50 further errors
were reclassified as intended
diagnostics per the declared-types philosophy there. The closure
literal-return shape gap is filed as T31 in
`docs/todo/type-inference.md`.

The B91–B102 batch below comes from the 2026-07-16 full re-triage of
the three remaining non-clean projects (PDepend, Luxplus Website,
Luxplus Backoffice). Together they account for every remaining
analyze error in those projects (42 errors after the same sweep's
project-side patches). Each was reproduced with the minimal fixture
shown unless noted otherwise. Note that `unresolved_member_access`
is off by default — fixtures must be run in a project whose
`.phpantom.toml` sets `[diagnostics] unresolved-member-access = true`,
matching the sample projects.

## B91. Narrowing guards do not apply to array-index subject expressions

**Severity: Medium (~5 errors: pdepend ×3, luxplus-backoffice ×2) · Reproduced with fixture**

```php
/** @psalm-assert =ExpectedType $actual */ // PHPUnit's assertInstanceOf
static::assertInstanceOf(Wanted::class, $constants['C']);
$constants['C']->getImage(); // "type of '$constants['C']' could not be resolved"

if (!is_a($config['class'], Extension::class, true)) {
    throw new RuntimeException('nope');
}
$m->activateExtension($config['class']); // "expects class-string<Extension>, got string"
```

Assert-tag narrowing (`assertInstanceOf`) and `is_a(..., true)`
class-string narrowing both work when the subject is a plain
variable, but are silently dropped when the subject is an array
index expression (`$arr['key']`, `$arr[0]`). PHPStan keys narrowing
by printed expression, so index expressions narrow like any other
subject. Covers PDepend's `PdependExtension.php:87`
`type_mismatch_argument` and the `$constants['C']`/`$elements[0]`
accesses in its PHP 8.2 enum tests, plus the two
`$parameters['amount']->toFixed(2)` accesses in Backoffice's
BusinessCentral tests.
