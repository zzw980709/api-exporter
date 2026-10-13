//! 解析整个源码目录，并建立“类型名 -> 源文件”的索引。
//!
//! 关键点：tree-sitter 的 `Node` 生命周期绑定在 `Tree` 上，所以我们不缓存节点，
//! 只缓存“哪个文件、什么类型名”，需要时再从 `Tree` 里按名字把节点找回来。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser, Tree};
use walkdir::WalkDir;

use super::ast;

const TYPE_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "annotation_type_declaration",
];

pub struct Source {
    #[allow(dead_code)] // 保留路径用于调试/日志
    pub path: PathBuf,
    pub code: String,
    pub tree: Tree,
    pub package: String,
    /// 显式 import 的完整类名，例如 `com.demo.Foo`。
    pub imports: Vec<String>,
    /// 通配 import 的包名，例如 `com.demo.*`。
    pub wildcard_imports: Vec<String>,
}

pub struct Project {
    pub sources: Vec<Source>,
    pub by_fqn: HashMap<String, usize>,
    pub by_simple: HashMap<String, Vec<usize>>,
    pub parse_errors: usize,
}

impl Project {
    pub fn scan(source_root: &Path) -> anyhow::Result<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .expect("加载 tree-sitter-java 语法失败");

        let mut sources = Vec::new();
        let mut parse_errors = 0usize;
        let mut builder = Self {
            sources: Vec::new(),
            by_fqn: HashMap::new(),
            by_simple: HashMap::new(),
            parse_errors: 0,
        };

        let mut files: Vec<PathBuf> = WalkDir::new(source_root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| p.extension().map(|e| e == "java").unwrap_or(false))
            .collect();
        files.sort();

        for path in files {
            let Ok(code) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(tree) = parser.parse(&code, None) else {
                parse_errors += 1;
                continue;
            };
            if tree.root_node().has_error() {
                parse_errors += 1;
            }
            let package = package_of(tree.root_node(), &code);
            let (imports, wildcard_imports) = imports_of(tree.root_node(), &code);
            let idx = sources.len();
            let source = Source {
                path,
                code,
                tree,
                package,
                imports,
                wildcard_imports,
            };
            builder.index_types(idx, &source);
            sources.push(source);
        }

        builder.sources = sources;
        builder.parse_errors = parse_errors;
        Ok(builder)
    }

    fn index_types(&mut self, idx: usize, src: &Source) {
        let root = src.tree.root_node();
        let mut entries: Vec<(String, String)> = Vec::new();
        for decl in top_level_types(root) {
            let name = type_name(decl, &src.code).to_string();
            if name.is_empty() {
                continue;
            }
            let fqn = if src.package.is_empty() {
                name.clone()
            } else {
                format!("{}.{}", src.package, name)
            };
            entries.push((fqn, name));
        }
        // 嵌套类型：只登记简单名（去掉 root 的直接子节点，避免与上面重复）
        for node in ast::descendants(root) {
            if !TYPE_KINDS.contains(&node.kind()) {
                continue;
            }
            if node.parent().map(|p| p.kind() == "program").unwrap_or(false) {
                continue;
            }
            let name = type_name(node, &src.code).to_string();
            if !name.is_empty() {
                entries.push((String::new(), name));
            }
        }
        for (fqn, simple) in entries {
            if !fqn.is_empty() {
                self.by_fqn.insert(fqn, idx);
            }
            self.by_simple.entry(simple).or_default().push(idx);
        }
    }

    /// 根据引用处的源码位置解析一个类型名，返回它所在的源文件下标。
    pub fn resolve(&self, name: &str, from: usize) -> Option<usize> {
        if let Some(&i) = self.by_fqn.get(name) {
            return Some(i);
        }
        let src = self.sources.get(from)?;
        for import in &src.imports {
            if import.ends_with(&format!(".{name}")) {
                if let Some(&i) = self.by_fqn.get(import) {
                    return Some(i);
                }
            }
        }
        let same_pkg = if src.package.is_empty() {
            name.to_string()
        } else {
            format!("{}.{name}", src.package)
        };
        if let Some(&i) = self.by_fqn.get(&same_pkg) {
            return Some(i);
        }
        for wildcard in &src.wildcard_imports {
            if let Some(&i) = self.by_fqn.get(&format!("{wildcard}.{name}")) {
                return Some(i);
            }
        }
        if let Some(candidates) = self.by_simple.get(name) {
            if let Some(&i) = candidates.iter().find(|&&i| self.sources[i].package == src.package) {
                return Some(i);
            }
            return candidates.first().copied();
        }
        None
    }

    /// 在指定源文件里按简单名找回类型声明节点。
    pub fn find_type_decl(&self, source_idx: usize, simple: &str) -> Option<Node<'_>> {
        let src = self.sources.get(source_idx)?;
        let root = src.tree.root_node();
        for decl in top_level_types(root) {
            if type_name(decl, &src.code) == simple {
                return Some(decl);
            }
        }
        ast::descendants(root)
            .into_iter()
            .find(|n| TYPE_KINDS.contains(&n.kind()) && type_name(*n, &src.code) == simple)
    }

    pub fn code(&self, source_idx: usize) -> &str {
        &self.sources[source_idx].code
    }
}

pub fn top_level_types(root: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|c| TYPE_KINDS.contains(&c.kind()))
        .collect()
}

pub fn type_name<'s>(decl: Node<'_>, src: &'s str) -> &'s str {
    decl.child_by_field_name("name")
        .map(|n| ast::text(n, src))
        .unwrap_or("")
}

fn package_of(root: Node<'_>, src: &str) -> String {
    ast::child_of_kind(root, "package_declaration")
        .and_then(|p| ast::child_of_kind(p, "scoped_identifier"))
        .map(|n| ast::text(n, src).to_string())
        .unwrap_or_default()
}

fn imports_of(root: Node<'_>, src: &str) -> (Vec<String>, Vec<String>) {
    let mut explicit = Vec::new();
    let mut wildcard = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() != "import_declaration" {
            continue;
        }
        let Some(name) = ast::child_of_kind(child, "scoped_identifier") else {
            continue;
        };
        let has_asterisk = {
            let mut c = child.walk();
            let found = child.named_children(&mut c).any(|n| n.kind() == "asterisk");
            found
        };
        let value = ast::text(name, src).to_string();
        if has_asterisk {
            wildcard.push(value);
        } else {
            explicit.push(value);
        }
    }
    (explicit, wildcard)
}
