# Symfony Language Tools comparison

## Scope and confidence

This report compares:

- Symfony Language Tools at commit [`faeb42e`](https://github.com/symfony/language-tools/commit/faeb42e95d5cc4a88471b55bcec28ac28047c753), inspected on 2026-08-27.
- The completed PHPantom feature bundle at commit [`5204284`](https://github.com/sidux/phpantom_lsp/commit/52042849aa85a3f7132f44f66b338f3058b28798).

Only repository source, tests, documentation, manifests, workflows, and commit metadata were inspected. No benchmarks or test suites were run. “Supported” below therefore means implemented or documented in the inspected source, not independently performance-tested here.

Symfony Language Tools is also very young: its root commit is [`fcecb57`](https://github.com/symfony/language-tools/commit/fcecb57957d66982092a6752ff4fa2fc0a05108f), dated 2026-07-30. Its breadth and test design are useful evidence, but not proof of long-term field maturity.

## Executive assessment

The strongest ideas to adopt are architectural, not another list of hard-coded Symfony call sites:

1. Keep static facts and trusted runtime facts as separate inputs to the same feature indexes.
2. Persist per-file framework facts and restore unchanged files without reparsing them.
3. Carry explicit completeness and staleness state, so diagnostics are emitted only when the available metadata can prove an error.
4. Split framework extraction into typed providers with independent payload versions and refresh domains.
5. Validate Symfony support across maintained Symfony branches and representative public applications.

PHPantom should not copy the upstream product boundary wholesale. Symfony Language Tools intentionally runs beside a general PHP server, while PHPantom already has a shared PHP parser, symbol index, type engine, and reference engine. PHPantom’s better direction is a generic metadata-provider seam feeding those engines, with Symfony as one adapter.

## Capability comparison

| Area | Symfony Language Tools | PHPantom feature bundle | Assessment |
| --- | --- | --- | --- |
| Product boundary | Symfony companion server; general PHP intelligence is delegated to another LSP. | General PHP LSP with framework features integrated into the same backend. | PHPantom has the stronger foundation for type-aware framework features. |
| Routing | Completion, rich hover, definition, references, rename, diagnostics, missing-parameter quick fix, PHP/YAML/Twig support. | Static PHP/YAML/XML/Twig completion, definition, references, rename, highlights, diagnostics, and lenses for route names and parameters. | Similar core navigation; upstream is deeper in hover, runtime route truth, context confidence, and quick fixes. |
| Dependency injection | Runtime services, aliases, decorators, private/hidden services, autowiring types and parameters; source YAML/XML/`Autowire`; hover, rename, diagnostics. | Static service and parameter declarations/usages across PHP/YAML/XML with completion, navigation, references, rename, diagnostics, and lenses. | Runtime container metadata and rich hover are the main PHPantom gaps. |
| Twig templates | Loader-aware names and namespaces, variables from render arrays/globals/type tags/components, links, references, diagnostics, create-template action. | Static template names across PHP and Twig, namespaced bundle templates, completion/navigation/references/lenses, missing-template action. | PHPantom covers template names well; upstream is stronger on loader truth, variables, and components. |
| Twig callables and components | Custom functions/filters, callable signatures and named arguments, component properties/actions/events, hover/navigation/references/lenses. | No comparable Twig callable, Twig component, or Live Component model in the inspected bundle. | Material missing domain. |
| Translations | Runtime catalogue plus YAML/JSON/XLIFF/PHP/INI sources; locale/domain/placeholder completion, hover, definition, references, rename, diagnostics, quick fix. | Static YAML/XLIFF/PHP catalogues with domain-aware completion, navigation, references, diagnostics, and lenses. | PHPantom lacks runtime catalogues, hover, rename, locale/placeholder depth, JSON/INI coverage. |
| Events | Runtime listener wiring, subscribers, priorities, event classes/names, completion, hover, navigation, references, lenses, listener-method diagnostics. | Compiled-container listener wiring read without execution, configurable publisher/subscriber conventions, proxy-aware bidirectional navigation/references/lenses; plus static named events. | PHPantom’s generic configuration and safe compiled-container reader are stronger; upstream has broader native Symfony semantics and richer metadata. |
| Messenger | Runtime buses, transports, senders, handlers, completion, hover, navigation, references, diagnostics, lenses. | Static buses and message-to-handler relationships with completion/navigation/references/diagnostics/lenses. | Runtime topology is missing in PHPantom. |
| Forms, validation, serializer | Runtime form and constraint options; static and runtime mappings; serializer groups; completion, hover, navigation, references, diagnostics. | Form field/property navigation and completion, validation property/constraint navigation, and local configuration schemas. | PHPantom lacks runtime option metadata and serializer-group intelligence. |
| Bundle configuration | Runtime Config Tree for installed bundles, including types, enums, defaults, normalization, deprecations, YAML/XML/PHP completion/hover/diagnostics. | Local application `TreeBuilder` schemas drive YAML completion/navigation/diagnostics/references/lenses. | PHPantom’s local schema support is useful; installed-bundle runtime schemas are a major gap. |
| Doctrine | Entity/repository relationships, criteria-field completion, hover/navigation/references/lenses; runtime adds XML/YAML/vendor mappings. | Entity/repository/configuration mapping navigation and lenses. | Upstream is deeper on field metadata and criteria arrays. Upstream explicitly does **not** interpret DQL strings or Query Builder field expressions. |
| Environment, Security, Console, assets, Stimulus | Dedicated integrations for environment processors, SecurityBundle, Console definitions, AssetMapper/public assets/import maps, Stimulus and Live Components. | No comparable Symfony-specific domains were found in the inspected bundle. | Missing domains; prioritize by user demand rather than copying all at once. |
| Generic YAML/XML PHP symbols | Feature extractors focus on known Symfony schemas and contexts. | Any qualified PHP class in YAML/XML can navigate, participate in references, rename, and CodeLens; `Class::member` is also understood. | PHPantom is materially more generic. |
| Project-specific DSLs | Mostly built-in Symfony conventions. | Config-driven ExpressionLanguage contracts, event naming, and transparent proxy relations reuse generic PHP resolution. | PHPantom is materially more extensible for unknown packages. |
| Framework hover | Broad feature-specific metadata hover. | No shared Symfony-resource hover path was found. | High-value feature gap. |
| Headless diagnostics | `symfony-lsp check` supports human, JSON, GitHub, SARIF, policies, source-only mode, and baselines. | `phpantom_lsp analyze` supports table, JSON, and GitHub output, but its documented scan is PHP-file focused. | PHPantom should bring resource diagnostics into `analyze`; upstream has stronger CI parity. |

Evidence: the upstream feature matrix lists the LSP surface by integration and its code-lens domains ([feature matrix](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/docs/features/index.rst#L17-L138)). PHPantom’s shared framework symbol kinds and reference forms are visible in [`framework.rs`](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/src/framework.rs#L23-L139), while the shipped feature set is summarized in its [changelog](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/docs/CHANGELOG.md#L12-L28). The Doctrine limitation is explicit in the upstream [Doctrine guide](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/docs/features/doctrine.rst#L59-L65).

## Upstream architecture and data flow

### Static source plane

Each feature contributes a `SourceIndexProviderInterface`. A project scan calls all providers, stores their per-file payloads, and restores payloads for unchanged files. Open documents are overlays over saved facts, so unsaved content takes precedence without rewriting the saved generation. Scans are project-keyed, cancel superseded work, and yield periodically to the event loop ([scanner lifecycle](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/src/Index/ApplicationSourceScanner.php#L24-L93), [incremental restore](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/src/Index/ApplicationSourceScanner.php#L383-L503), [overlay store](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/src/Index/SourceFactsStore.php#L5-L63)).

The disk format is append-friendly JSON Lines. Full scans stream and atomically replace a generation; single-file changes append a record; the last record for a path wins. Headers include schema and server versions, and offsets allow one file’s payload to be loaded directly ([persistent store](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/src/Index/PersistentSourceIndexStore.php#L11-L137)).

This is materially stronger than PHPantom’s current framework-resource path, which rebuilds an in-memory URI map and derived inverted lookup during startup ([workspace scan](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/src/framework.rs#L473-L533)). PHPantom’s general PHP index and semantic reference layer are stronger than that framework path, but they do not currently provide a persistent per-provider resource-fact cache.

### Trusted runtime plane

When allowed, a subprocess bridge loads the application Composer autoloader, boots the selected environment, and emits versioned JSON sections for routes, container, Twig, translations, Messenger, events, Security, metadata, assets, Stimulus, configuration, Doctrine, environment processors, and Console definitions ([bridge entry point](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/resources/bridge.php#L27-L45), [section dispatch](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/resources/bridge.php#L112-L163)). Individual section failures are isolated.

The server validates the schema, loads successful sections, persists a hashed snapshot, merges targeted section refreshes, and restores the last compatible snapshot after a failed refresh ([initializer](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/src/Runtime/ProjectRuntimeInitializer.php#L26-L103), [snapshot store](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/src/Runtime/RuntimeSnapshotStore.php#L22-L157)). A refresh planner maps changed source-fact domains to only the affected runtime sections and clears the container only when necessary ([refresh planner](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/src/Runtime/RuntimeRefreshPlanner.php#L7-L78)).

Runtime execution is explicitly trust-gated. A checked-in project file cannot grant trust, and source-only features remain available when runtime indexing is unavailable ([trust and configuration](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/docs/project-configuration.rst#L102-L113), [runtime fallback semantics](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/docs/features/index.rst#L140-L163)).

### Feature dispatch

Completion, hover, diagnostics, definition, links, references, rename, source indexing, and runtime snapshot loading are provider interfaces registered through tags. Feature packages own their extractors, facts, indexes, providers, and runtime loaders ([service registration](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/resources/services.php#L97-L165), [provider registries](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/resources/services.php#L207-L232)).

The code uses a tolerant PHP parser and a native Tree-sitter wrapper, but several feature contexts and extractors still use regular expressions. It should not be described as uniformly AST-driven ([parser dependencies](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/composer.json#L6-L17), [parser wiring](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/resources/services.php#L175-L188), [route receiver example](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/src/Feature/Route/RoutePhpReceiver.php#L13-L42)).

## Patterns materially stronger upstream

### 1. Completeness is part of the data model

Upstream does not equate “not indexed” with “does not exist.” Runtime-dependent diagnostics are omitted when runtime facts are unavailable. Optional DI references are not diagnosed, unknown event names are accepted because listeners are not required, roles remain an open set because custom voters exist, and unknown Doctrine fields are not diagnosed because custom mappings exist ([DI diagnostics](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/docs/features/dependency-injection.rst#L106-L112), [event diagnostics](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/docs/features/events.rst#L48-L54), [Security diagnostics](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/docs/features/security.rst#L39-L45)).

PHPantom currently treats a project-local event reference as unknown when it is absent from its static set ([diagnostic decision](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/src/diagnostics/symfony.rs#L125-L165)). That can be semantically wrong for Symfony’s open event dispatcher. The remedy is not blanket suppression: model whether a domain is closed and whether its facts are complete, then diagnose only provable errors.

### 2. Persistent incremental framework facts

Per-file provider payloads, content hashes, schema versions, open-buffer overlays, append-only updates, and atomic generations make repeated startup proportional to changed files rather than the whole framework resource tree. PHPantom’s framework index already supports efficient per-URI replacement and an inverted lookup, so persistence can be added without redesigning its consumers.

### 3. Rich runtime truth with safe degradation

Symfony’s generated container, router, translator, form registry, bundle Config Trees, and Doctrine metadata contain facts that static source cannot reliably reconstruct. Upstream captures those facts in isolated JSON sections, keeps static results useful, and restores stale-but-compatible runtime snapshots on failure. This is a better pattern than adding more guesses for private services, aliases, decorator stacks, loader paths, inherited Doctrine mappings, or third-party bundle schemas.

### 4. Feature-local provider boundaries

PHPantom’s framework scanner is currently concentrated in one large module. Upstream’s fact/index/provider/loader split gives each domain an explicit ownership boundary and lets one changed domain trigger only its related runtime sections. PHPantom should adopt the seam, not the PHP implementation style.

### 5. Ecosystem validation

Upstream CI is configured to run unit tests, runtime-refresh and source-index scaling benchmarks, static analysis, and style checks ([quality workflow](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/.github/workflows/quality.yaml#L36-L59)). A compatibility job resolves maintained Symfony branches from official release metadata and tests the bridge against each branch ([compatibility workflow](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/.github/workflows/compatibility.yaml#L35-L89)). A separate matrix is configured for representative public Symfony applications ([dogfood workflow](https://github.com/symfony/language-tools/blob/faeb42e95d5cc4a88471b55bcec28ac28047c753/.github/workflows/dogfood.yaml#L56-L125)). These workflows are strong test-design evidence; this review did not verify their historical pass rate.

## PHPantom advantages to preserve

1. **One PHP semantic engine.** Framework expressions can delegate member resolution to the same type engine used by completion, diagnostics, hover, and definition. Upstream necessarily maintains narrower receiver/context logic because it is a companion server.
2. **Generic cross-language class navigation.** PHPantom recognizes qualified PHP classes and `Class::member` in arbitrary YAML/XML, not only known Symfony schemas ([generic scanner](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/src/framework.rs#L4533-L4588)).
3. **Generic proxy metadata.** Transparent proxy relations are independently configurable and canonicalize metadata owners without rewriting normal PHP type identity ([proxy index](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/src/proxy_metadata.rs#L1-L15), [canonical family](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/src/proxy_metadata.rs#L78-L135)).
4. **Safe generated-container inspection.** PHPantom extracts the small event/proxy subset it needs from compiled PHP as text and never executes it ([container adapter](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/src/symfony/container.rs#L1-L61)). Keep this as the default static path even if an optional runtime adapter is added.
5. **Config-driven DSL support.** Expression roots and package-specific event conventions are configuration, while PHP member traversal remains generic ([framework metadata architecture](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/docs/ARCHITECTURE.md#L158-L170)).
6. **Semantic reference mode.** The complete workspace index can proactively resolve compact member relationships for lower first-use CodeLens latency, while exact locations remain bounded and refresh-capable clients receive only ready lenses ([reference index design](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/docs/ARCHITECTURE.md#L903-L914), [indexing strategies](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/docs/configuration.md#L300-L324)).

No comparative speed claim is made: neither project was benchmarked in this research.

## Ranked recommendations

| Rank | Recommendation | Impact | Cost | Concrete shape |
| --- | --- | --- | --- | --- |
| 1 | Add completeness, freshness, and openness to framework indexes. | Very high | Low–medium | Each domain reports `complete`, `partial`, or `stale`, plus whether unknown names are semantically invalid. Gate diagnostics on that state. Correct event diagnostics first. |
| 2 | Persist per-file framework facts. | Very high | Medium | Cache provider payloads by file content hash, provider schema, and server version. Restore unchanged payloads, overlay open documents, append single-file updates, and atomically replace full generations. |
| 3 | Introduce a generic metadata-provider protocol. | Very high | Medium | Providers emit versioned facts and relationships with source locations, domain, generation, environment, completeness, and provenance. Static Rust providers and optional external commands feed the same indexes. |
| 4 | Split the framework module by domain behind that protocol. | High | Medium | Separate generic class/member/path scanning from Symfony services, routes, templates, translations, events, Messenger, forms/validation, and configuration schema providers. Keep shared lookup and LSP consumers centralized. |
| 5 | Add an optional trusted Symfony runtime adapter. | Very high | High | Run an isolated bridge only after client-side trust. Return sectioned JSON. Key snapshots by root, command/container path, environment, and debug flag. Support targeted refresh and stale snapshot fallback. A checked-in config must never grant execution trust. |
| 6 | Deepen existing high-use domains before adding every missing domain. | High | Medium–high | First add service/route/template/translation hover, translation placeholders and rename, Twig loader paths/callables/components, installed-bundle Config Trees, and runtime form/constraint options. These extend features users already see. |
| 7 | Add new domains from telemetry and issues. | Medium | Variable | Environment processors and Security are likely broadly useful. Console, AssetMapper, Stimulus, and Live Components should follow demonstrated demand. Keep each as a provider, not core special cases. |
| 8 | Bring framework-resource diagnostics into `analyze`. | Medium–high | Medium | Reuse the same provider indexes and completeness gates in editor and CLI; then consider SARIF, baselines, and code-based failure policy. PHPantom’s current CLI is documented as scanning PHP files ([CLI scope](https://github.com/sidux/phpantom_lsp/blob/52042849aa85a3f7132f44f66b338f3058b28798/docs/cli.md#L70-L89)). |
| 9 | Add Symfony compatibility and public-application matrices. | High | Medium | Test maintained Symfony branches against the runtime adapter, then run protocol probes for completion, hover, definition, references, ranges, and rename safety on pinned public repositories. |
| 10 | Replace regex contexts selectively, not wholesale. | Medium | High | Use PHPantom’s existing PHP AST/type engine for receiver-aware PHP contexts. Introduce structured YAML/XML/Twig parsing where edits and nested syntax justify it; keep byte scanners for safe, well-bounded discovery hot paths. |

## Suggested implementation sequence

1. Add the metadata-provider types and completeness semantics without changing behavior.
2. Move the existing framework scanners behind providers one domain at a time.
3. Add persistent provider payloads and open-document overlays.
4. Correct diagnostics to use domain completeness and open/closed-world semantics.
5. Add richer hover and translation/template depth from existing static facts.
6. Build the optional Symfony runtime bridge as another provider source.
7. Add compatibility and public-application CI before expanding runtime sections.

This sequence preserves PHPantom’s generic design, improves startup work independently of runtime execution, and creates a stable place for Symfony-specific truth without teaching the core about every bundle or private package.
