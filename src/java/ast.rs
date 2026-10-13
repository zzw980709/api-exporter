//! tree-sitter 节点的读取小工具。
//!
//! tree-sitter 的 `Node` 是廉价的树节点句柄（实现了 `Copy`），这里把它包装成
//! “像读 AST 一样”的辅助函数，业务代码就不必关心 node kind 的细节了。

use tree_sitter::Node;

pub fn text<'s>(node: Node<'_>, src: &'s str) -> &'s str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

/// 第一个名为 `kind` 的直接子节点。
pub fn child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// 深度优先收集所有后代节点（不含自身）。
pub fn descendants<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut stack: Vec<Node<'t>> = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        stack.push(child);
    }
    while let Some(n) = stack.pop() {
        out.push(n);
        let mut cursor = n.walk();
        let children: Vec<Node<'t>> = n.named_children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    out
}

/// 节点上的注解（`@RequestMapping(...)` / `@GetMapping`）。
pub fn annotations(node: Node<'_>) -> Vec<Node<'_>> {
    let Some(modifiers) = child_of_kind(node, "modifiers") else {
        return Vec::new();
    };
    let mut cursor = modifiers.walk();
    modifiers
        .named_children(&mut cursor)
        .filter(|c| matches!(c.kind(), "annotation" | "marker_annotation"))
        .collect()
}

pub fn annotation_name<'s>(annotation: Node<'_>, src: &'s str) -> &'s str {
    annotation
        .child_by_field_name("name")
        .map(|n| text(n, src))
        .unwrap_or("")
}

/// 找到节点上名为 `name` 的注解（只看直接注解）。
pub fn find_annotation<'t>(node: Node<'t>, name: &str, src: &str) -> Option<Node<'t>> {
    annotations(node)
        .into_iter()
        .find(|a| annotation_name(*a, src) == name)
}

pub fn has_annotation(node: Node<'_>, name: &str, src: &str) -> bool {
    find_annotation(node, name, src).is_some()
}

/// 在所有后代里找名为 `name` 的注解（例如 `@ApiImplicitParams` 内嵌的注解）。
pub fn find_annotation_deep<'t>(node: Node<'t>, name: &str, src: &str) -> Vec<Node<'t>> {
    descendants(node)
        .into_iter()
        .filter(|n| matches!(n.kind(), "annotation" | "marker_annotation"))
        .filter(|n| annotation_name(*n, src) == name)
        .collect()
}

fn arguments<'t>(annotation: Node<'t>) -> Option<Node<'t>> {
    annotation.child_by_field_name("arguments")
}

/// 取注解参数对应的“值节点”。
///
/// 支持三种写法：
/// * `@Api(tags = "x")`   -> `element_value_pair`
/// * `@Api(tags = {"x"})` -> `element_value_pair`，值是数组初始化器
/// * `@GetMapping("/x")`  -> 无 key 的单值
pub fn arg_value_node<'t>(annotation: Node<'t>, key: &str, src: &str) -> Option<Node<'t>> {
    let args = arguments(annotation)?;
    let mut cursor = args.walk();
    let named: Vec<Node<'t>> = args.named_children(&mut cursor).collect();
    let mut has_pair = false;
    for child in &named {
        if child.kind() == "element_value_pair" {
            has_pair = true;
            if let Some(k) = child.child_by_field_name("key") {
                if text(k, src) == key {
                    return child.child_by_field_name("value");
                }
            }
        }
    }
    if !has_pair && key == "value" {
        return named.into_iter().next();
    }
    None
}

fn string_literal_value(literal: Node<'_>, src: &str) -> String {
    descendants(literal)
        .into_iter()
        .filter(|n| n.kind() == "string_fragment")
        .map(|n| text(n, src))
        .collect::<String>()
}

/// 从任意值节点里取第一个字符串字面量。
pub fn first_string(node: Node<'_>, src: &str) -> Option<String> {
    if node.kind() == "string_literal" {
        return Some(string_literal_value(node, src));
    }
    descendants(node)
        .into_iter()
        .find(|n| n.kind() == "string_literal")
        .map(|n| string_literal_value(n, src))
}

/// 从任意值节点里取所有字符串字面量（数组初始化器也能处理）。
pub fn all_strings(node: Node<'_>, src: &str) -> Vec<String> {
    if node.kind() == "string_literal" {
        return vec![string_literal_value(node, src)];
    }
    descendants(node)
        .into_iter()
        .filter(|n| n.kind() == "string_literal")
        .map(|n| string_literal_value(n, src))
        .collect()
}

pub fn arg_string(annotation: Node<'_>, key: &str, src: &str) -> Option<String> {
    arg_value_node(annotation, key, src).and_then(|v| first_string(v, src))
}

pub fn arg_strings(annotation: Node<'_>, key: &str, src: &str) -> Vec<String> {
    match arg_value_node(annotation, key, src) {
        Some(v) => all_strings(v, src),
        None => Vec::new(),
    }
}

pub fn arg_bool(annotation: Node<'_>, key: &str, src: &str) -> Option<bool> {
    let value = arg_value_node(annotation, key, src)?;
    match value.kind() {
        "true" => Some(true),
        "false" => Some(false),
        _ => {
            let t = text(value, src);
            match t {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }
        }
    }
}

#[allow(dead_code)]
pub fn arg_int(annotation: Node<'_>, key: &str, src: &str) -> Option<i64> {
    let value = arg_value_node(annotation, key, src)?;
    text(value, src).parse().ok()
}

/// 修饰符里是否包含某个关键字/注解（如 `static` / `transient`）。
pub fn has_modifier(node: Node<'_>, keyword: &str, src: &str) -> bool {
    let Some(modifiers) = child_of_kind(node, "modifiers") else {
        return false;
    };
    let mut cursor = modifiers.walk();
    let found = modifiers
        .named_children(&mut cursor)
        .any(|c| text(c, src) == keyword);
    found
}
