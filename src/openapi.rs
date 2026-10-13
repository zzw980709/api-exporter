//! 把 [`Endpoint`] 序列化为 OpenAPI 3.0.3 文档（`serde_json::Value`）。

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{json, Map, Value};

use crate::model::{Body, Endpoint, Location};
use crate::schema::SchemaBuilder;

pub struct OpenApiConfig {
    pub title: String,
    pub version: String,
    pub server_url: String,
    pub context_path: String,
    pub prepend_context_path: bool,
}

pub fn generate(endpoints: &[Endpoint], schema: &SchemaBuilder, config: &OpenApiConfig) -> Value {
    let mut root = Map::new();
    root.insert("openapi".into(), json!("3.0.3"));
    root.insert(
        "info".into(),
        json!({ "title": config.title, "version": config.version }),
    );
    root.insert("servers".into(), json!([{ "url": config.server_url }]));

    let mut tags: BTreeSet<String> = BTreeSet::new();
    for endpoint in endpoints {
        if let Some(tag) = &endpoint.tag {
            if !tag.is_empty() {
                tags.insert(tag.clone());
            }
        }
    }
    if !tags.is_empty() {
        let tag_list: Vec<Value> = tags.iter().map(|t| json!({ "name": t })).collect();
        root.insert("tags".into(), Value::Array(tag_list));
    }

    let mut paths: BTreeMap<String, Value> = BTreeMap::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for endpoint in endpoints {
        let path = if config.prepend_context_path {
            join_context(&config.context_path, &endpoint.path)
        } else {
            endpoint.path.clone()
        };
        let method: String = endpoint.http_method.to_lowercase();
        if !seen.insert((path.clone(), method.clone())) {
            continue;
        }
        let entry = paths.entry(path).or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(map) = entry {
            map.insert(method, operation(endpoint));
        }
    }
    root.insert("paths".into(), Value::Object(paths.into_iter().collect()));

    let mut components = Map::new();
    let schemas: Map<String, Value> = schema
        .components()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    components.insert("schemas".into(), Value::Object(schemas));
    root.insert("components".into(), Value::Object(components));

    Value::Object(root)
}

fn operation(endpoint: &Endpoint) -> Value {
    let mut op = Map::new();
    if let Some(tag) = &endpoint.tag {
        if !tag.is_empty() {
            op.insert("tags".into(), json!([tag]));
        }
    }
    if let Some(summary) = &endpoint.summary {
        if !summary.is_empty() {
            op.insert("summary".into(), json!(summary));
        }
    }
    if let Some(description) = &endpoint.description {
        if !description.is_empty() {
            op.insert("description".into(), json!(description));
        }
    }
    if !endpoint.operation_id.is_empty() {
        op.insert("operationId".into(), json!(endpoint.operation_id));
    }
    if endpoint.deprecated {
        op.insert("deprecated".into(), json!(true));
    }

    let parameters: Vec<Value> = endpoint
        .params
        .iter()
        .filter(|p| p.location != Location::FormData)
        .map(|p| {
            let mut param = Map::new();
            param.insert("name".into(), json!(p.name));
            param.insert("in".into(), json!(p.location.as_str()));
            param.insert("required".into(), json!(p.location == Location::Path || p.required));
            if let Some(description) = &p.description {
                if !description.is_empty() {
                    param.insert("description".into(), json!(description));
                }
            }
            param.insert("schema".into(), p.schema.clone());
            if let Some(example) = &p.example {
                param.insert("example".into(), json!(example));
            }
            Value::Object(param)
        })
        .collect();
    if !parameters.is_empty() {
        op.insert("parameters".into(), Value::Array(parameters));
    }

    if let Some(body) = &endpoint.body {
        op.insert("requestBody".into(), request_body(body));
    }

    let mut ok = Map::new();
    ok.insert("description".into(), json!("成功"));
    if let Some(schema) = &endpoint.response_schema {
        if !schema.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            ok.insert(
                "content".into(),
                json!({ "application/json": { "schema": schema } }),
            );
        }
    }
    op.insert("responses".into(), json!({ "200": Value::Object(ok) }));

    Value::Object(op)
}

fn request_body(body: &Body) -> Value {
    let schema = if body.content_type == "application/json" {
        body.schema.clone().unwrap_or_else(|| json!({}))
    } else {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for field in &body.form_fields {
            properties.insert(field.name.clone(), field.schema.clone());
            if field.required {
                required.push(json!(field.name));
            }
        }
        let mut schema = json!({ "type": "object", "properties": properties });
        if !required.is_empty() {
            schema["required"] = Value::Array(required);
        }
        schema
    };
    let mut content_entry = Map::new();
    content_entry.insert("schema".into(), schema);
    let mut content = Map::new();
    content.insert(body.content_type.clone(), Value::Object(content_entry));
    let mut result = Map::new();
    result.insert("required".into(), json!(body.required));
    result.insert("content".into(), Value::Object(content));
    Value::Object(result)
}

fn join_context(context_path: &str, path: &str) -> String {
    if context_path.is_empty() || context_path == "/" {
        return path.to_string();
    }
    if path.is_empty() || path == "/" {
        return context_path.to_string();
    }
    format!("{context_path}{path}")
}
