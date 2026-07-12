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

## B63. Template bound to a union of `class-string`s falls back to the constraint bound instead of checking each member's subtype

**Severity: Low (2 errors in agcms) · Reproduced**

```php
/** @template T of AbstractEntity | @param class-string<T> $class | @return T[] */
function getByQuery(string $class, string $query): array { /* ... */ }

foreach ([Page::class, CustomPage::class] as $className) {
    // Page extends AbstractRenderable extends AbstractEntity — a valid bind
    $rows = $orm->getByQuery($className, $sql);
    // "Argument 1 ($class) expects class-string<AbstractEntity>, got
    //  class-string<Page>|class-string<CustomPage>"
    // $rows is then typed AbstractEntity[] instead of (Page|CustomPage)[],
    // producing a cascading array<AbstractEntity> vs array<InterfaceRichText>
    // mismatch at the next call site.
}
```

When the argument passed for a `@template T of Bound` parameter is a
union of `class-string<X>` types (e.g. from a `foreach` over a
class-constant array), PHPantom does not check each union member
against the `of Bound` constraint individually. Instead it appears to
give up and substitute the constraint's upper bound (`AbstractEntity`)
as the "expected" type in the mismatch message, then reuses that same
fallback for the method's `T[]` return type — which cascades into a
second, unrelated-looking mismatch at the next call site that consumes
the return value
(agcms `inc/Http/Controllers/Admin/ExplorerController.php:369-374`).

**Fix:** when binding a template parameter from a union argument,
check each union member against the template's bound individually
(walking the inheritance chain, not just direct parents) and keep the
union as the bound type instead of collapsing to the constraint.
