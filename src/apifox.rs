//! 调用 Apifox 开放 API 的「导入 OpenAPI」接口。
//!
//! 文档: <https://apifox-openapi.apifox.cn/api-173409873>

use serde_json::{json, Map, Value};

const API_VERSION: &str = "2024-03-28";
const BASE: &str = "https://api.apifox.com/v1/projects";

#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// OVERWRITE_EXISTING | AUTO_MERGE | KEEP_EXISTING | CREATE_NEW
    pub endpoint_overwrite_behavior: String,
    pub schema_overwrite_behavior: String,
    pub prepend_base_path: bool,
    pub update_folder_of_changed_endpoint: bool,
    pub delete_unmatched_resources: bool,
    pub target_endpoint_folder_id: Option<u64>,
    pub target_schema_folder_id: Option<u64>,
    pub target_branch_id: Option<u64>,
    pub module_id: Option<u64>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            endpoint_overwrite_behavior: "AUTO_MERGE".into(),
            schema_overwrite_behavior: "AUTO_MERGE".into(),
            prepend_base_path: false,
            update_folder_of_changed_endpoint: false,
            delete_unmatched_resources: false,
            target_endpoint_folder_id: None,
            target_schema_folder_id: None,
            target_branch_id: None,
            module_id: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct ImportResult {
    pub http_status: u16,
    pub counters: Option<Value>,
    pub raw_body: String,
    pub success: bool,
}

impl std::fmt::Display for ImportResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.counters {
            Some(c) => write!(
                f,
                "HTTP {} | 新增接口 {}, 更新接口 {}, 忽略 {}, 失败 {} | 新增模型 {}, 更新模型 {}",
                self.http_status,
                c["endpointCreated"], c["endpointUpdated"], c["endpointIgnored"], c["endpointFailed"],
                c["schemaCreated"], c["schemaUpdated"],
            ),
            None => write!(f, "HTTP {} {}", self.http_status, self.raw_body),
        }
    }
}

pub fn import_openapi(
    project_id: u64,
    token: &str,
    openapi_json: &str,
    options: &ImportOptions,
) -> anyhow::Result<ImportResult> {
    let mut opts = Map::new();
    opts.insert(
        "endpointOverwriteBehavior".into(),
        json!(options.endpoint_overwrite_behavior),
    );
    opts.insert(
        "schemaOverwriteBehavior".into(),
        json!(options.schema_overwrite_behavior),
    );
    opts.insert("prependBasePath".into(), json!(options.prepend_base_path));
    opts.insert(
        "updateFolderOfChangedEndpoint".into(),
        json!(options.update_folder_of_changed_endpoint),
    );
    opts.insert(
        "deleteUnmatchedResources".into(),
        json!(options.delete_unmatched_resources),
    );
    if let Some(id) = options.target_endpoint_folder_id {
        opts.insert("targetEndpointFolderId".into(), json!(id));
    }
    if let Some(id) = options.target_schema_folder_id {
        opts.insert("targetSchemaFolderId".into(), json!(id));
    }
    if let Some(id) = options.target_branch_id {
        opts.insert("targetBranchId".into(), json!(id));
    }
    if let Some(id) = options.module_id {
        opts.insert("moduleId".into(), json!(id));
    }

    let body = json!({ "input": openapi_json, "options": Value::Object(opts) });
    let url = format!("{BASE}/{project_id}/import-openapi?locale=zh-CN");

    let agent = build_agent()?;
    let response = agent
        .post(&url)
        .set("X-Apifox-Api-Version", API_VERSION)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", "application/json")
        .send_json(body);

    let mut result = ImportResult::default();
    match response {
        Ok(resp) => {
            result.http_status = resp.status();
            result.raw_body = resp.into_string().unwrap_or_default();
            result.success = true;
        }
        Err(ureq::Error::Status(code, resp)) => {
            result.http_status = code;
            result.raw_body = resp.into_string().unwrap_or_default();
            result.success = false;
        }
        Err(err) => return Err(anyhow::anyhow!("请求 Apifox 失败: {err}")),
    }
    if result.success {
        if let Ok(value) = serde_json::from_str::<Value>(&result.raw_body) {
            result.counters = value.get("data").and_then(|d| d.get("counters")).cloned();
        }
    }
    Ok(result)
}

/// 如果设置了 HTTPS_PROXY，则让 ureq 走代理。
fn build_agent() -> anyhow::Result<ureq::Agent> {
    let proxy = std::env::var("HTTPS_PROXY")
        .or_else(|_| std::env::var("https_proxy"))
        .ok()
        .filter(|p| !p.is_empty());
    match proxy {
        Some(addr) => {
            let proxy = ureq::Proxy::new(addr)?;
            Ok(ureq::AgentBuilder::new().proxy(proxy).build())
        }
        None => Ok(ureq::agent()),
    }
}
