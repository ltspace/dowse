//! 浮窗筛选下拉用的预设文件类型分组。
//!
//! 图片分组是为里程碑 4 OCR 预留的：当前文本抽取管线（见 extract.rs）不产出
//! 图片文档，选中这组时索引里天然没有匹配项，是正常的空结果，不是错误——
//! schema v3 已经把 kind 字段捎带加上，就是为这天做准备（见 lib.rs 顶部注释）。

/// 文档类：常见办公 / 标记文档格式。
pub const DOC: &[&str] = &["md", "txt", "pdf", "docx", "xlsx", "pptx", "html"];

/// 代码类：参照 extract.rs 的纯文本白名单，挑出偏"编程语言/配置脚本"的那部分
/// （排掉 txt/md/csv 这类通用文本，它们已经在文档分组里）。
pub const CODE: &[&str] = &[
    "rs", "py", "go", "js", "ts", "jsx", "tsx", "java", "c", "h", "cpp", "hpp", "cs", "rb", "php",
    "lua", "vue", "json", "yaml", "yml", "toml", "sh", "ps1", "bat", "sql",
];

/// 图片类：里程碑 4 OCR 接入后才会真正入索引，现在选中这组只是搜不到东西。
pub const IMAGE: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp"];

/// 按名字取预设分组的扩展名集合。`"all"`、`None`、未知名字都表示不筛选——
/// 浮窗前端传字符串过来，未知输入宽松地当"全部"处理，不报错。
///
/// # Examples
///
/// ```
/// use dowse::ext_group_by_name;
///
/// // "doc" 分组包含常见文档扩展名。
/// assert!(ext_group_by_name(Some("doc")).unwrap().contains(&"pdf"));
/// // "all"、未知名字、None 都表示不筛选。
/// assert_eq!(ext_group_by_name(Some("all")), None);
/// assert_eq!(ext_group_by_name(None), None);
/// ```
pub fn by_name(name: Option<&str>) -> Option<&'static [&'static str]> {
    match name {
        Some("doc") => Some(DOC),
        Some("code") => Some(CODE),
        Some("image") => Some(IMAGE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_maps_known_groups() {
        assert_eq!(by_name(Some("doc")), Some(DOC));
        assert_eq!(by_name(Some("code")), Some(CODE));
        assert_eq!(by_name(Some("image")), Some(IMAGE));
    }

    #[test]
    fn by_name_falls_back_to_none_for_all_or_unknown() {
        assert_eq!(by_name(Some("all")), None);
        assert_eq!(by_name(Some("bogus")), None);
        assert_eq!(by_name(None), None);
    }
}
