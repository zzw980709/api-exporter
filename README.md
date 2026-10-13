# api-exporter

静态扫描 Spring Boot 源码，生成 **OpenAPI 3.0** 文档，并可一键导入 **Apifox**。

用 Rust 写成，核心解析交给 [`tree-sitter-java`](https://github.com/tree-sitter/tree-sitter-java)，
**不需要启动服务、不需要 classpath**。

## 它识别什么

| 目标 | 支持 |
| --- | --- |
| 类/方法路径 | `@RestController`/`@Controller`、类级 `@RequestMapping`、方法级 `@GetMapping/PostMapping/PutMapping/PatchMapping/DeleteMapping/RequestMapping`（含多值、多方法） |
| `context-path` | 从 `src/main/resources/**/application*.yaml\|yml\|properties` 按 profile 读取 `server.servlet.context-path`（支持多文档 `spring.config.activate.on-profile`） |
| `server.port` | 同上；用于组装 `servers.url` |
| GET 参数 | `@RequestParam`、`@PathVariable`、`@RequestHeader`、`@ModelAttribute`/裸 DTO（**自动把 DTO 字段拍平为 query 参数**） |
| JSON 参数 | `@RequestBody` -> `application/json`；字段名取 **`@JsonProperty`**，描述取 `@ApiModelProperty` |
| 表单/上传 | `MultipartFile`/`@RequestPart` -> `multipart/form-data`；非 GET 的 `@ModelAttribute` -> `application/x-www-form-urlencoded` |
| 模型 | 字段类型、`List`/`Map`/数组/枚举、泛型（`DataResponse<T>` 内联展开）、**父类字段继承**、`@NotNull` 等 required、`@ApiModelProperty(example)` |
| 文档 | `@Api(tags)` / `@ApiOperation(value,notes,tags)` / `@ApiImplicitParam(s)` / `@ApiResponse(response=X.class)` |
| 响应 | 返回值类型；`ResponseEntity<Object>` 这类擦除类型回退到 `@ApiResponse(response=...)` |

## 快速开始

```bash
# 1. 编译（首次会下载依赖）
cargo build --release

# 2. 生成 OpenAPI（默认写到 <项目>/build/apifox/openapi.json）
./target/release/api-exporter --project ../sample-spring-project --profile dev

# 3. 生成 + 推送到 Apifox
export APIFOX_TOKEN=你的系统级访问令牌
./target/release/api-exporter \
  --project ../sample-spring-project --profile dev \
  --apifox-project 123456 \
  --apifox-mode AUTO_MERGE
```

## 参数

| 参数 | 说明 |
| --- | --- |
| `--project <dir>` | 项目根目录（默认 `.`） |
| `--profile <name>` | `application-{profile}.yaml`（默认 `dev`） |
| `--context-path <p>` | 覆盖 context-path |
| `--base-url <url>` | 直接覆盖 `servers.url` |
| `--scheme/--host/--port` | 组装 `servers.url` |
| `--out <file>` | 输出路径（默认 `<project>/build/apifox/openapi.json`） |
| `--prepend-context-path` | 把 context-path 拼进每个接口 path（默认放进 servers） |
| `--include-tag <tag>` | 只导出指定分组（可重复） |
| `--apifox-token` | 或环境变量 `APIFOX_TOKEN` |
| `--apifox-project` | 或环境变量 `APIFOX_PROJECT_ID` |
| `--apifox-mode` | `OVERWRITE_EXISTING`/`AUTO_MERGE`/`KEEP_EXISTING`/`CREATE_NEW`（默认 `AUTO_MERGE`） |
| `--apifox-folder` | 目标接口目录 ID |
| `--apifox-branch` | 目标分支 ID |
| `--delete-unmatched` | 同步删除源里不存在的接口/模型 |
| `--dry-run` | 只生成文件，不推送 |

## 项目结构

``` json
src/
  main.rs          入口：编排 扫描 -> 生成 -> 写文件 -> 推送
  cli.rs           clap 命令行参数
  config.rs        读取 application*.yaml 的 context-path / port
  model.rs         Endpoint / Param / Body 模型
  java/
    project.rs     解析所有 .java，建立 类型名 -> 源文件 索引
    ast.rs         tree-sitter 节点读取工具
    controllers.rs Controller -> Endpoint
  schema.rs        Java 类型 -> OpenAPI Schema（含 components 与防环）
  openapi.rs       Endpoint -> OpenAPI 3.0.3 文档
  apifox.rs        调用 Apifox「导入 OpenAPI」接口
```

## 已知边界（可选增强）

- `ResponseEntity<Object>` 这类被擦除的返回类型，若没有 `@ApiResponse(response=...)`，无法得知响应结构。
- 公共请求头（token/`company_id`/`Accept-Language`）目前不会自动作为全局 Header 注入，需在 Apifox 项目级配置。
- 泛型包装类（如 `DataResponse<T>`）以内联展开而非独立模型。
- 暂未识别的写法：`@RequestMapping` 不带 method（默认按 GET 处理）、自定义组合注解（`@GetMapping` 的元注解封装）、Spring 函数式路由。
