//! Java 类型 -> OpenAPI Schema，并把复杂 DTO 登记为 `components.schemas`。
//!
//! 这里不依赖 classpath：类型解析完全走 [`Project`] 建立的源码索引。
//! 泛型用 [`Ty`]（owned 的小枚举）表示，从而摆脱 tree-sitter 节点的生命周期困扰。

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{json, Map, Value};
use tree_sitter::Node;

use crate::java::ast;
use crate::java::project::{type_name, Project};

const UNWRAP: &[&str] = &["ResponseEntity", "HttpEntity", "RequestEntity", "Callable", "DeferredResult", "CompletableFuture"];
const LIST_TYPES: &[&str] = &[
    "List", "ArrayList", "LinkedList", "Set", "HashSet", "LinkedHashSet", "TreeSet",
    "Collection", "Iterable", "Queue", "Deque", "SortedSet",
];
const MAP_TYPES: &[&str] = &[
    "Map", "HashMap", "LinkedHashMap", "TreeMap", "SortedMap", "ConcurrentMap", "ConcurrentHashMap",
];
/// 这些泛型“包一层”的响应对象直接内联展开，而不是生成组件。
const INLINE_BOXES: &[&str] = &["DataResponse", "PageResponse", "PageResult", "ApiResponse", "Result"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Simple(String),
    Generic(String, Vec<Ty>),
    Array(Box<Ty>),
    Wildcard(Option<Box<Ty>>),
    Void,
    Unknown,
}

impl Ty {
    pub fn base_name(&self) -> Option<&str> {
        match self {
            Ty::Simple(n) | Ty::Generic(n, _) => Some(n),
            _ => None,
        }
    }

    pub fn args(&self) -> &[Ty] {
        match self {
            Ty::Generic(_, a) => a,
            _ => &[],
        }
    }
}

pub fn parse_type(node: Node<'_>, src: &str) -> Ty {
    match node.kind() {
        "void_type" => Ty::Void,
        "integral_type" | "floating_point_type" | "boolean_type" | "primitive_type" => {
            Ty::Simple(ast::text(node, src).to_string())
        }
        "type_identifier" | "scoped_type_identifier" | "identifier" => {
            Ty::Simple(ast::text(node, src).to_string())
        }
        "generic_type" => {
            let base = node
                .named_child(0)
                .map(|n| ast::text(n, src).to_string())
                .unwrap_or_default();
            let args = ast::child_of_kind(node, "type_arguments")
                .map(|a| parse_type_args(a, src))
                .unwrap_or_default();
            Ty::Generic(base, args)
        }
        "array_type" => node
            .named_child(0)
            .map(|n| Ty::Array(Box::new(parse_type(n, src))))
            .unwrap_or(Ty::Unknown),
        "wildcard" => Ty::Wildcard(node.named_child(0).map(|n| Box::new(parse_type(n, src)))),
        _ => Ty::Unknown,
    }
}

fn parse_type_args(args: Node<'_>, src: &str) -> Vec<Ty> {
    let mut cursor = args.walk();
    args.named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .map(|n| parse_type(n, src))
        .collect()
}

fn last_segment(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).to_string()
}

pub struct SchemaBuilder<'a> {
    project: &'a Project,
    components: BTreeMap<String, Value>,
    fqn_to_name: HashMap<String, String>,
    in_progress: HashSet<String>,
}

impl<'a> SchemaBuilder<'a> {
    pub fn new(project: &'a Project) -> Self {
        Self {
            project,
            components: BTreeMap::new(),
            fqn_to_name: HashMap::new(),
            in_progress: HashSet::new(),
        }
    }

    pub fn components(&self) -> &BTreeMap<String, Value> {
        &self.components
    }

    pub fn schema_of(&mut self, ty: &Ty, from: usize, tyvars: &HashMap<String, Ty>) -> Value {
        self.schema_at(ty, from, tyvars, 0)
    }

    fn schema_at(&mut self, ty: &Ty, from: usize, tyvars: &HashMap<String, Ty>, depth: usize) -> Value {
        if depth > 64 {
            return json!({});
        }
        match ty {
            Ty::Void | Ty::Unknown => json!({}),
            Ty::Array(inner) => {
                json!({ "type": "array", "items": self.schema_at(inner, from, tyvars, depth + 1) })
            }
            Ty::Wildcard(Some(b)) => self.schema_at(b, from, tyvars, depth + 1),
            Ty::Wildcard(None) => json!({}),
            Ty::Simple(name) => self.named_schema(name, &[], from, tyvars, depth + 1),
            Ty::Generic(name, args) => self.named_schema(name, args, from, tyvars, depth + 1),
        }
    }

    fn named_schema(
        &mut self,
        name: &str,
        args: &[Ty],
        from: usize,
        tyvars: &HashMap<String, Ty>,
        depth: usize,
    ) -> Value {
        if let Some(sub) = tyvars.get(name) {
            return self.schema_at(sub, from, tyvars, depth + 1);
        }
        if UNWRAP.contains(&name) {
            let inner = args.first().map(|a| self.schema_at(a, from, tyvars, depth + 1));
            let mut inner = inner.unwrap_or_else(|| json!({}));
            if name == "Optional" {
                inner["nullable"] = json!(true);
            }
            return inner;
        }
        if LIST_TYPES.contains(&name) {
            let items = args
                .first()
                .map(|a| self.schema_at(a, from, tyvars, depth + 1))
                .unwrap_or(json!({}));
            return json!({ "type": "array", "items": items });
        }
        if MAP_TYPES.contains(&name) {
            let value = args
                .get(1)
                .map(|a| self.schema_at(a, from, tyvars, depth + 1))
                .unwrap_or(json!({}));
            return json!({ "type": "object", "additionalProperties": value });
        }
        if let Some(primitive) = primitive_schema(name) {
            return primitive;
        }

        let project = self.project;
        let simple = last_segment(name);
        let Some(source_idx) = project.resolve(name, from) else {
            return json!({});
        };
        let Some(decl) = project.find_type_decl(source_idx, &simple) else {
            return json!({});
        };
        let code = project.code(source_idx);
        if decl.kind() == "enum_declaration" {
            return enum_schema(decl, code);
        }
        if INLINE_BOXES.contains(&name) && !args.is_empty() {
            return self.inline_expand(decl, source_idx, args, tyvars, depth);
        }
        self.ref_to(decl, source_idx, &simple, args, tyvars, depth)
    }

    fn inline_expand(
        &mut self,
        decl: Node,
        source_idx: usize,
        args: &[Ty],
        tyvars: &HashMap<String, Ty>,
        depth: usize,
    ) -> Value {
        let project = self.project;
        let code = project.code(source_idx);
        let simple = type_name(decl, code).to_string();
        let package = &project.sources[source_idx].package;
        let fqn = if package.is_empty() {
            simple.clone()
        } else {
            format!("{package}.{simple}")
        };
        // 内联展开同样需要防环，否则自引用的包装类会无限递归
        let key = self.component_name(&fqn, &simple, args);
        if !self.in_progress.insert(key.clone()) {
            return json!({ "type": "object" });
        }
        let mut subst = tyvars.clone();
        for (p, a) in type_params(decl, code).iter().zip(args) {
            subst.insert(p.clone(), a.clone());
        }
        let mut props = Map::new();
        let mut required = Vec::new();
        let mut visited = HashSet::new();
        self.collect_fields(decl, source_idx, &subst, &mut props, &mut required, &mut visited, depth + 1);
        self.in_progress.remove(&key);
        let mut schema = json!({ "type": "object", "properties": props });
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        schema
    }

    fn ref_to(
        &mut self,
        decl: Node,
        source_idx: usize,
        simple: &str,
        args: &[Ty],
        tyvars: &HashMap<String, Ty>,
        depth: usize,
    ) -> Value {
        let project = self.project;
        let code = project.code(source_idx);
        let package = &project.sources[source_idx].package;
        let fqn = if package.is_empty() {
            simple.to_string()
        } else {
            format!("{package}.{simple}")
        };
        let component = self.component_name(&fqn, simple, args);
        let reference = json!({ "$ref": format!("#/components/schemas/{component}") });
        if self.components.contains_key(&component) || self.in_progress.contains(&component) {
            return reference;
        }
        self.in_progress.insert(component.clone());

        let mut subst = tyvars.clone();
        for (p, a) in type_params(decl, code).iter().zip(args) {
            subst.insert(p.clone(), a.clone());
        }
        let mut props = Map::new();
        let mut required = Vec::new();
        let mut visited = HashSet::new();
        self.collect_fields(decl, source_idx, &subst, &mut props, &mut required, &mut visited, depth + 1);

        let mut schema = json!({ "type": "object", "properties": props });
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        self.components.insert(component.clone(), schema);
        self.in_progress.remove(&component);
        reference
    }

    fn component_name(&mut self, fqn: &str, simple: &str, args: &[Ty]) -> String {
        if args.is_empty() {
            if let Some(existing) = self.fqn_to_name.get(fqn) {
                return existing.clone();
            }
            let mut name = simple.to_string();
            if self.components.contains_key(&name) {
                name = fqn.replace('.', "_");
            }
            self.fqn_to_name.insert(fqn.to_string(), name.clone());
            name
        } else {
            let mut name = simple.to_string();
            for arg in args {
                name.push('_');
                name.push_str(&ty_name(arg));
            }
            name
        }
    }

    fn collect_fields(
        &mut self,
        decl: Node,
        source_idx: usize,
        subst: &HashMap<String, Ty>,
        props: &mut Map<String, Value>,
        required: &mut Vec<String>,
        visited: &mut HashSet<String>,
        depth: usize,
    ) {
        if depth > 16 {
            return;
        }
        let project = self.project;
        let code = project.code(source_idx);
        let simple = type_name(decl, code);
        let package = &project.sources[source_idx].package;
        let fqn = if package.is_empty() {
            simple.to_string()
        } else {
            format!("{package}.{simple}")
        };
        if !visited.insert(fqn) {
            return;
        }

        // 先处理父类，让子类字段可覆盖同名父类字段
        if let Some(super_node) = decl.child_by_field_name("superclass") {
            if let Some(type_node) = super_node.named_child(0) {
                let parent_ty = parse_type(type_node, code);
                if let Some(parent_name) = parent_ty.base_name() {
                    let parent_simple = last_segment(parent_name);
                    if let Some(parent_idx) = project.resolve(parent_name, source_idx) {
                        if let Some(parent_decl) = project.find_type_decl(parent_idx, &parent_simple) {
                            let parent_code = project.code(parent_idx);
                            let mut parent_subst = subst.clone();
                            for (p, a) in type_params(parent_decl, parent_code).iter().zip(parent_ty.args()) {
                                parent_subst.insert(p.clone(), a.clone());
                            }
                            self.collect_fields(
                                parent_decl, parent_idx, &parent_subst, props, required, visited, depth + 1,
                            );
                        }
                    }
                }
            }
        }

        let Some(body) = decl.child_by_field_name("body") else {
            return;
        };
        let mut cursor = body.walk();
        let members: Vec<Node> = body.named_children(&mut cursor).collect();

        for field in &members {
            if field.kind() != "field_declaration" {
                continue;
            }
            if ast::has_modifier(*field, "static", code) || ast::has_modifier(*field, "transient", code) {
                continue;
            }
            if ast::find_annotation(*field, "JsonIgnore", code).is_some() {
                continue;
            }
            let json_property = ast::find_annotation(*field, "JsonProperty", code);
            let api_model = ast::find_annotation(*field, "ApiModelProperty", code);
            if api_model.map(|a| ast::arg_bool(a, "hidden", code) == Some(true)).unwrap_or(false) {
                continue;
            }
            let description = api_model
                .and_then(|a| ast::arg_string(a, "value", code))
                .filter(|s| !s.is_empty())
                .or_else(|| api_model.and_then(|a| ast::arg_string(a, "notes", code)))
                .filter(|s| !s.is_empty());
            let example = api_model.and_then(|a| ast::arg_string(a, "example", code)).filter(|s| !s.is_empty());
            let json_name = json_property.and_then(|a| ast::arg_string(a, "value", code)).filter(|s| !s.is_empty());
            let required_field = ["NotNull", "NotBlank", "NotEmpty", "NonNull"]
                .iter()
                .any(|a| ast::has_annotation(*field, a, code))
                || json_property.and_then(|a| ast::arg_bool(a, "required", code)) == Some(true);

            let field_type = field.child_by_field_name("type");
            let mut decl_cursor = field.walk();
            let declarators: Vec<Node> = field
                .named_children(&mut decl_cursor)
                .filter(|n| n.kind() == "variable_declarator")
                .collect();
            for declarator in declarators {
                let var_name = declarator
                    .child_by_field_name("name")
                    .map(|n| ast::text(n, code))
                    .unwrap_or("");
                if var_name.is_empty() {
                    continue;
                }
                let prop_name = json_name.clone().unwrap_or_else(|| var_name.to_string());
                let mut schema = match field_type {
                    Some(t) => {
                        let ty = parse_type(t, code);
                        self.schema_of(&ty, source_idx, subst)
                    }
                    None => json!({}),
                };
                attach_meta(&mut schema, description.as_deref(), example.as_deref());
                props.insert(prop_name.clone(), schema);
                if required_field {
                    required.push(prop_name);
                }
            }
        }

        apply_getter_json_names(&members, code, props);
    }
}

fn attach_meta(schema: &mut Value, description: Option<&str>, example: Option<&str>) {
    if let Some(d) = description {
        if !d.is_empty() {
            schema["description"] = json!(d);
        }
    }
    if let Some(e) = example {
        if !e.is_empty() {
            schema["example"] = json!(e);
        }
    }
}

/// 处理 `@JsonProperty` 打在 getter 上的情况。
fn apply_getter_json_names(members: &[Node], code: &str, props: &mut Map<String, Value>) {
    for method in members {
        if method.kind() != "method_declaration" {
            continue;
        }
        let Some(json_property) = ast::find_annotation(*method, "JsonProperty", code) else {
            continue;
        };
        let params = method.child_by_field_name("parameters");
        if params.map(|p| p.named_child_count() > 0).unwrap_or(true) {
            continue;
        }
        let Some(name_node) = method.child_by_field_name("name") else {
            continue;
        };
        let method_name = ast::text(name_node, code);
        let property = if let Some(rest) = method_name.strip_prefix("get") {
            decapitalize(rest)
        } else if let Some(rest) = method_name.strip_prefix("is") {
            decapitalize(rest)
        } else {
            continue;
        };
        if !props.contains_key(&property) {
            continue;
        }
        if let Some(json_name) = ast::arg_string(json_property, "value", code) {
            if !json_name.is_empty() && json_name != property {
                if let Some(value) = props.remove(&property) {
                    props.insert(json_name, value);
                }
            }
        }
    }
}

fn type_params(decl: Node<'_>, code: &str) -> Vec<String> {
    let Some(node) = ast::child_of_kind(decl, "type_parameters") else {
        return Vec::new();
    };
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|n| n.kind() == "type_parameter")
        .map(|p| {
            p.named_child(0)
                .map(|n| ast::text(n, code).to_string())
                .unwrap_or_default()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn enum_schema(decl: Node<'_>, code: &str) -> Value {
    let mut values = Vec::new();
    if let Some(body) = decl.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.named_children(&mut cursor) {
            if child.kind() == "enum_constant" {
                if let Some(name) = child.child_by_field_name("name") {
                    values.push(json!(ast::text(name, code)));
                }
            }
        }
    }
    let mut schema = json!({ "type": "string" });
    if !values.is_empty() {
        schema["enum"] = json!(values);
    }
    schema
}

fn ty_name(ty: &Ty) -> String {
    match ty {
        Ty::Simple(n) => n.clone(),
        Ty::Generic(n, args) => {
            let mut s = n.clone();
            for a in args {
                s.push_str("Of");
                s.push_str(&ty_name(a));
            }
            s
        }
        Ty::Array(inner) => format!("{}Array", ty_name(inner)),
        Ty::Wildcard(_) => "Wildcard".to_string(),
        Ty::Void => "Void".to_string(),
        Ty::Unknown => "Object".to_string(),
    }
}

fn decapitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn primitive_schema(name: &str) -> Option<Value> {
    let schema = match name {
        "String" | "CharSequence" | "char" | "Character" => json!({ "type": "string" }),
        "int" | "Integer" | "short" | "Short" | "byte" | "Byte" => {
            json!({ "type": "integer", "format": "int32" })
        }
        "long" | "Long" | "BigInteger" => json!({ "type": "integer", "format": "int64" }),
        "float" | "Float" => json!({ "type": "number", "format": "float" }),
        "double" | "Double" | "BigDecimal" | "Number" => json!({ "type": "number", "format": "double" }),
        "boolean" | "Boolean" => json!({ "type": "boolean" }),
        "LocalDate" => json!({ "type": "string", "format": "date" }),
        "LocalDateTime" | "Instant" | "ZonedDateTime" | "OffsetDateTime" | "Date" | "Timestamp" => {
            json!({ "type": "string", "format": "date-time" })
        }
        "LocalTime" | "OffsetTime" => json!({ "type": "string", "format": "time" }),
        "UUID" => json!({ "type": "string", "format": "uuid" }),
        "MultipartFile" | "Part" | "Resource" | "InputStream" => {
            json!({ "type": "string", "format": "binary" })
        }
        "Object" | "JsonNode" | "Class" | "JsonElement" => json!({}),
        _ => return None,
    };
    Some(schema)
}

/// 供 `@ApiImplicitParam(dataType=...)` 使用。
pub fn schema_for_simple_name(name: &str) -> Value {
    primitive_schema(name).unwrap_or_else(|| json!({}))
}

/// 判断一个类型是否“没有信息”（Object / 通配符 / void）。
pub fn is_uninformative(ty: &Ty, tyvars: &HashMap<String, Ty>) -> bool {
    match ty {
        Ty::Void | Ty::Unknown => true,
        Ty::Wildcard(None) => true,
        Ty::Wildcard(Some(b)) => is_uninformative(b, tyvars),
        Ty::Array(inner) => is_uninformative(inner, tyvars),
        Ty::Simple(name) => {
            if let Some(sub) = tyvars.get(name) {
                return is_uninformative(sub, tyvars);
            }
            matches!(name.as_str(), "Object" | "JsonNode" | "JsonElement")
        }
        Ty::Generic(name, args) => {
            if UNWRAP.contains(&name.as_str()) || LIST_TYPES.contains(&name.as_str()) {
                return args.first().map(|a| is_uninformative(a, tyvars)).unwrap_or(true);
            }
            matches!(name.as_str(), "Object" | "JsonNode" | "JsonElement")
        }
    }
}
