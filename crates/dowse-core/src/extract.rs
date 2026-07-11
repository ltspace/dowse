use std::fs;
use std::path::Path;

/// 单文件体积上限：超过就跳过，防止索引一个 2GB 日志把内存吃穿。
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

/// 按扩展名白名单认定的纯文本文件。
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "markdown", "log", "csv", "tsv", "json", "toml", "yaml", "yml", "ini", "cfg",
    "conf", "xml", "html", "htm", "sql", "sh", "ps1", "bat", "rs", "py", "go", "java", "js", "ts",
    "jsx", "tsx", "c", "h", "cpp", "hpp", "cs", "rb", "php", "lua", "vue",
];

/// 这个文件是否属于能抽取文本的类型（只看扩展名，不读文件，很便宜）。
/// 启动对账用它过滤：非文本类型的文件从来不会进索引，没必要参与三态比对——
/// 否则每次对账都把它们当"新增"白跑一趟、还把对账统计数字撑虚。
pub(crate) fn is_extractable(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    ext == "pdf" || TEXT_EXTS.contains(&ext.as_str())
}

/// 从文件里抽出可索引的纯文本。
/// 返回 Option 而不是 Result：抽不出来（不支持的格式/太大/损坏）不算错误，
/// 调用方只需要知道"这个文件没有文本"，跳过即可。
pub fn extract_text(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();

    let meta = fs::metadata(path).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }

    match ext.as_str() {
        "pdf" => pdf_extract::extract_text(path).ok(),
        e if TEXT_EXTS.contains(&e) => read_text_smart(path),
        _ => None,
    }
}

/// 读文本文件，自动探测编码。
/// Windows 上的中文 txt 很多是 GBK 而不是 UTF-8，直接按 UTF-8 读会变乱码，
/// 所以先用 chardetng 猜编码，再用 encoding_rs 解码成 UTF-8。
fn read_text_smart(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }

    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    detector.feed(&bytes, true);
    let encoding = detector.guess(None, chardetng::Utf8Detection::Allow);

    let (text, _, had_errors) = encoding.decode(&bytes);
    if had_errors && encoding != encoding_rs::UTF_8 {
        // 猜错编码时宁可退回 UTF-8 有损解码，也不索引一堆乱码词条
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    Some(text.into_owned())
}
