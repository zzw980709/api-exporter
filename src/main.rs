//! api-exporter：扫描 Spring Boot 源码 -> OpenAPI 3.0 -> Apifox

mod apifox;
mod cli;
mod config;
mod java;
mod model;
mod openapi;
mod schema;

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use clap::Parser;

use crate::model::Endpoint;

fn main() {
    let args = cli::Args::parse();
    if let Err(err) = run(&args) {
        eprintln!("[error] {err:#}");
        if args.verbose {
            eprintln!("{err:?}");
        }
        std::process::exit(1);
    }
}

fn run(args: &cli::Args) -> Result<()> {
    let project_dir = args
        .project
        .canonicalize()
        .unwrap_or_else(|_| args.project.clone());
    let source_root = project_dir.join("src/main/java");
    if !source_root.is_dir() {
        bail!("找不到源码目录: {}", source_root.display());
    }

    if args.verbose {
        println!("[info] 项目目录 : {}", project_dir.display());
        println!("[info] profile  : {}", args.profile);
    }

    // 1. 解析所有源码
    let project = java::project::Project::scan(&source_root)?;

    // 2. 生成 endpoints（同时登记 components）
    let mut builder = schema::SchemaBuilder::new(&project);
    let endpoints = {
        let mut auditor = java::controllers::Auditor::new(&project, &mut builder);
        auditor.analyze(&args.include_tags)
    };

    // 3. 读取 context-path / port
    let server = config::read_server_config(&project_dir, &args.profile, args.config.as_deref());
    let context_path = args
        .context_path
        .clone()
        .unwrap_or_else(|| server.context_path.clone());
    let port = args.port.or(server.port).unwrap_or(8080);
    let server_url = args.base_url.clone().unwrap_or_else(|| {
        format!("{}://{}:{}{}", args.scheme, args.host, port, context_path)
    });

    let title = args.title.clone().unwrap_or_else(|| {
        project_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "API".to_string())
    });

    // 4. 生成 OpenAPI
    let openapi_config = openapi::OpenApiConfig {
        title,
        version: args.api_version.clone(),
        server_url: server_url.clone(),
        context_path: context_path.clone(),
        prepend_context_path: args.prepend_context_path,
    };
    let document = openapi::generate(&endpoints, &builder, &openapi_config);

    // 5. 写文件
    let out = args
        .out
        .clone()
        .unwrap_or_else(|| project_dir.join("build/apifox/openapi.json"));
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(&document)?;
    std::fs::write(&out, &json).with_context(|| format!("写入 {} 失败", out.display()))?;

    // 6. 摘要
    println!("=========================================================");
    println!("api-exporter 完成");
    println!("  扫描类型   : {} 个 (解析失败 {})", project.by_fqn.len(), project.parse_errors);
    println!("  接口数量   : {}", endpoints.len());
    println!("  数据模型   : {}", builder.components().len());
    println!(
        "  context-path: {}   [{}]",
        if context_path.is_empty() { "(空)" } else { &context_path },
        server.describe()
    );
    println!("  servers    : {server_url}");
    println!("  输出文件   : {}", out.display());
    println!("=========================================================");
    print_breakdown(&endpoints);

    // 7. 推送 Apifox
    if args.dry_run {
        println!("[info] --dry-run，跳过推送 Apifox");
    } else if let (Some(token), Some(project_id)) = (args.apifox_token.as_ref(), args.apifox_project) {
        push_apifox(args, token, project_id, &json)?;
    } else {
        println!("[info] 未提供 Apifox token / project，仅生成文件。");
        println!("       推送示例: --apifox-token <token> --apifox-project <id>");
    }

    Ok(())
}

fn print_breakdown(endpoints: &[Endpoint]) {
    let mut by_method: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_tag: BTreeMap<String, usize> = BTreeMap::new();
    for endpoint in endpoints {
        *by_method.entry(endpoint.http_method.clone()).or_default() += 1;
        *by_tag
            .entry(endpoint.tag.clone().unwrap_or_else(|| "(未分组)".to_string()))
            .or_default() += 1;
    }
    println!("按方法统计: {by_method:?}");
    println!("分组数量  : {}", by_tag.len());
    println!("全部分组  : {:?}", by_tag.keys().collect::<Vec<_>>());
}

fn push_apifox(args: &cli::Args, token: &str, project_id: u64, json: &str) -> Result<()> {
    let schema_mode = if args.apifox_mode == "AUTO_MERGE" {
        "AUTO_MERGE".to_string()
    } else {
        args.apifox_mode.clone()
    };
    let options = apifox::ImportOptions {
        endpoint_overwrite_behavior: args.apifox_mode.clone(),
        schema_overwrite_behavior: schema_mode,
        delete_unmatched_resources: args.delete_unmatched,
        target_endpoint_folder_id: args.apifox_folder,
        target_branch_id: args.apifox_branch,
        ..Default::default()
    };
    println!(
        "[info] 正在推送到 Apifox 项目 {} (mode={}) ...",
        project_id, args.apifox_mode
    );
    let result = apifox::import_openapi(project_id, token, json, &options)?;
    println!("[apifox] {result}");
    if !result.success {
        bail!("推送失败: {}", result.raw_body);
    }
    Ok(())
}
