//! 命令行参数（clap derive）。

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "api-exporter",
    about = "扫描 Spring Boot 源码生成 OpenAPI 3.0 并导入 Apifox",
    version
)]
pub struct Args {
    /// 项目根目录
    #[arg(long, default_value = ".")]
    pub project: PathBuf,

    /// application-{profile}.yaml 的 profile 名
    #[arg(long, default_value = "dev")]
    pub profile: String,

    /// 额外读取的 yaml 配置（覆盖 profile 配置）
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// 覆盖 server.servlet.context-path
    #[arg(long)]
    pub context_path: Option<String>,

    /// 直接覆盖 servers URL（优先级最高）
    #[arg(long)]
    pub base_url: Option<String>,

    #[arg(long, default_value = "http")]
    pub scheme: String,

    #[arg(long, default_value = "localhost")]
    pub host: String,

    /// 覆盖 server.port
    #[arg(long)]
    pub port: Option<u16>,

    /// 输出 OpenAPI 文件路径（默认 build/apifox/openapi.json）
    #[arg(long)]
    pub out: Option<PathBuf>,

    #[arg(long)]
    pub title: Option<String>,

    #[arg(long, default_value = "1.0.0")]
    pub api_version: String,

    /// 把 context-path 拼进每个接口 path（默认放进 servers URL）
    #[arg(long)]
    pub prepend_context_path: bool,

    /// Apifox 系统级访问令牌
    #[arg(long, env = "APIFOX_TOKEN")]
    pub apifox_token: Option<String>,

    /// Apifox 项目 ID
    #[arg(long, env = "APIFOX_PROJECT_ID")]
    pub apifox_project: Option<u64>,

    /// 匹配接口的处理方式: OVERWRITE_EXISTING|AUTO_MERGE|KEEP_EXISTING|CREATE_NEW
    #[arg(long, default_value = "AUTO_MERGE", env = "APIFOX_MODE")]
    pub apifox_mode: String,

    /// 目标接口目录 ID
    #[arg(long)]
    pub apifox_folder: Option<u64>,

    /// 目标分支 ID
    #[arg(long)]
    pub apifox_branch: Option<u64>,

    /// 同步删除源中不存在的接口/模型
    #[arg(long)]
    pub delete_unmatched: bool,

    /// 只生成文件，不推送 Apifox
    #[arg(long)]
    pub dry_run: bool,

    #[arg(long, short = 'v')]
    pub verbose: bool,

    /// 只导出指定分组（@Api tags）
    #[arg(long = "include-tag")]
    pub include_tags: Vec<String>,
}
