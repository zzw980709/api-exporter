//! 与框架无关的接口模型：分析结果在这里落地，再交给 openapi 模块序列化。

use serde_json::Value;

/// 参数位置。OpenAPI 里对应 `in` 字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Query,
    Path,
    Header,
    /// multipart / urlencoded 表单字段（主体内）。
    FormData,
}

impl Location {
    pub fn as_str(self) -> &'static str {
        match self {
            Location::Query => "query",
            Location::Path => "path",
            Location::Header => "header",
            Location::FormData => "formData",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub location: Location,
    pub schema: Value,
    pub required: bool,
    pub description: Option<String>,
    pub example: Option<String>,
}

impl Param {
    pub fn new(name: impl Into<String>, location: Location) -> Self {
        Self {
            name: name.into(),
            location,
            schema: Value::Object(Default::default()),
            required: false,
            description: None,
            example: None,
        }
    }
}

/// 请求体：JSON 模型、multipart 或 x-www-form-urlencoded。
#[derive(Debug, Clone, Default)]
pub struct Body {
    pub content_type: String,
    pub schema: Option<Value>,
    pub form_fields: Vec<Param>,
    pub required: bool,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // controller / controller_fqn 保留给后续分组与调试使用
pub struct Endpoint {
    pub controller: String,
    pub controller_fqn: String,
    pub method_name: String,
    pub http_method: String,
    pub path: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub tag: Option<String>,
    pub deprecated: bool,
    pub operation_id: String,
    pub params: Vec<Param>,
    pub body: Option<Body>,
    pub response_schema: Option<Value>,
}
