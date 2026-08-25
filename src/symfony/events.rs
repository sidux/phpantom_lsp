//! Symfony event metadata, navigation, references, and lenses.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use tower_lsp::lsp_types::{CodeLens, Command, Location, Position, Range, Url};

use super::container::{EventSubscription, load_compiled_container};
use super::php_attributes::{
    PhpArgument, argument_value, attribute_calls, configured_argument, is_php_identifier,
    method_after_attribute, php_arguments, string_argument,
};
use crate::Backend;
use crate::config::{
    SymfonyEventPublisherConfig, SymfonyEventSubscriberConfig, SymfonyEventsConfig,
};
use crate::text_position::{offset_to_position, position_to_offset};
use crate::types::ClassInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventRole {
    Publisher,
    Subscriber,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventSite {
    event: String,
    owner_fqn: String,
    method: String,
    uri: String,
    start: u32,
    end: u32,
    event_start: Option<u32>,
    event_end: Option<u32>,
    role: EventRole,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SymfonyEventIndex {
    sources: BTreeMap<String, Vec<EventSite>>,
    container_path: Option<std::path::PathBuf>,
    subscriptions: Vec<EventSubscription>,
    proxied_classes: Vec<String>,
}

impl SymfonyEventIndex {
    fn replace_source(&mut self, uri: String, sites: Vec<EventSite>) {
        if sites.is_empty() {
            self.sources.remove(&uri);
        } else {
            self.sources.insert(uri, sites);
        }
    }

    fn reset_container(
        &mut self,
        path: Option<std::path::PathBuf>,
        subscriptions: Vec<EventSubscription>,
        proxied_classes: Vec<String>,
    ) {
        self.container_path = path;
        self.subscriptions = subscriptions;
        self.proxied_classes = proxied_classes;
    }

    fn clear_sources(&mut self) {
        self.sources.clear();
    }

    fn source_sites(&self) -> Vec<EventSite> {
        self.sources
            .values()
            .flat_map(|sites| sites.iter().cloned())
            .collect()
    }
}

impl Backend {
    /// Rebuild Symfony metadata from the newest compiled container.
    pub(crate) fn rebuild_symfony_metadata(&self, workspace_root: &Path) -> usize {
        let config = self.config().symfony;
        if config.events.publishers.is_empty() && config.events.subscribers.is_empty() {
            let mut index = self.symfony_events.write();
            index.clear_sources();
            index.reset_container(None, Vec::new(), Vec::new());
            return 0;
        }
        let metadata = load_compiled_container(workspace_root, &config.container);
        let (path, subscriptions, proxied_classes) = metadata.map_or_else(
            || (None, Vec::new(), Vec::new()),
            |metadata| {
                (
                    Some(metadata.path),
                    metadata.subscriptions,
                    metadata.proxied_classes,
                )
            },
        );

        {
            let mut index = self.symfony_events.write();
            index.clear_sources();
            index.reset_container(path, subscriptions, proxied_classes.clone());
        }

        // `createProxy(new RealClass(...))` narrows attribute scanning to the
        // classes the runtime actually decorates. Files opened later are kept
        // fresh by `update_ast`, so no workspace-wide source walk is needed.
        for class_fqn in proxied_classes {
            let Some(uri) = self.resolve_class_uri(&class_fqn).or_else(|| {
                self.find_or_load_class(&class_fqn);
                self.resolve_class_uri(&class_fqn)
            }) else {
                continue;
            };
            let Some(content) = self.get_file_content(&uri) else {
                continue;
            };
            self.find_or_load_class(&class_fqn);
            self.refresh_symfony_event_sites(&uri, &content);
        }

        // Initialization and config reload can race with didOpen. Re-scan the
        // current buffers after clearing old rules so a container refresh
        // cannot discard attribute sites the editor already published.
        let open_files: Vec<(String, std::sync::Arc<String>)> = self
            .open_files
            .read()
            .iter()
            .map(|(uri, content)| (uri.clone(), std::sync::Arc::clone(content)))
            .collect();
        for (uri, content) in open_files {
            self.refresh_symfony_event_sites(&uri, &content);
        }

        let index = self.symfony_events.read();
        index.subscriptions.len()
            + index
                .sources
                .values()
                .map(|sites| sites.len())
                .sum::<usize>()
    }

    /// Refresh configured publisher/subscriber attributes in one PHP file.
    pub(crate) fn refresh_symfony_event_sites(&self, uri: &str, content: &str) {
        if !uri_path(uri).ends_with(".php") {
            return;
        }
        let event_config = self.config().symfony.events;
        if event_config.publishers.is_empty() && event_config.subscribers.is_empty() {
            self.symfony_events
                .write()
                .replace_source(uri.to_string(), Vec::new());
            return;
        }

        let classes = self
            .symbols
            .uri_classes_index
            .read()
            .get(uri)
            .cloned()
            .unwrap_or_default();
        let use_map = self
            .file_imports
            .read()
            .get(uri)
            .cloned()
            .unwrap_or_default();
        let mut sites = Vec::new();

        for attribute in attribute_calls(content) {
            let raw_name = &content[attribute.name_start..attribute.name_end];
            let namespace = crate::text_scan::namespace_at_offset(content, attribute.name_start)
                .map(str::to_string);
            let attribute_fqn =
                normalize_fqn(&crate::util::resolve_to_fqn(raw_name, &use_map, &namespace));
            let Some((method_start, method_end)) =
                method_after_attribute(content, attribute.group_end)
            else {
                continue;
            };
            let Some(owner) = class_at_method(&classes, method_start) else {
                continue;
            };
            let owner_fqn = self.canonical_metadata_class(&owner.fqn());
            let method = content[method_start..method_end].to_string();
            let arguments = attribute
                .args
                .map_or_else(Vec::new, |(start, end)| php_arguments(content, start, end));

            for rule in event_config
                .publishers
                .iter()
                .filter(|rule| normalize_fqn(&rule.attribute).eq_ignore_ascii_case(&attribute_fqn))
            {
                scan_publisher_attribute(
                    uri,
                    content,
                    &arguments,
                    rule,
                    &owner_fqn,
                    &method,
                    method_start,
                    method_end,
                    &mut sites,
                );
            }
            for rule in event_config
                .subscribers
                .iter()
                .filter(|rule| normalize_fqn(&rule.attribute).eq_ignore_ascii_case(&attribute_fqn))
            {
                scan_subscriber_attribute(
                    uri,
                    content,
                    &arguments,
                    rule,
                    &owner_fqn,
                    &method,
                    method_start,
                    method_end,
                    &mut sites,
                );
            }
        }

        sites.sort_by(|left, right| {
            left.start
                .cmp(&right.start)
                .then(left.event.cmp(&right.event))
                .then((left.role as u8).cmp(&(right.role as u8)))
        });
        sites.dedup();
        self.symfony_events
            .write()
            .replace_source(uri.to_string(), sites);
    }

    pub(crate) fn remove_symfony_event_sites(&self, uri: &str) {
        self.symfony_events
            .write()
            .replace_source(uri.to_string(), Vec::new());
    }

    pub(crate) fn symfony_event_lenses(
        &self,
        classes: &[std::sync::Arc<ClassInfo>],
        uri: &str,
        content: &str,
    ) -> Vec<CodeLens> {
        let (sites, subscriptions) = self.symfony_event_snapshot();
        if sites.is_empty() {
            return Vec::new();
        }
        let events_config = self.config().symfony.events;
        let mut lenses = Vec::new();

        for class in classes {
            let owner = self.canonical_metadata_class(&class.fqn());
            for method in &class.methods {
                if method.is_virtual || method.name_offset == 0 {
                    continue;
                }
                let method_name = method.name.as_str();
                let publishers: Vec<&EventSite> = sites
                    .iter()
                    .filter(|site| {
                        site.role == EventRole::Publisher
                            && same_class(&site.owner_fqn, &owner)
                            && site.method.eq_ignore_ascii_case(method_name)
                    })
                    .collect();
                let subscriber_events: Vec<&str> = subscriptions
                    .iter()
                    .filter(|subscription| {
                        same_class(
                            &self.canonical_metadata_class(&subscription.listener_fqn),
                            &owner,
                        ) && subscription.method.eq_ignore_ascii_case(method_name)
                    })
                    .map(|subscription| subscription.event.as_str())
                    .chain(sites.iter().filter_map(|site| {
                        (site.role == EventRole::Subscriber
                            && same_class(&site.owner_fqn, &owner)
                            && site.method.eq_ignore_ascii_case(method_name))
                        .then_some(site.event.as_str())
                    }))
                    .collect();

                if !publishers.is_empty() {
                    let mut locations: Vec<Location> = subscriptions
                        .iter()
                        .filter(|subscription| {
                            publishers.iter().any(|publisher| {
                                event_names_match(
                                    &publisher.event,
                                    &subscription.event,
                                    &events_config,
                                )
                            })
                        })
                        .filter_map(|subscription| {
                            self.class_member_declaration_location(
                                &self.canonical_metadata_class(&subscription.listener_fqn),
                                &subscription.method,
                            )
                        })
                        .collect();
                    locations.extend(
                        sites
                            .iter()
                            .filter(|site| {
                                site.role == EventRole::Subscriber
                                    && publishers.iter().any(|publisher| {
                                        event_names_match(
                                            &publisher.event,
                                            &site.event,
                                            &events_config,
                                        )
                                    })
                            })
                            .filter_map(|site| self.source_site_location(site)),
                    );
                    let locations = dedupe_locations(locations);
                    if let Some(lens) =
                        self.event_lens(uri, content, method.name_offset, "subscriber", locations)
                    {
                        lenses.push(lens);
                    }
                }

                if !subscriber_events.is_empty() {
                    let locations = dedupe_locations(
                        sites
                            .iter()
                            .filter(|site| {
                                site.role == EventRole::Publisher
                                    && subscriber_events.iter().any(|event| {
                                        event_names_match(event, &site.event, &events_config)
                                    })
                            })
                            .filter_map(|site| self.source_site_location(site))
                            .collect(),
                    );
                    if let Some(lens) =
                        self.event_lens(uri, content, method.name_offset, "publisher", locations)
                    {
                        lenses.push(lens);
                    }
                }
            }
        }
        lenses
    }

    pub(crate) fn symfony_event_definitions_at(
        &self,
        uri: &str,
        content: &str,
        position: Position,
    ) -> Option<Vec<Location>> {
        let subjects = self.symfony_event_subjects_at(uri, content, position);
        if subjects.is_empty() {
            return None;
        }
        let (sites, subscriptions) = self.symfony_event_snapshot();
        let config = self.config().symfony.events;
        let mut locations = Vec::new();
        for (event, role) in subjects {
            match role {
                EventRole::Publisher => {
                    locations.extend(
                        subscriptions
                            .iter()
                            .filter(|subscription| {
                                event_names_match(&event, &subscription.event, &config)
                            })
                            .filter_map(|subscription| {
                                self.class_member_declaration_location(
                                    &self.canonical_metadata_class(&subscription.listener_fqn),
                                    &subscription.method,
                                )
                            }),
                    );
                    locations.extend(
                        sites
                            .iter()
                            .filter(|site| {
                                site.role == EventRole::Subscriber
                                    && event_names_match(&event, &site.event, &config)
                            })
                            .filter_map(|site| self.source_site_location(site)),
                    );
                }
                EventRole::Subscriber => {
                    locations.extend(
                        sites
                            .iter()
                            .filter(|site| {
                                site.role == EventRole::Publisher
                                    && event_names_match(&event, &site.event, &config)
                            })
                            .filter_map(|site| self.source_site_location(site)),
                    );
                }
            }
        }
        let locations = dedupe_locations(locations);
        (!locations.is_empty()).then_some(locations)
    }

    pub(crate) fn symfony_event_references_at(
        &self,
        uri: &str,
        content: &str,
        position: Position,
        include_declaration: bool,
    ) -> Option<Vec<Location>> {
        let subjects = self.symfony_event_subjects_at(uri, content, position);
        if subjects.is_empty() {
            return None;
        }
        let (sites, subscriptions) = self.symfony_event_snapshot();
        let config = self.config().symfony.events;
        let mut locations = Vec::new();
        for (event, _) in subjects {
            if include_declaration {
                locations.extend(
                    sites
                        .iter()
                        .filter(|site| {
                            site.role == EventRole::Publisher
                                && event_names_match(&event, &site.event, &config)
                        })
                        .filter_map(|site| self.source_site_location(site)),
                );
            }
            locations.extend(
                subscriptions
                    .iter()
                    .filter(|subscription| event_names_match(&event, &subscription.event, &config))
                    .filter_map(|subscription| {
                        self.class_member_declaration_location(
                            &self.canonical_metadata_class(&subscription.listener_fqn),
                            &subscription.method,
                        )
                    }),
            );
            locations.extend(
                sites
                    .iter()
                    .filter(|site| {
                        site.role == EventRole::Subscriber
                            && event_names_match(&event, &site.event, &config)
                    })
                    .filter_map(|site| self.source_site_location(site)),
            );
        }
        Some(dedupe_locations(locations))
    }

    fn symfony_event_subjects_at(
        &self,
        uri: &str,
        content: &str,
        position: Position,
    ) -> Vec<(String, EventRole)> {
        let offset = position_to_offset(content, position);
        let (sites, subscriptions) = self.symfony_event_snapshot();
        let mut subjects: Vec<(String, EventRole)> = sites
            .iter()
            .filter(|site| {
                site.uri == uri
                    && (contains_offset(site.start, site.end, offset)
                        || site
                            .event_start
                            .zip(site.event_end)
                            .is_some_and(|(start, end)| contains_offset(start, end, offset)))
            })
            .map(|site| (site.event.clone(), site.role))
            .collect();

        if let Some((owner, method)) = method_name_at_offset(self, uri, offset) {
            let owner = self.canonical_metadata_class(&owner);
            subjects.extend(
                subscriptions
                    .iter()
                    .filter(|subscription| {
                        same_class(
                            &self.canonical_metadata_class(&subscription.listener_fqn),
                            &owner,
                        ) && subscription.method.eq_ignore_ascii_case(&method)
                    })
                    .map(|subscription| (subscription.event.clone(), EventRole::Subscriber)),
            );
        }

        subjects.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then((left.1 as u8).cmp(&(right.1 as u8)))
        });
        subjects.dedup();
        subjects
    }

    fn symfony_event_snapshot(&self) -> (Vec<EventSite>, Vec<EventSubscription>) {
        let index = self.symfony_events.read();
        (index.source_sites(), index.subscriptions.clone())
    }

    fn canonical_metadata_class(&self, fqn: &str) -> String {
        self.metadata_class_family(fqn)
            .into_iter()
            .next()
            .unwrap_or_else(|| normalize_fqn(fqn))
    }

    fn source_site_location(&self, site: &EventSite) -> Option<Location> {
        let uri: Url = site.uri.parse().ok()?;
        let content = self.get_file_content(&site.uri)?;
        let position = offset_to_position(&content, site.start as usize);
        Some(Location::new(uri, Range::new(position, position)))
    }

    fn event_lens(
        &self,
        uri: &str,
        content: &str,
        method_offset: u32,
        target: &str,
        locations: Vec<Location>,
    ) -> Option<CodeLens> {
        if locations.is_empty() {
            return None;
        }
        let position = offset_to_position(content, method_offset as usize);
        let line_start = content[..method_offset as usize]
            .rfind('\n')
            .map_or(0, |offset| offset + 1);
        let indent = content[line_start..method_offset as usize]
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .count() as u32;
        let origin = Position::new(position.line, indent);
        let title = format!(
            "Symfony event: {} {}{}",
            locations.len(),
            target,
            if locations.len() == 1 { "" } else { "s" }
        );
        let origin_uri: Url = uri.parse().ok()?;
        let command = if locations.len() == 1
            && self
                .supports_show_document
                .load(std::sync::atomic::Ordering::Acquire)
        {
            Command {
                title,
                command: "phpantom.navigateToPrototype".to_string(),
                arguments: Some(vec![
                    serde_json::json!(locations[0].uri),
                    serde_json::json!(locations[0].range.start),
                ]),
            }
        } else {
            Command {
                title,
                command: "editor.action.showReferences".to_string(),
                arguments: Some(vec![
                    serde_json::json!(origin_uri),
                    serde_json::json!(origin),
                    serde_json::json!(locations),
                ]),
            }
        };
        Some(CodeLens {
            range: Range::new(origin, origin),
            command: Some(command),
            data: None,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_publisher_attribute(
    uri: &str,
    content: &str,
    arguments: &[PhpArgument<'_>],
    rule: &SymfonyEventPublisherConfig,
    owner_fqn: &str,
    method: &str,
    method_start: usize,
    method_end: usize,
    sites: &mut Vec<EventSite>,
) {
    if rule.name_template.trim().is_empty() {
        return;
    }
    let explicit =
        configured_argument(arguments, rule.name_argument.as_deref(), rule.name_position).and_then(
            |argument| {
                let raw = argument_value(content, argument);
                if raw.eq_ignore_ascii_case("null") {
                    None
                } else {
                    string_argument(content, argument)
                }
            },
        );

    let dispatches = configured_argument(
        arguments,
        rule.dispatch_argument.as_deref(),
        rule.dispatch_position,
    )
    .map_or_else(
        || rule.default_dispatch.clone(),
        |argument| dispatch_values(argument_value(content, argument), rule),
    );

    for dispatch in dispatches {
        if should_skip_dispatch(content, arguments, rule, &dispatch) {
            continue;
        }
        let (event, event_start, event_end) = if let Some((name, start, end)) = &explicit {
            (
                render_event_template(
                    rule.explicit_name_template.as_deref().unwrap_or("{name}"),
                    owner_fqn,
                    method,
                    &dispatch,
                    Some(name),
                    &rule.default_methods,
                ),
                Some(*start as u32),
                Some(*end as u32),
            )
        } else {
            (
                render_event_template(
                    &rule.name_template,
                    owner_fqn,
                    method,
                    &dispatch,
                    None,
                    &rule.default_methods,
                ),
                None,
                None,
            )
        };
        if event.is_empty() {
            continue;
        }
        sites.push(EventSite {
            event,
            owner_fqn: owner_fqn.to_string(),
            method: method.to_string(),
            uri: uri.to_string(),
            start: method_start as u32,
            end: method_end as u32,
            event_start,
            event_end,
            role: EventRole::Publisher,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_subscriber_attribute(
    uri: &str,
    content: &str,
    arguments: &[PhpArgument<'_>],
    rule: &SymfonyEventSubscriberConfig,
    owner_fqn: &str,
    method: &str,
    method_start: usize,
    method_end: usize,
    sites: &mut Vec<EventSite>,
) {
    let Some(argument) =
        configured_argument(arguments, rule.name_argument.as_deref(), rule.name_position)
    else {
        return;
    };
    let Some((mut event, event_start, event_end)) = string_argument(content, argument) else {
        return;
    };
    if let Some(transport) = configured_argument(
        arguments,
        rule.transport_argument.as_deref(),
        rule.transport_position,
    ) {
        let raw = argument_value(content, transport);
        for (case, suffix) in &rule.transport_cases {
            if enum_case_present(raw, case) {
                event.push_str(suffix);
                break;
            }
        }
    }
    sites.push(EventSite {
        event,
        owner_fqn: owner_fqn.to_string(),
        method: method.to_string(),
        uri: uri.to_string(),
        start: method_start as u32,
        end: method_end as u32,
        event_start: Some(event_start as u32),
        event_end: Some(event_end as u32),
        role: EventRole::Subscriber,
    });
}

fn dispatch_values(value: &str, rule: &SymfonyEventPublisherConfig) -> Vec<String> {
    let decoded =
        crate::text_scan::decode_php_string_literal(value.trim()).map(|value| value.into_owned());
    let mut dispatches = Vec::new();
    for (case, dispatch) in &rule.dispatch_cases {
        if enum_case_present(value, case)
            || decoded
                .as_deref()
                .is_some_and(|value| value == case || value == dispatch)
        {
            dispatches.push(dispatch.clone());
        }
    }
    dispatches.sort();
    dispatches.dedup();
    dispatches
}

fn should_skip_dispatch(
    content: &str,
    arguments: &[PhpArgument<'_>],
    rule: &SymfonyEventPublisherConfig,
    dispatch: &str,
) -> bool {
    rule.skip.iter().any(|skip| {
        skip.dispatch.eq_ignore_ascii_case(dispatch)
            && configured_argument(arguments, Some(&skip.argument), skip.position).is_some_and(
                |argument| !argument_value(content, argument).eq_ignore_ascii_case("null"),
            )
    })
}

fn render_event_template(
    template: &str,
    owner_fqn: &str,
    method: &str,
    dispatch: &str,
    explicit_name: Option<&str>,
    default_methods: &[String],
) -> String {
    let short_class = owner_fqn.rsplit('\\').next().unwrap_or(owner_fqn);
    let default_method = method.is_empty()
        || default_methods
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(method));
    let method_suffix = if default_method {
        String::new()
    } else {
        format!(".{method}")
    };
    let method_suffix_snake = if default_method {
        String::new()
    } else {
        format!(".{}", snake_case(method))
    };
    template
        .replace("{dispatch}", dispatch)
        .replace("{class}", short_class)
        .replace("{class_snake}", &snake_case(short_class))
        .replace("{method}", method)
        .replace("{method_snake}", &snake_case(method))
        .replace("{method_suffix}", &method_suffix)
        .replace("{method_suffix_snake}", &method_suffix_snake)
        .replace("{name}", explicit_name.unwrap_or_default())
}

fn snake_case(value: &str) -> String {
    let mut snake = String::with_capacity(value.len() + 8);
    let mut previous_is_word = false;
    for character in value.chars() {
        if character.is_ascii_uppercase() && previous_is_word {
            snake.push('_');
        }
        snake.extend(character.to_lowercase());
        previous_is_word = character.is_alphanumeric() || character == '_';
    }
    snake
}

fn event_names_match(lhs: &str, rhs: &str, config: &SymfonyEventsConfig) -> bool {
    canonical_event_name(lhs, config) == canonical_event_name(rhs, config)
}

fn canonical_event_name<'a>(mut event: &'a str, config: &SymfonyEventsConfig) -> &'a str {
    loop {
        let mut changed = false;
        if let Some(stripped) = config
            .ignored_prefixes
            .iter()
            .find_map(|prefix| event.strip_prefix(prefix))
        {
            event = stripped;
            changed = true;
        }
        if let Some(stripped) = config
            .ignored_suffixes
            .iter()
            .find_map(|suffix| event.strip_suffix(suffix))
        {
            event = stripped;
            changed = true;
        }
        if !changed {
            return event;
        }
    }
}

fn enum_case_present(value: &str, case: &str) -> bool {
    let needle = format!("::{case}");
    value.match_indices(&needle).any(|(start, _)| {
        value
            .as_bytes()
            .get(start + needle.len())
            .is_none_or(|byte| !is_php_identifier(*byte))
    })
}

fn class_at_method(classes: &[std::sync::Arc<ClassInfo>], offset: usize) -> Option<&ClassInfo> {
    classes
        .iter()
        .find(|class| class.start_offset as usize <= offset && offset <= class.end_offset as usize)
        .map(AsRef::as_ref)
}

fn method_name_at_offset(backend: &Backend, uri: &str, offset: u32) -> Option<(String, String)> {
    let classes = backend.symbols.uri_classes_index.read();
    for class in classes.get(uri)? {
        for method in &class.methods {
            if contains_offset(
                method.name_offset,
                method.name_offset + method.name.len() as u32,
                offset,
            ) {
                return Some((class.fqn().to_string(), method.name.to_string()));
            }
        }
    }
    None
}

fn dedupe_locations(locations: Vec<Location>) -> Vec<Location> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for location in locations {
        let key = (
            location.uri.to_string(),
            location.range.start.line,
            location.range.start.character,
        );
        if seen.insert(key) {
            unique.push(location);
        }
    }
    unique.sort_by(|left, right| {
        left.uri
            .as_str()
            .cmp(right.uri.as_str())
            .then(left.range.start.line.cmp(&right.range.start.line))
            .then(left.range.start.character.cmp(&right.range.start.character))
    });
    unique
}

fn contains_offset(start: u32, end: u32, offset: u32) -> bool {
    start <= offset && offset <= end
}

fn same_class(left: &str, right: &str) -> bool {
    normalize_fqn(left).eq_ignore_ascii_case(&normalize_fqn(right))
}

fn normalize_fqn(name: &str) -> String {
    name.trim().trim_start_matches('\\').to_string()
}

fn uri_path(uri: &str) -> &str {
    uri.strip_prefix("file://")
        .unwrap_or(uri)
        .split('?')
        .next()
        .unwrap_or(uri)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_proxy_rule() -> SymfonyEventPublisherConfig {
        SymfonyEventPublisherConfig {
            attribute: "OpenClassrooms\\ServiceProxy\\Attribute\\Event".to_string(),
            name_argument: Some("name".to_string()),
            name_position: Some(2),
            dispatch_argument: Some("dispatch".to_string()),
            dispatch_position: Some(4),
            default_dispatch: vec!["post".to_string()],
            dispatch_cases: [
                ("PRE".to_string(), "pre".to_string()),
                ("POST".to_string(), "post".to_string()),
                ("EXCEPTION".to_string(), "exception".to_string()),
            ]
            .into_iter()
            .collect(),
            name_template: "{dispatch}.{class_snake}{method_suffix_snake}".to_string(),
            explicit_name_template: Some("{name}".to_string()),
            default_methods: vec!["execute".to_string(), "__invoke".to_string()],
            skip: vec![crate::config::SymfonyEventSkipConfig {
                dispatch: "post".to_string(),
                argument: "messageClass".to_string(),
                position: Some(5),
            }],
        }
    }

    #[test]
    fn renders_configured_event_names() {
        assert_eq!(
            render_event_template(
                "{dispatch}.{class_snake}{method_suffix_snake}",
                "App\\UseCase\\HTTPReport",
                "refreshCache",
                "post",
                None,
                &["execute".to_string(), "__invoke".to_string()],
            ),
            "post.h_t_t_p_report.refresh_cache"
        );
        assert_eq!(
            render_event_template(
                "{dispatch}.{class_snake}{method_suffix_snake}",
                "App\\UseCase\\PublishCourse",
                "execute",
                "post",
                None,
                &["execute".to_string()],
            ),
            "post.publish_course"
        );
    }

    #[test]
    fn configured_aliases_match_compiled_event_names() {
        let config = SymfonyEventsConfig {
            ignored_prefixes: vec!["use_case.".to_string()],
            ignored_suffixes: vec![".async".to_string()],
            ..SymfonyEventsConfig::default()
        };
        assert!(event_names_match(
            "post.publish_course",
            "use_case.post.publish_course.async",
            &config
        ));
    }

    #[test]
    fn publisher_rule_uses_named_arguments_and_conditional_dispatch_skips() {
        let content = "dispatch: [On::PRE, On::POST], messageClass: CoursePublished::class";
        let arguments = php_arguments(content, 0, content.len());
        let mut sites = Vec::new();
        scan_publisher_attribute(
            "file:///project/PublishCourse.php",
            content,
            &arguments,
            &service_proxy_rule(),
            "App\\UseCase\\PublishCourse",
            "execute",
            0,
            "execute".len(),
            &mut sites,
        );

        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].event, "pre.publish_course");
    }

    #[test]
    fn explicit_publisher_names_bypass_the_derived_template() {
        let content = "name: 'course.failed'";
        let arguments = php_arguments(content, 0, content.len());
        let mut sites = Vec::new();
        scan_publisher_attribute(
            "file:///project/PublishCourse.php",
            content,
            &arguments,
            &service_proxy_rule(),
            "App\\UseCase\\PublishCourse",
            "execute",
            0,
            "execute".len(),
            &mut sites,
        );

        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].event, "course.failed");
        assert_eq!(sites[0].event_start, Some(7));
    }
}
