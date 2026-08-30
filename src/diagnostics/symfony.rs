//! Conservative diagnostics for project-local Symfony named resources.

use std::collections::HashSet;

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Range};

use crate::Backend;
use crate::diagnostics::helpers::make_diagnostic;
use crate::diagnostics::unknown_members::UNKNOWN_MEMBER_CODE;
use crate::framework::{FrameworkReferenceKind, SymfonyExpressionAccessKind, SymfonySymbolKind};
use crate::text_position::offset_to_position;

impl Backend {
    /// Report missing PHP members used by configured ExpressionLanguage strings.
    pub(crate) fn collect_unknown_symfony_expression_diagnostics(
        &self,
        uri: &str,
        content: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        let Some(references) = self.framework_references.read().get(uri).cloned() else {
            return;
        };
        for reference in references.iter() {
            let FrameworkReferenceKind::SymfonyExpression {
                variable,
                path,
                expression_start,
                context,
            } = &reference.kind
            else {
                continue;
            };
            let Some(target) = path.last() else {
                continue;
            };
            let Some(classes) = self.symfony_expression_missing_classes(
                uri,
                variable,
                path,
                *expression_start,
                context,
            ) else {
                continue;
            };
            let kind = match target.kind {
                SymfonyExpressionAccessKind::Property => "Property",
                SymfonyExpressionAccessKind::Method => "Method",
            };
            let class_display = if classes.len() == 1 {
                format!("class '{}'", classes[0])
            } else {
                format!("any possible type ({})", classes.join(", "))
            };
            out.push(make_diagnostic(
                Range {
                    start: offset_to_position(content, reference.start as usize),
                    end: offset_to_position(content, reference.end as usize),
                },
                DiagnosticSeverity::WARNING,
                UNKNOWN_MEMBER_CODE,
                format!("{kind} '{}' not found on {class_display}", target.name),
            ));
        }
    }

    pub(crate) fn collect_unknown_symfony_resource_diagnostics(
        &self,
        uri: &str,
        content: &str,
        out: &mut Vec<Diagnostic>,
    ) {
        let Some(references) = self.framework_references.read().get(uri).cloned() else {
            return;
        };
        let known_services = self
            .framework_symfony_symbol_names(SymfonySymbolKind::Service)
            .into_iter()
            .collect::<HashSet<_>>();
        let known_parameters = self
            .framework_symfony_symbol_names(SymfonySymbolKind::Parameter)
            .into_iter()
            .collect::<HashSet<_>>();
        let known_routes = self
            .framework_symfony_symbol_names(SymfonySymbolKind::Route)
            .into_iter()
            .collect::<HashSet<_>>();
        let known_templates = self
            .framework_symfony_symbol_names(SymfonySymbolKind::Template)
            .into_iter()
            .collect::<HashSet<_>>();
        let known_events = self
            .framework_symfony_symbol_names(SymfonySymbolKind::Event)
            .into_iter()
            .collect::<HashSet<_>>();
        let known_buses = self
            .framework_symfony_symbol_names(SymfonySymbolKind::MessengerBus)
            .into_iter()
            .collect::<HashSet<_>>();
        let mut translation_domains = HashSet::new();
        let mut known_translations = HashSet::new();
        let mut config_roots = HashSet::new();
        let mut known_config_keys = HashSet::new();
        for refs in self.framework_references.read().values() {
            for reference in refs.iter() {
                if let FrameworkReferenceKind::ConfigKey {
                    path,
                    declaration: true,
                } = &reference.kind
                {
                    config_roots.insert(path.split('.').next().unwrap_or_default().to_string());
                    known_config_keys.insert(path.clone());
                    continue;
                }
                let FrameworkReferenceKind::Translation {
                    domain,
                    name,
                    declaration: true,
                } = &reference.kind
                else {
                    continue;
                };
                translation_domains.insert(domain.clone());
                known_translations.insert((domain.clone(), name.clone()));
            }
        }

        for reference in references.iter() {
            if let FrameworkReferenceKind::ConfigKey {
                path,
                declaration: false,
            } = &reference.kind
            {
                let root = path.split('.').next().unwrap_or_default();
                if config_roots.contains(root) && !known_config_keys.contains(path) {
                    out.push(Diagnostic {
                        range: Range {
                            start: offset_to_position(content, reference.start as usize),
                            end: offset_to_position(content, reference.end as usize),
                        },
                        severity: Some(DiagnosticSeverity::WARNING),
                        code: Some(NumberOrString::String(
                            "unknown_symfony_config_key".to_string(),
                        )),
                        source: Some("PHPantom".to_string()),
                        message: format!("Symfony configuration key '{}' is not declared", path),
                        ..Default::default()
                    });
                }
                continue;
            }
            if let FrameworkReferenceKind::Translation {
                domain,
                name,
                declaration: false,
            } = &reference.kind
            {
                if translation_domains.contains(domain)
                    && !known_translations.contains(&(domain.clone(), name.clone()))
                {
                    out.push(Diagnostic {
                        range: Range {
                            start: offset_to_position(content, reference.start as usize),
                            end: offset_to_position(content, reference.end as usize),
                        },
                        severity: Some(DiagnosticSeverity::WARNING),
                        code: Some(NumberOrString::String(
                            "unknown_symfony_translation".to_string(),
                        )),
                        source: Some("PHPantom".to_string()),
                        message: format!(
                            "Symfony translation '{}' is not declared in the '{}' domain",
                            name, domain
                        ),
                        ..Default::default()
                    });
                }
                continue;
            }
            let FrameworkReferenceKind::SymfonySymbol {
                kind,
                name,
                declaration: false,
            } = &reference.kind
            else {
                continue;
            };

            let known = match kind {
                SymfonySymbolKind::Service => {
                    known_services.contains(name)
                        || (name.starts_with("App\\") && self.find_or_load_class(name).is_some())
                }
                SymfonySymbolKind::Parameter => known_parameters.contains(name),
                SymfonySymbolKind::Route => known_routes.contains(name),
                SymfonySymbolKind::RouteParameter => true,
                SymfonySymbolKind::Template => known_templates.contains(name),
                SymfonySymbolKind::Translation => true,
                SymfonySymbolKind::Event => known_events.contains(name),
                SymfonySymbolKind::MessengerBus => known_buses.contains(name),
            };
            if known || !is_project_local_name(*kind, name) {
                continue;
            }

            let label = kind.label();
            out.push(Diagnostic {
                range: Range {
                    start: offset_to_position(content, reference.start as usize),
                    end: offset_to_position(content, reference.end as usize),
                },
                severity: Some(DiagnosticSeverity::WARNING),
                code: Some(NumberOrString::String(format!(
                    "unknown_symfony_{}",
                    kind.diagnostic_name()
                ))),
                source: Some("PHPantom".to_string()),
                message: format!("Symfony {label} '{}' is not declared", name),
                ..Default::default()
            });
        }
    }
}

fn is_project_local_name(kind: SymfonySymbolKind, name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("app.")
        || (kind == SymfonySymbolKind::Service && name.starts_with("App\\"))
        || (kind == SymfonySymbolKind::Route && lower.starts_with("app_"))
        || (kind == SymfonySymbolKind::Template
            && name.to_ascii_lowercase().ends_with(".twig")
            && !name.starts_with(['@', '/', '\\'])
            && !name.starts_with("./")
            && !name.starts_with("../"))
        || (kind == SymfonySymbolKind::Event
            && (lower.starts_with("app.") || lower.starts_with("app_")))
        || (kind == SymfonySymbolKind::MessengerBus
            && (lower.starts_with("app.") || lower.starts_with("app_")))
}
