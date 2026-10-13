//! 遍历 Controller，产出 [`Endpoint`] 列表。

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use tree_sitter::Node;

use super::ast;
use super::project::{type_name, Project};
use crate::model::{Body, Endpoint, Location, Param};
use crate::schema::{is_uninformative, parse_type, schema_for_simple_name, SchemaBuilder, Ty};

const MAPPINGS: &[&str] = &[
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "PatchMapping",
    "DeleteMapping",
    "RequestMapping",
];

const WEB_TYPES: &[&str] = &[
    "HttpServletRequest", "HttpServletResponse", "ServletRequest", "ServletResponse", "HttpSession",
    "HttpMethod", "HttpHeaders", "Model", "ModelMap", "ModelAndView", "BindingResult", "Errors",
    "Principal", "Locale", "TimeZone", "OutputStream", "Writer", "InputStream", "Reader",
    "MultipartHttpServletRequest", "WebRequest", "NativeWebRequest", "Pageable",
    "UriComponentsBuilder", "ServletContext",
];

pub struct Auditor<'p, 's> {
    project: &'p Project,
    schema: &'s mut SchemaBuilder<'p>,
    used_ids: HashSet<String>,
}

impl<'p, 's> Auditor<'p, 's> {
    pub fn new(project: &'p Project, schema: &'s mut SchemaBuilder<'p>) -> Self {
        Self {
            project,
            schema,
            used_ids: HashSet::new(),
        }
    }

    pub fn analyze(&mut self, include_tags: &[String]) -> Vec<Endpoint> {
        let mut endpoints = Vec::new();
        let project = self.project;
        for source_idx in 0..project.sources.len() {
            let root = project.sources[source_idx].tree.root_node();
            let controllers: Vec<Node> = super::project::top_level_types(root)
                .into_iter()
                .filter(|c| {
                    (c.kind() == "class_declaration" || c.kind() == "interface_declaration")
                        && {
                            let code = project.code(source_idx);
                            ast::has_annotation(*c, "RestController", code)
                                || ast::has_annotation(*c, "Controller", code)
                        }
                })
                .collect();
            for class in controllers {
                self.analyze_controller(class, source_idx, &mut endpoints);
            }
        }
        if !include_tags.is_empty() {
            endpoints.retain(|e| {
                e.tag
                    .as_deref()
                    .map(|t| include_tags.iter().any(|f| t.contains(f.as_str())))
                    .unwrap_or(false)
            });
        }
        endpoints
    }

    fn analyze_controller(&mut self, class: Node, source_idx: usize, out: &mut Vec<Endpoint>) {
        let code = self.project.code(source_idx);
        let class_name = type_name(class, code).to_string();
        let package = &self.project.sources[source_idx].package;
        let class_fqn = if package.is_empty() {
            class_name.clone()
        } else {
            format!("{package}.{class_name}")
        };
        let class_paths = ast::find_annotation(class, "RequestMapping", code)
            .map(|a| paths_of(a, code))
            .unwrap_or_else(|| vec![String::new()]);
        let class_tag = class_tag(class, code).unwrap_or_else(|| class_name.clone());

        let Some(body) = class.child_by_field_name("body") else {
            return;
        };
        let mut cursor = body.walk();
        let methods: Vec<Node> = body
            .named_children(&mut cursor)
            .filter(|n| n.kind() == "method_declaration")
            .collect();

        for method in methods {
            let Some(mapping) = find_mapping(method, code) else {
                continue;
            };
            let http_methods = http_methods_of(mapping, code);
            let method_paths = paths_of(mapping, code);
            let api_operation = ast::find_annotation(method, "ApiOperation", code);

            for http_method in &http_methods {
                for class_path in &class_paths {
                    for method_path in &method_paths {
                        let mut endpoint = Endpoint {
                            controller: class_name.clone(),
                            controller_fqn: class_fqn.clone(),
                            method_name: method_name(method, code),
                            http_method: http_method.clone(),
                            path: join_paths(class_path, method_path),
                            summary: summary_of(method, api_operation, code),
                            description: api_operation
                                .and_then(|a| ast::arg_string(a, "notes", code))
                                .filter(|s| !s.is_empty()),
                            tag: Some(pick_tag(api_operation, &class_tag, code)),
                            deprecated: ast::has_annotation(method, "Deprecated", code),
                            operation_id: String::new(),
                            params: Vec::new(),
                            body: None,
                            response_schema: None,
                        };
                        endpoint.operation_id =
                            self.unique_id(&format!("{class_name}_{}", endpoint.method_name));
                        self.analyze_method(method, source_idx, &mut endpoint);
                        out.push(endpoint);
                    }
                }
            }
        }
    }

    fn analyze_method(&mut self, method: Node, source_idx: usize, endpoint: &mut Endpoint) {
        let code = self.project.code(source_idx);
        let multipart = method_has_multipart(method, code);

        if let Some(params_node) = method.child_by_field_name("parameters") {
            let mut cursor = params_node.walk();
            let params: Vec<Node> = params_node
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "formal_parameter")
                .collect();
            for param in params {
                self.analyze_parameter(param, source_idx, endpoint, multipart);
            }
        }

        self.analyze_implicit_params(method, source_idx, endpoint);
        ensure_path_params(endpoint);
        endpoint.response_schema = self.response_schema(method, source_idx);
    }

    fn analyze_parameter(&mut self, param: Node, source_idx: usize, endpoint: &mut Endpoint, multipart: bool) {
        let code = self.project.code(source_idx);
        let ty = param
            .child_by_field_name("type")
            .map(|t| parse_type(t, code))
            .unwrap_or(Ty::Unknown);
        if let Some(name) = ty.base_name() {
            if WEB_TYPES.contains(&name) {
                return;
            }
        }
        let param_name = param
            .child_by_field_name("name")
            .map(|n| ast::text(n, code).to_string())
            .unwrap_or_default();

        let path_variable = ast::find_annotation(param, "PathVariable", code);
        let request_param = ast::find_annotation(param, "RequestParam", code);
        let request_body = ast::find_annotation(param, "RequestBody", code);
        let model_attribute = ast::find_annotation(param, "ModelAttribute", code);
        let request_part = ast::find_annotation(param, "RequestPart", code);
        let request_header = ast::find_annotation(param, "RequestHeader", code);

        if is_multipart_type(&ty) {
            let name = annotation_name_param(path_variable, request_param, request_part, &param_name, code);
            let required = is_required(request_param.or(request_part), false, code);
            let mut p = Param::new(name.clone(), Location::FormData);
            p.schema = json!({ "type": "string", "format": "binary" });
            p.required = required;
            p.description = implicit_description(method_of(param), &name, code);
            body_for(endpoint, "multipart/form-data").form_fields.push(p);
            return;
        }

        if let Some(ann) = path_variable {
            let name = annotation_name_param(Some(ann), None, None, &param_name, code);
            let mut p = Param::new(name.clone(), Location::Path);
            p.schema = self.schema.schema_of(&ty, source_idx, &HashMap::new());
            p.required = true;
            p.description = implicit_description(method_of(param), &name, code);
            endpoint.params.push(p);
            return;
        }

        if let Some(ann) = request_header {
            let name = annotation_name_param(None, Some(ann), None, &param_name, code);
            let mut p = Param::new(name.clone(), Location::Header);
            p.schema = self.schema.schema_of(&ty, source_idx, &HashMap::new());
            p.required = is_required(Some(ann), false, code);
            p.description = implicit_description(method_of(param), &name, code);
            endpoint.params.push(p);
            return;
        }

        if request_body.is_some() {
            let mut body = Body {
                content_type: "application/json".to_string(),
                schema: Some(self.schema.schema_of(&ty, source_idx, &HashMap::new())),
                form_fields: Vec::new(),
                required: !matches!(ast::arg_bool(request_body.unwrap(), "required", code), Some(false)),
            };
            body.form_fields.clear();
            endpoint.body = Some(body);
            return;
        }

        if let Some(ann) = request_part {
            let name = annotation_name_param(None, None, Some(ann), &param_name, code);
            let mut p = Param::new(name.clone(), Location::FormData);
            p.schema = self.schema.schema_of(&ty, source_idx, &HashMap::new());
            p.required = is_required(Some(ann), false, code);
            p.description = implicit_description(method_of(param), &name, code);
            body_for(endpoint, "multipart/form-data").form_fields.push(p);
            return;
        }

        if let Some(ann) = request_param {
            let name = annotation_name_param(None, Some(ann), None, &param_name, code);
            let mut p = Param::new(name.clone(), Location::Query);
            p.schema = self.schema.schema_of(&ty, source_idx, &HashMap::new());
            let default = ast::arg_string(ann, "defaultValue", code).filter(|s| !s.is_empty());
            p.required = is_required(Some(ann), false, code) && default.is_none();
            p.example = default;
            p.description = implicit_description(method_of(param), &name, code);
            endpoint.params.push(p);
            return;
        }

        if model_attribute.is_some() {
            self.flatten_dto(&ty, source_idx, endpoint, multipart);
            return;
        }

        // 无注解的复杂类型：Spring 会按 @ModelAttribute 处理
        if ty.base_name().is_some() {
            let simple = ty.base_name().unwrap();
            if self.project.resolve(simple, source_idx).is_some() {
                self.flatten_dto(&ty, source_idx, endpoint, multipart);
            }
        }
    }

    fn flatten_dto(&mut self, ty: &Ty, source_idx: usize, endpoint: &mut Endpoint, multipart: bool) {
        let value = self.schema.schema_of(ty, source_idx, &HashMap::new());
        let mut collected = Vec::new();
        flatten_schema(self.schema.components(), &value, "", &mut collected, 0);
        if collected.is_empty() {
            return;
        }
        if !multipart && endpoint.body.is_none() {
            endpoint.params.extend(collected);
        } else {
            let body = body_for(endpoint, if multipart { "multipart/form-data" } else { "application/x-www-form-urlencoded" });
            body.form_fields.extend(collected);
        }
    }

    fn analyze_implicit_params(&mut self, method: Node, source_idx: usize, endpoint: &mut Endpoint) {
        let code = self.project.code(source_idx);
        let annotations = ast::find_annotation_deep(method, "ApiImplicitParam", code);
        for ann in annotations {
            let Some(name) = ast::arg_string(ann, "name", code).filter(|s| !s.is_empty()) else {
                continue;
            };
            let location = match ast::arg_string(ann, "paramType", code).as_deref() {
                Some("path") => Location::Path,
                Some("header") => Location::Header,
                Some("form") => Location::FormData,
                Some("body") => continue,
                _ => Location::Query,
            };
            if endpoint
                .params
                .iter()
                .any(|p| p.name == name && p.location == location)
            {
                continue;
            }
            let schema = if let Some(class_node) = ast::arg_value_node(ann, "dataTypeClass", code) {
                match class_node
                    .named_children(&mut class_node.walk())
                    .find(|n| matches!(n.kind(), "type_identifier" | "scoped_type_identifier"))
                    .map(|n| Ty::Simple(ast::text(n, code).to_string()))
                {
                    Some(ty) => self.schema.schema_of(&ty, source_idx, &HashMap::new()),
                    None => json!({}),
                }
            } else {
                match ast::arg_string(ann, "dataType", code) {
                    Some(dt) if !dt.eq_ignore_ascii_case("object") => schema_for_simple_name(&dt),
                    _ => json!({}),
                }
            };
            let mut p = Param::new(name, location);
            p.schema = schema;
            p.required = ast::arg_bool(ann, "required", code) == Some(true);
            p.description = ast::arg_string(ann, "value", code).filter(|s| !s.is_empty());
            p.example = ast::arg_string(ann, "example", code).filter(|s| !s.is_empty());
            if location == Location::FormData {
                body_for(endpoint, "multipart/form-data").form_fields.push(p);
            } else {
                endpoint.params.push(p);
            }
        }
    }

    fn response_schema(&mut self, method: Node, source_idx: usize) -> Option<Value> {
        let code = self.project.code(source_idx);
        let return_ty = method
            .child_by_field_name("type")
            .map(|t| parse_type(t, code))
            .unwrap_or(Ty::Void);
        if !is_uninformative(&return_ty, &HashMap::new()) {
            let schema = self.schema.schema_of(&return_ty, source_idx, &HashMap::new());
            if !schema.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                return Some(schema);
            }
        }
        // 回退到 @ApiResponse(response = X.class) / @ApiOperation(response = X.class)
        let mut candidates = Vec::new();
        candidates.extend(ast::find_annotation(method, "ApiOperation", code));
        candidates.extend(ast::find_annotation(method, "ApiResponse", code));
        candidates.extend(ast::find_annotation_deep(method, "ApiResponse", code));
        for ann in candidates {
            let Some(value_node) = ast::arg_value_node(ann, "response", code) else {
                continue;
            };
            let Some(type_node) = ast::descendants(value_node)
                .into_iter()
                .find(|n| matches!(n.kind(), "type_identifier" | "scoped_type_identifier"))
            else {
                continue;
            };
            let ty = Ty::Simple(ast::text(type_node, code).to_string());
            let mut schema = self.schema.schema_of(&ty, source_idx, &HashMap::new());
            if ast::arg_string(ann, "responseContainer", code).as_deref() == Some("list") {
                schema = json!({ "type": "array", "items": schema });
            }
            return Some(schema);
        }
        None
    }

    fn unique_id(&mut self, base: &str) -> String {
        let mut candidate = base.to_string();
        let mut n = 2;
        while !self.used_ids.insert(candidate.clone()) {
            candidate = format!("{base}_{n}");
            n += 1;
        }
        candidate
    }
}

// ---------------------------------------------------------------- helpers

fn flatten_schema(
    components: &std::collections::BTreeMap<String, Value>,
    schema: &Value,
    prefix: &str,
    out: &mut Vec<Param>,
    depth: usize,
) {
    if depth > 3 {
        return;
    }
    let resolved = resolve_ref(components, schema);
    let Some(props) = resolved.get("properties").and_then(|p| p.as_object()) else {
        return;
    };
    let required: Vec<String> = resolved
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    for (name, prop) in props {
        let full = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        let prop_resolved = resolve_ref(components, prop);
        if prop_resolved.get("properties").is_some() {
            flatten_schema(components, prop, &full, out, depth + 1);
            continue;
        }
        let mut p = Param::new(full, Location::Query);
        p.schema = prop.clone();
        p.required = required.contains(name);
        p.description = prop_resolved
            .get("description")
            .and_then(|d| d.as_str())
            .map(String::from);
        out.push(p);
    }
}

fn resolve_ref<'v>(components: &'v std::collections::BTreeMap<String, Value>, schema: &'v Value) -> &'v Value {
    if let Some(reference) = schema.get("$ref").and_then(|r| r.as_str()) {
        if let Some(name) = reference.rsplit('/').next() {
            if let Some(component) = components.get(name) {
                return component;
            }
        }
    }
    schema
}

fn find_mapping<'t>(method: Node<'t>, code: &str) -> Option<Node<'t>> {
    MAPPINGS.iter().find_map(|name| ast::find_annotation(method, name, code))
}

fn http_methods_of(annotation: Node, code: &str) -> Vec<String> {
    let name = ast::annotation_name(annotation, code);
    let fixed = match name {
        "GetMapping" => Some("GET"),
        "PostMapping" => Some("POST"),
        "PutMapping" => Some("PUT"),
        "PatchMapping" => Some("PATCH"),
        "DeleteMapping" => Some("DELETE"),
        _ => None,
    };
    if let Some(m) = fixed {
        return vec![m.to_string()];
    }
    let methods = ast::arg_strings(annotation, "method", code);
    if methods.is_empty() {
        return vec!["GET".to_string()];
    }
    methods
        .iter()
        .map(|m| {
            let v = m.rsplit('.').next().unwrap_or(m);
            v.to_uppercase()
        })
        .collect()
}

fn paths_of(annotation: Node, code: &str) -> Vec<String> {
    let mut value = ast::arg_strings(annotation, "value", code);
    if value.is_empty() {
        value = ast::arg_strings(annotation, "path", code);
    }
    if value.is_empty() {
        value.push(String::new());
    }
    value
}

fn summary_of(method: Node, api_operation: Option<Node>, code: &str) -> Option<String> {
    if let Some(value) = api_operation.and_then(|a| ast::arg_string(a, "value", code)) {
        if !value.is_empty() {
            return Some(value);
        }
    }
    // 退而求其次，取上一行的 javadoc 第一行
    method
        .prev_sibling()
        .filter(|n| matches!(n.kind(), "block_comment" | "line_comment"))
        .map(|c| {
            ast::text(c, code)
                .trim_start_matches("/**")
                .trim_start_matches("/*")
                .lines()
                .map(|l| l.trim().trim_start_matches('*').trim())
                .find(|l| !l.is_empty() && !l.starts_with('@'))
                .unwrap_or("")
                .to_string()
        })
        .filter(|s| !s.is_empty())
}

fn class_tag(class: Node, code: &str) -> Option<String> {
    let api = ast::find_annotation(class, "Api", code)?;
    let tags: Vec<String> = ast::arg_strings(api, "tags", code)
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect();
    if let Some(first) = tags.first() {
        return Some(first.clone());
    }
    ast::arg_string(api, "value", code).filter(|s| !s.trim().is_empty())
}

fn pick_tag(api_operation: Option<Node>, class_tag: &str, code: &str) -> String {
    if let Some(ann) = api_operation {
        let tags: Vec<String> = ast::arg_strings(ann, "tags", code)
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect();
        if let Some(first) = tags.first() {
            return first.clone();
        }
    }
    if class_tag.trim().is_empty() {
        return String::new();
    }
    class_tag.to_string()
}

fn method_name(method: Node, code: &str) -> String {
    method
        .child_by_field_name("name")
        .map(|n| ast::text(n, code).to_string())
        .unwrap_or_default()
}

fn join_paths(class_path: &str, method_path: &str) -> String {
    let a = normalize(class_path);
    let b = normalize(method_path);
    if a.is_empty() {
        if b.is_empty() {
            "/".to_string()
        } else {
            b
        }
    } else if b.is_empty() {
        a
    } else {
        format!("{a}{b}")
    }
}

fn normalize(path: &str) -> String {
    let mut p = path.trim().to_string();
    if p.is_empty() {
        return String::new();
    }
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    while p.len() > 1 && p.ends_with('/') {
        p.pop();
    }
    p
}

fn annotation_name_param(
    a: Option<Node>,
    b: Option<Node>,
    c: Option<Node>,
    fallback: &str,
    code: &str,
) -> String {
    for ann in [a, b, c].into_iter().flatten() {
        if let Some(v) = ast::arg_string(ann, "value", code) {
            if !v.is_empty() {
                return v;
            }
        }
        if let Some(v) = ast::arg_string(ann, "name", code) {
            if !v.is_empty() {
                return v;
            }
        }
    }
    fallback.to_string()
}

fn is_required(annotation: Option<Node>, default_required: bool, code: &str) -> bool {
    let Some(ann) = annotation else {
        return default_required;
    };
    let has_default = ast::arg_string(ann, "defaultValue", code).map(|s| !s.is_empty()).unwrap_or(false);
    if has_default {
        return false;
    }
    match ast::arg_bool(ann, "required", code) {
        Some(required) => required,
        None => default_required,
    }
}

fn ensure_path_params(endpoint: &mut Endpoint) {
    let mut placeholders = Vec::new();
    let bytes = endpoint.path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = endpoint.path[i + 1..].find('}') {
                placeholders.push(endpoint.path[i + 1..i + 1 + end].to_string());
                i += end + 2;
                continue;
            }
        }
        i += 1;
    }
    for placeholder in placeholders {
        let exists = endpoint
            .params
            .iter()
            .any(|p| p.location == Location::Path && p.name == placeholder);
        if !exists {
            let mut p = Param::new(placeholder, Location::Path);
            p.schema = json!({ "type": "string" });
            p.required = true;
            endpoint.params.insert(0, p);
        }
    }
    // path 参数放最前
    endpoint.params.sort_by_key(|p| match p.location {
        Location::Path => 0,
        Location::Query => 1,
        Location::Header => 2,
        Location::FormData => 3,
    });
}

fn body_for<'e>(endpoint: &'e mut Endpoint, content_type: &str) -> &'e mut Body {
    if endpoint.body.is_none() {
        endpoint.body = Some(Body {
            content_type: content_type.to_string(),
            schema: None,
            form_fields: Vec::new(),
            required: true,
        });
    }
    endpoint.body.as_mut().unwrap()
}

fn method_has_multipart(method: Node, code: &str) -> bool {
    let Some(params) = method.child_by_field_name("parameters") else {
        return false;
    };
    let mut cursor = params.walk();
    let found = params.named_children(&mut cursor).any(|p| {
        p.kind() == "formal_parameter"
            && p.child_by_field_name("type")
                .map(|t| is_multipart_type(&parse_type(t, code)))
                .unwrap_or(false)
    });
    found
}

fn is_multipart_type(ty: &Ty) -> bool {
    match ty {
        Ty::Array(inner) => is_multipart_type(inner),
        Ty::Simple(name) | Ty::Generic(name, _) => {
            let simple = name.rsplit('.').next().unwrap_or(name);
            simple == "MultipartFile" || simple == "Part"
        }
        _ => false,
    }
}

fn method_of(node: Node) -> Node {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "method_declaration" {
            return n;
        }
        current = n.parent();
    }
    node
}

fn implicit_description(method: Node, name: &str, code: &str) -> Option<String> {
    ast::find_annotation_deep(method, "ApiImplicitParam", code)
        .into_iter()
        .find(|a| ast::arg_string(*a, "name", code).as_deref() == Some(name))
        .and_then(|a| ast::arg_string(a, "value", code))
        .filter(|s| !s.is_empty())
}
