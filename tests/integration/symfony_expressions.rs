use crate::common::create_psr4_workspace_with_config;
use phpantom_lsp::Backend;
use tower_lsp::LanguageServer;
use tower_lsp::lsp_types::*;

const COMPOSER: &str = r#"{
    "autoload": { "psr-4": { "App\\": "src/" } }
}"#;

const CONFIG: &str = r#"
[[symfony.expression-language.attributes]]
attribute = 'Acme\Attribute\Cache'
argument = "tags"
position = 3
method-parameters = true

[[symfony.expression-language.attributes]]
attribute = 'Acme\Attribute\InvalidateCache'
argument = "tags"
position = 2
method-parameters = true

[[symfony.expression-language.attributes]]
attribute = 'Acme\Attribute\Lock'
argument = "key"
position = 0
method-parameters = true

[[symfony.expression-language.attributes]]
attribute = 'Acme\Attribute\Security'
argument = "expression"
position = 0
method-parameters = true

[[symfony.expression-language.constructors]]
class = 'Symfony\Component\ExpressionLanguage\Expression'
position = 0
inside-attribute-prefixes = ['Acme\Attribute\']
bindings = { request = "parameter:0", response = "return" }
"#;

async fn open_doc(backend: &Backend, uri: Url, text: &str) {
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri,
                language_id: "php".to_string(),
                version: 1,
                text: text.to_string(),
            },
        })
        .await;
}

fn uri_for(dir: &tempfile::TempDir, rel: &str) -> Url {
    Url::from_file_path(dir.path().join(rel)).unwrap()
}

fn position_in(content: &str, needle: &str, inside: usize) -> Position {
    let offset = content.find(needle).expect("needle should exist") + inside;
    let prefix = &content[..offset];
    Position::new(
        prefix.bytes().filter(|byte| *byte == b'\n').count() as u32,
        prefix
            .rsplit_once('\n')
            .map_or(prefix.len(), |(_, line)| line.len()) as u32,
    )
}

async fn definition_at(backend: &Backend, uri: Url, position: Position) -> Location {
    let response = backend
        .goto_definition(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position,
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
        })
        .await
        .unwrap()
        .expect("expression symbol should resolve");
    match response {
        GotoDefinitionResponse::Scalar(location) => location,
        GotoDefinitionResponse::Array(mut locations) if locations.len() == 1 => {
            locations.pop().unwrap()
        }
        other => panic!("expected one definition, got {other:?}"),
    }
}

#[tokio::test]
async fn cache_expression_variable_and_property_navigate_to_php_declarations() {
    let request_php = r#"<?php
namespace App\Request;

final readonly class CourseRequest
{
    public function __construct(public int $courseId) {}
}
"#;
    let controller_php = r#"<?php
namespace App\Controller;

use App\Request\CourseRequest;
use Acme\Attribute\Cache;

final class CourseController
{
    #[Cache(ttl: 86345, tags: ['"oc_course" ~ request.courseId'])]
    public function show(CourseRequest $request): void {}
}
"#;
    let (backend, dir) = create_psr4_workspace_with_config(
        COMPOSER,
        CONFIG,
        &[
            ("src/Request/CourseRequest.php", request_php),
            ("src/Controller/CourseController.php", controller_php),
        ],
    );
    let request_uri = uri_for(&dir, "src/Request/CourseRequest.php");
    let controller_uri = uri_for(&dir, "src/Controller/CourseController.php");
    open_doc(&backend, request_uri.clone(), request_php).await;
    open_doc(&backend, controller_uri.clone(), controller_php).await;

    let root = definition_at(
        &backend,
        controller_uri.clone(),
        position_in(controller_php, "request.courseId", 2),
    )
    .await;
    assert_eq!(root.uri, controller_uri);
    assert_eq!(root.range.start.line, 9);

    let property = definition_at(
        &backend,
        controller_uri,
        position_in(controller_php, "request.courseId", "request.".len() + 2),
    )
    .await;
    assert_eq!(property.uri, request_uri);
    assert_eq!(property.range.start.line, 5);
}

#[tokio::test]
async fn invalidate_cache_and_lock_expressions_navigate_and_report_missing_members() {
    let request_php = r#"<?php
namespace App\Request;

final readonly class CourseRequest
{
    public function __construct(public int $courseId) {}
}
"#;
    let controller_php = r#"<?php
namespace App\Controller;

use App\Request\CourseRequest;
use Acme\Attribute\InvalidateCache;
use Acme\Attribute\Lock;

final class CourseController
{
    #[InvalidateCache(tags: ['"course" ~ request.courseId'])]
    #[Lock(key: '"course" ~ request.courseId ~ request.missingField')]
    public function update(CourseRequest $request): void {}
}
"#;
    let (backend, dir) = create_psr4_workspace_with_config(
        COMPOSER,
        CONFIG,
        &[
            ("src/Request/CourseRequest.php", request_php),
            ("src/Controller/CourseController.php", controller_php),
        ],
    );
    let request_uri = uri_for(&dir, "src/Request/CourseRequest.php");
    let controller_uri = uri_for(&dir, "src/Controller/CourseController.php");
    open_doc(&backend, request_uri.clone(), request_php).await;
    open_doc(&backend, controller_uri.clone(), controller_php).await;

    for needle in [
        "request.courseId'])]",
        "request.courseId ~ request.missingField",
    ] {
        let property = definition_at(
            &backend,
            controller_uri.clone(),
            position_in(controller_php, needle, "request.".len() + 2),
        )
        .await;
        assert_eq!(property.uri, request_uri);
        assert_eq!(property.range.start.line, 5);
    }

    let mut diagnostics = Vec::new();
    backend.collect_slow_diagnostics(controller_uri.as_str(), controller_php, &mut diagnostics);
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_ref().is_some_and(
            |code| matches!(code, NumberOrString::String(value) if value == "unknown_member"),
        ) && diagnostic.message.contains("missingField")
    }));
}

#[tokio::test]
async fn explicit_expression_object_resolves_request_alias_and_response_type() {
    let request_php = r#"<?php
namespace App\Request;

final readonly class CourseRequest
{
    public function __construct(public int $courseId) {}
}
"#;
    let response_php = r#"<?php
namespace App\Response;

final readonly class CourseResponse
{
    public function __construct(public int $candidateId) {}
}
"#;
    let use_case_php = r#"<?php
namespace App\UseCase;

use App\Request\CourseRequest;
use App\Response\CourseResponse;
use Acme\Attribute\Track;
use Symfony\Component\ExpressionLanguage\Expression;

final class PublishCourse
{
    #[Track(name: new Expression('request.courseId ~ response.candidateId ~ response.missingField'))]
    public function execute(CourseRequest $useCaseRequest): CourseResponse
    {
        return new CourseResponse(1);
    }
}
"#;
    let (backend, dir) = create_psr4_workspace_with_config(
        COMPOSER,
        CONFIG,
        &[
            ("src/Request/CourseRequest.php", request_php),
            ("src/Response/CourseResponse.php", response_php),
            ("src/UseCase/PublishCourse.php", use_case_php),
        ],
    );
    let request_uri = uri_for(&dir, "src/Request/CourseRequest.php");
    let response_uri = uri_for(&dir, "src/Response/CourseResponse.php");
    let use_case_uri = uri_for(&dir, "src/UseCase/PublishCourse.php");
    open_doc(&backend, request_uri.clone(), request_php).await;
    open_doc(&backend, response_uri.clone(), response_php).await;
    open_doc(&backend, use_case_uri.clone(), use_case_php).await;

    let request_root = definition_at(
        &backend,
        use_case_uri.clone(),
        position_in(use_case_php, "request.courseId", 2),
    )
    .await;
    assert_eq!(request_root.uri, use_case_uri);
    assert_eq!(request_root.range.start.line, 11);

    let request_property = definition_at(
        &backend,
        use_case_uri.clone(),
        position_in(use_case_php, "request.courseId", "request.".len() + 2),
    )
    .await;
    assert_eq!(request_property.uri, request_uri);

    let response_property = definition_at(
        &backend,
        use_case_uri.clone(),
        position_in(use_case_php, "response.candidateId", "response.".len() + 2),
    )
    .await;
    assert_eq!(response_property.uri, response_uri);

    let mut diagnostics = Vec::new();
    backend.collect_slow_diagnostics(use_case_uri.as_str(), use_case_php, &mut diagnostics);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("missingField"))
    );
}

#[tokio::test]
async fn explicit_expression_object_resolves_backed_enum_value_to_enum() {
    let interaction_php = r#"<?php
namespace App\Model;

enum Interaction: string
{
    case Viewed = 'viewed';
}
"#;
    let request_php = r#"<?php
namespace App\Request;

use App\Model\Interaction;

final readonly class TrackRequest
{
    public function __construct(public Interaction $interaction) {}
}
"#;
    let use_case_php = r#"<?php
namespace App\UseCase;

use App\Request\TrackRequest;
use Acme\Attribute\Track;
use Symfony\Component\ExpressionLanguage\Expression;

final class TrackInteraction
{
    #[Track(name: new Expression('request.interaction.value'))]
    public function execute(TrackRequest $request): void {}
}
"#;
    let (backend, dir) = create_psr4_workspace_with_config(
        COMPOSER,
        CONFIG,
        &[
            ("src/Model/Interaction.php", interaction_php),
            ("src/Request/TrackRequest.php", request_php),
            ("src/UseCase/TrackInteraction.php", use_case_php),
        ],
    );
    let interaction_uri = uri_for(&dir, "src/Model/Interaction.php");
    let request_uri = uri_for(&dir, "src/Request/TrackRequest.php");
    let use_case_uri = uri_for(&dir, "src/UseCase/TrackInteraction.php");
    open_doc(&backend, interaction_uri.clone(), interaction_php).await;
    open_doc(&backend, request_uri, request_php).await;
    open_doc(&backend, use_case_uri.clone(), use_case_php).await;

    let value = definition_at(
        &backend,
        use_case_uri.clone(),
        position_in(
            use_case_php,
            "request.interaction.value",
            "request.interaction.".len() + 2,
        ),
    )
    .await;
    assert_eq!(value.uri, interaction_uri);
    assert_eq!(value.range.start.line, 3);

    let mut diagnostics = Vec::new();
    backend.collect_slow_diagnostics(use_case_uri.as_str(), use_case_php, &mut diagnostics);
    assert!(!diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_ref().is_some_and(
            |code| matches!(code, NumberOrString::String(value) if value == "unknown_member"),
        )
    }));
}

#[tokio::test]
async fn non_service_proxy_expression_objects_keep_their_own_variable_contract() {
    let controller_php = r#"<?php
namespace App\Controller;

use Symfony\Component\ExpressionLanguage\Expression;
use Symfony\Component\Security\Http\Attribute\IsGranted;

final class Subject {}

final class CourseController
{
    #[IsGranted(
        attribute: new Expression('subject.missing'),
        subject: new Expression('args["courseId"]'),
    )]
    public function edit(Subject $subject, int $courseId): void {}
}
"#;
    let (backend, dir) = create_psr4_workspace_with_config(
        COMPOSER,
        CONFIG,
        &[("src/Controller/CourseController.php", controller_php)],
    );
    let controller_uri = uri_for(&dir, "src/Controller/CourseController.php");
    open_doc(&backend, controller_uri.clone(), controller_php).await;

    let mut diagnostics = Vec::new();
    backend.collect_slow_diagnostics(controller_uri.as_str(), controller_php, &mut diagnostics);
    assert!(!diagnostics.iter().any(|diagnostic| {
        diagnostic.code.as_ref().is_some_and(
            |code| matches!(code, NumberOrString::String(value) if value == "unknown_member"),
        ) && diagnostic.message.contains("missing")
    }));
}

#[tokio::test]
async fn security_expression_follows_method_return_type() {
    let course_php = r#"<?php
namespace App\Model;

final class Course
{
    public int $id = 1;
}
"#;
    let request_php = r#"<?php
namespace App\Request;

use App\Model\Course;

final class CourseRequest
{
    public function course(): Course { return new Course(); }
}
"#;
    let controller_php = r#"<?php
namespace App\Controller;

use App\Request\CourseRequest;
use Acme\Attribute\Security;

final class CourseController
{
    #[Security(expression: 'request.course().id > 0')]
    public function show(CourseRequest $request): void {}
}
"#;
    let (backend, dir) = create_psr4_workspace_with_config(
        COMPOSER,
        CONFIG,
        &[
            ("src/Model/Course.php", course_php),
            ("src/Request/CourseRequest.php", request_php),
            ("src/Controller/CourseController.php", controller_php),
        ],
    );
    let course_uri = uri_for(&dir, "src/Model/Course.php");
    let request_uri = uri_for(&dir, "src/Request/CourseRequest.php");
    let controller_uri = uri_for(&dir, "src/Controller/CourseController.php");
    open_doc(&backend, course_uri.clone(), course_php).await;
    open_doc(&backend, request_uri.clone(), request_php).await;
    open_doc(&backend, controller_uri.clone(), controller_php).await;

    let method = definition_at(
        &backend,
        controller_uri.clone(),
        position_in(controller_php, "request.course()", "request.".len() + 2),
    )
    .await;
    assert_eq!(method.uri, request_uri);
    assert_eq!(method.range.start.line, 7);

    let property = definition_at(
        &backend,
        controller_uri,
        position_in(
            controller_php,
            "request.course().id",
            "request.course().".len() + 1,
        ),
    )
    .await;
    assert_eq!(property.uri, course_uri);
    assert_eq!(property.range.start.line, 5);
}

#[tokio::test]
async fn expression_diagnostics_report_only_the_first_missing_member_in_each_chain() {
    let request_php = r#"<?php
namespace App\Request;

final class CourseRequest
{
    public int $courseId = 1;
    public function existing(): self { return $this; }
}
"#;
    let controller_php = r#"<?php
namespace App\Controller;

use App\Request\CourseRequest;
use Acme\Attribute\{Cache, Security};

final class CourseController
{
    #[Cache(tags: [
        '"course" ~ request.courseId',
        '"missing" ~ request.missingField.afterMissing',
    ])]
    #[Security('request.existing().courseId > 0 and request.missingMethod()')]
    public function show(CourseRequest $request): void {}

    #[Cache(null, [], null, ['request.positionalMissing'])]
    public function other(CourseRequest $request): void {}
}
"#;
    let (backend, dir) = create_psr4_workspace_with_config(
        COMPOSER,
        CONFIG,
        &[
            ("src/Request/CourseRequest.php", request_php),
            ("src/Controller/CourseController.php", controller_php),
        ],
    );
    let request_uri = uri_for(&dir, "src/Request/CourseRequest.php");
    let controller_uri = uri_for(&dir, "src/Controller/CourseController.php");
    open_doc(&backend, request_uri, request_php).await;
    open_doc(&backend, controller_uri.clone(), controller_php).await;

    let mut diagnostics = Vec::new();
    backend.collect_slow_diagnostics(controller_uri.as_str(), controller_php, &mut diagnostics);
    diagnostics.retain(|diagnostic| {
        diagnostic.code.as_ref().is_some_and(
            |code| matches!(code, NumberOrString::String(value) if value == "unknown_member"),
        )
    });
    assert_eq!(
        diagnostics.len(),
        3,
        "unexpected diagnostics: {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("missingField"))
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("missingMethod"))
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("positionalMissing"))
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.message.contains("afterMissing"))
    );
}

#[tokio::test]
async fn unrelated_cache_attribute_is_not_treated_as_expression_language() {
    let request_php = "<?php\nnamespace App\\Request;\nfinal class CourseRequest {}\n";
    let controller_php = r#"<?php
namespace App\Controller;

use App\Attribute\Cache;
use App\Request\CourseRequest;

final class CourseController
{
    #[Cache(tags: ['request.missingField'])]
    public function show(CourseRequest $request): void {}
}
"#;
    let (backend, dir) = create_psr4_workspace_with_config(
        COMPOSER,
        CONFIG,
        &[
            ("src/Request/CourseRequest.php", request_php),
            ("src/Controller/CourseController.php", controller_php),
        ],
    );
    let request_uri = uri_for(&dir, "src/Request/CourseRequest.php");
    let controller_uri = uri_for(&dir, "src/Controller/CourseController.php");
    open_doc(&backend, request_uri, request_php).await;
    open_doc(&backend, controller_uri.clone(), controller_php).await;

    let mut diagnostics = Vec::new();
    backend.collect_slow_diagnostics(controller_uri.as_str(), controller_php, &mut diagnostics);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| { !diagnostic.message.contains("missingField") }),
        "unrelated Cache attribute should not be indexed: {diagnostics:?}"
    );
}
