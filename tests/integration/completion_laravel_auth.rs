//! Integration tests for resolving the authenticated-user model from
//! `config/auth.php` (`Request::user()` / `Guard::user()`).

use crate::common::create_psr4_workspace;
use tower_lsp::LanguageServer;
use tower_lsp::lsp_types::*;

const COMPOSER_JSON: &str = r#"{
    "autoload": {
        "psr-4": {
            "App\\Models\\": "src/Models/",
            "Illuminate\\Contracts\\Auth\\": "vendor/illuminate/Contracts/Auth/",
            "Illuminate\\Http\\": "vendor/illuminate/Http/"
        }
    }
}"#;

const AUTHENTICATABLE_PHP: &str = "\
<?php
namespace Illuminate\\Contracts\\Auth;
interface Authenticatable {
    public function getAuthIdentifier();
}
";

const GUARD_PHP: &str = "\
<?php
namespace Illuminate\\Contracts\\Auth;
interface Guard {
    /** @return \\Illuminate\\Contracts\\Auth\\Authenticatable|null */
    public function user();
}
";

const REQUEST_PHP: &str = "\
<?php
namespace Illuminate\\Http;
class Request {
    /** @return \\Illuminate\\Contracts\\Auth\\Authenticatable|null */
    public function user($guard = null) { return null; }
}
";

const USER_PHP: &str = "\
<?php
namespace App\\Models;
use Illuminate\\Contracts\\Auth\\Authenticatable;
class User implements Authenticatable {
    public function getAuthIdentifier() { return 1; }
    public function isActive(): bool { return true; }
}
";

const ADMIN_PHP: &str = "\
<?php
namespace App\\Models;
use Illuminate\\Contracts\\Auth\\Authenticatable;
class Admin implements Authenticatable {
    public function getAuthIdentifier() { return 1; }
    public function isSuperUser(): bool { return true; }
}
";

fn base_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "vendor/illuminate/Contracts/Auth/Authenticatable.php",
            AUTHENTICATABLE_PHP,
        ),
        ("vendor/illuminate/Contracts/Auth/Guard.php", GUARD_PHP),
        ("vendor/illuminate/Http/Request.php", REQUEST_PHP),
        ("src/Models/User.php", USER_PHP),
        ("src/Models/Admin.php", ADMIN_PHP),
    ]
}

async fn complete_labels(
    files: &[(&str, &str)],
    open_path: &str,
    content: &str,
    line: u32,
    character: u32,
) -> Vec<String> {
    let (backend, dir) = create_psr4_workspace(COMPOSER_JSON, files);
    let uri = Url::from_file_path(dir.path().join(open_path)).unwrap();
    backend
        .did_open(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "php".to_string(),
                version: 1,
                text: content.to_string(),
            },
        })
        .await;

    let result = backend
        .completion(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            context: None,
        })
        .await
        .unwrap();

    let items = match result {
        Some(CompletionResponse::Array(items)) => items,
        Some(CompletionResponse::List(list)) => list.items,
        _ => Vec::new(),
    };
    items.into_iter().map(|i| i.label).collect()
}

/// A hard-literal single guard resolves `$request->user()` precisely to the
/// configured `User` model, so its own members complete.
#[tokio::test]
async fn request_user_resolves_configured_model() {
    let mut files = base_files();
    files.push((
        "config/auth.php",
        "<?php return [
            'defaults' => ['guard' => 'web'],
            'guards' => ['web' => ['provider' => 'users']],
            'providers' => ['users' => ['model' => App\\Models\\User::class]],
        ];",
    ));

    let controller = "\
<?php
namespace App;
use Illuminate\\Http\\Request;
class C {
    public function show(Request $request) {
        $request->user()->
    }
}
";
    // Cursor right after `->` on the `$request->user()->` line (0-indexed
    // line 5, after the arrow).
    let labels = complete_labels(&files, "src/C.php", controller, 5, 26).await;
    assert!(
        labels.iter().any(|l| l.starts_with("isActive")),
        "expected User::isActive in completions, got: {labels:?}"
    );
}

/// An env-overridable guard fans out to every configured guard's model, so
/// members of both `User` and `Admin` are offered.
#[tokio::test]
async fn request_user_fans_out_over_guards() {
    let mut files = base_files();
    files.push((
        "config/auth.php",
        "<?php return [
            'defaults' => ['guard' => env('AUTH_GUARD', 'web')],
            'guards' => [
                'web' => ['provider' => 'users'],
                'api' => ['provider' => 'admins'],
            ],
            'providers' => [
                'users' => ['model' => App\\Models\\User::class],
                'admins' => ['model' => App\\Models\\Admin::class],
            ],
        ];",
    ));

    let controller = "\
<?php
namespace App;
use Illuminate\\Http\\Request;
class C {
    public function show(Request $request) {
        $request->user()->
    }
}
";
    let labels = complete_labels(&files, "src/C.php", controller, 5, 26).await;
    assert!(
        labels.iter().any(|l| l.starts_with("isActive")),
        "expected User::isActive, got: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.starts_with("isSuperUser")),
        "expected Admin::isSuperUser from fan-out, got: {labels:?}"
    );
}

/// The `Guard` contract's `user()` is patched the same way, so a
/// `Guard`-typed value resolves the configured model.
#[tokio::test]
async fn guard_user_resolves_configured_model() {
    let mut files = base_files();
    files.push((
        "config/auth.php",
        "<?php return [
            'defaults' => ['guard' => 'web'],
            'guards' => ['web' => ['provider' => 'users']],
            'providers' => ['users' => ['model' => App\\Models\\User::class]],
        ];",
    ));

    let controller = "\
<?php
namespace App;
use Illuminate\\Contracts\\Auth\\Guard;
class C {
    public function show(Guard $guard) {
        $guard->user()->
    }
}
";
    let labels = complete_labels(&files, "src/C.php", controller, 5, 24).await;
    assert!(
        labels.iter().any(|l| l.starts_with("isActive")),
        "expected User::isActive via Guard::user(), got: {labels:?}"
    );
}

/// With no resolvable model (bare `env()`), the floor is raised to every
/// concrete class that implements `Authenticatable` in the project, so members
/// of all of them are offered rather than only the bare contract.
#[tokio::test]
async fn unresolvable_model_raises_floor_to_implementors() {
    let mut files = base_files();
    files.push((
        "config/auth.php",
        "<?php return [
            'defaults' => ['guard' => 'web'],
            'guards' => ['web' => ['provider' => 'users']],
            'providers' => ['users' => ['model' => env('AUTH_MODEL')]],
        ];",
    ));

    let controller = "\
<?php
namespace App;
use Illuminate\\Http\\Request;
class C {
    public function show(Request $request) {
        $request->user()->
    }
}
";
    let labels = complete_labels(&files, "src/C.php", controller, 5, 26).await;
    assert!(
        labels.iter().any(|l| l.starts_with("isActive")),
        "expected User::isActive from the raised floor, got: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.starts_with("isSuperUser")),
        "expected Admin::isSuperUser from the raised floor, got: {labels:?}"
    );
}
