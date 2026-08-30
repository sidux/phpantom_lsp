use super::{
    FrameworkClassReferenceSource, FrameworkReference, FrameworkReferenceKind, leading_spaces,
    line_offsets, normalize_framework_fqn, scalar_value, strip_yaml_quotes, valid_framework_name,
    yaml_content_before_comment, yaml_mapping_entry,
};

pub(super) fn annotate_operation_class_references(content: &str, refs: &mut [FrameworkReference]) {
    if !content.contains("x-usecase") {
        return;
    }

    let lines = line_offsets(content);
    for (idx, (line_start, line)) in lines.iter().enumerate() {
        let semantic = yaml_content_before_comment(line);
        let Some((raw_key, _, _, value_start)) = yaml_mapping_entry(semantic, *line_start) else {
            continue;
        };
        if strip_yaml_quotes(raw_key).0 != "x-usecase" {
            continue;
        }

        let Some(raw_value) = semantic.get(value_start..) else {
            continue;
        };
        let value_leading = raw_value.len() - raw_value.trim_start().len();
        let Some((raw_fqn, start, end)) = scalar_value(
            raw_value.trim_start(),
            line_start + value_start + value_leading,
        ) else {
            continue;
        };
        let fqn = normalize_framework_fqn(raw_fqn);
        if !valid_framework_name(&fqn) {
            continue;
        }

        let (method_idx, method_indent, method) = operation_method(&lines, idx)
            .map(|(idx, indent, method)| (Some(idx), Some(indent), Some(method)))
            .unwrap_or((None, None, None));
        let operation_id = method_idx
            .zip(method_indent)
            .and_then(|(method_idx, method_indent)| {
                operation_id(&lines, method_idx, method_indent)
            });
        let path = method_idx
            .zip(method_indent)
            .and_then(|(method_idx, method_indent)| {
                operation_path(&lines, method_idx, method_indent)
            });

        if let Some(reference) = refs.iter_mut().find(|reference| {
            reference.start == start as u32
                && reference.end == end as u32
                && matches!(
                    &reference.kind,
                    FrameworkReferenceKind::Class { fqn: candidate, .. }
                        if candidate.eq_ignore_ascii_case(&fqn)
                )
        }) && let FrameworkReferenceKind::Class { source, .. } = &mut reference.kind
        {
            *source = FrameworkClassReferenceSource::OpenApi {
                method,
                path,
                operation_id,
            };
        }
    }
}

fn operation_method(lines: &[(usize, &str)], usecase_idx: usize) -> Option<(usize, usize, String)> {
    let usecase_indent = leading_spaces(yaml_content_before_comment(lines.get(usecase_idx)?.1));
    for idx in (0..usecase_idx).rev() {
        let semantic = yaml_content_before_comment(lines[idx].1);
        if semantic.trim().is_empty() {
            continue;
        }
        let indent = leading_spaces(semantic);
        if indent >= usecase_indent {
            continue;
        }
        let Some((raw_key, _, _, _)) = yaml_mapping_entry(semantic, lines[idx].0) else {
            continue;
        };
        let method = strip_yaml_quotes(raw_key).0.to_ascii_lowercase();
        if is_http_method(&method) {
            return Some((idx, indent, method.to_ascii_uppercase()));
        }
    }
    None
}

fn operation_id(
    lines: &[(usize, &str)],
    method_idx: usize,
    method_indent: usize,
) -> Option<String> {
    for (line_start, line) in lines.iter().skip(method_idx + 1) {
        let semantic = yaml_content_before_comment(line);
        if semantic.trim().is_empty() {
            continue;
        }
        let indent = leading_spaces(semantic);
        if indent <= method_indent {
            break;
        }
        let Some((raw_key, _, _, value_start)) = yaml_mapping_entry(semantic, *line_start) else {
            continue;
        };
        if strip_yaml_quotes(raw_key).0 != "operationId" {
            continue;
        }
        let raw_value = semantic.get(value_start..)?.trim_start();
        let leading = semantic.get(value_start..)?.len() - raw_value.len();
        return scalar_value(raw_value, line_start + value_start + leading)
            .map(|(value, _, _)| value.to_string());
    }
    None
}

fn operation_path(
    lines: &[(usize, &str)],
    method_idx: usize,
    method_indent: usize,
) -> Option<String> {
    for (line_start, line) in lines[..method_idx].iter().rev() {
        let semantic = yaml_content_before_comment(line);
        if semantic.trim().is_empty() || leading_spaces(semantic) >= method_indent {
            continue;
        }
        if let Some((raw_key, _, _, _)) = yaml_mapping_entry(semantic, *line_start) {
            let path = strip_yaml_quotes(raw_key).0;
            if path.starts_with('/') {
                return Some(path.to_string());
            }
        }
    }

    lines[..method_idx].iter().rev().find_map(|(_, line)| {
        line.trim_start()
            .strip_prefix("# Path:")
            .map(str::trim)
            .filter(|path| path.starts_with('/'))
            .map(str::to_string)
    })
}

fn is_http_method(value: &str) -> bool {
    matches!(
        value,
        "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framework::scan_framework_references;

    fn openapi_source(content: &str) -> FrameworkClassReferenceSource {
        scan_framework_references("file:///schema/path.yml", content)
            .into_iter()
            .find_map(|reference| match reference.kind {
                FrameworkReferenceKind::Class { source, .. } => Some(source),
                _ => None,
            })
            .expect("expected an OpenAPI class reference")
    }

    #[test]
    fn reads_split_file_path_comment_and_operation_id() {
        let source = openapi_source(
            "# Path: /playlists/{playlistId}\ndelete:\n  operationId: oc_api_playlist_delete\n  x-usecase: App\\\\UseCase\\\\DeletePlaylist\n",
        );
        assert_eq!(
            source,
            FrameworkClassReferenceSource::OpenApi {
                method: Some("DELETE".to_string()),
                path: Some("/playlists/{playlistId}".to_string()),
                operation_id: Some("oc_api_playlist_delete".to_string()),
            }
        );
    }

    #[test]
    fn reads_path_from_a_monolithic_openapi_document() {
        let source = openapi_source(
            "paths:\n  /courses/{courseId}:\n    patch:\n      operationId: course_patch\n      x-usecase: App\\\\UseCase\\\\PatchCourse\n",
        );
        assert_eq!(
            source,
            FrameworkClassReferenceSource::OpenApi {
                method: Some("PATCH".to_string()),
                path: Some("/courses/{courseId}".to_string()),
                operation_id: Some("course_patch".to_string()),
            }
        );
    }
}
