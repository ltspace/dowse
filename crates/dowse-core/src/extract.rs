use std::fs;
use std::io::Read;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

/// 单文件体积上限：超过就跳过，防止索引一个 2GB 日志把内存吃穿。
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

/// 按扩展名白名单认定的纯文本文件。
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "markdown", "log", "csv", "tsv", "json", "toml", "yaml", "yml", "ini", "cfg",
    "conf", "xml", "html", "htm", "sql", "sh", "ps1", "bat", "rs", "py", "go", "java", "js", "ts",
    "jsx", "tsx", "c", "h", "cpp", "hpp", "cs", "rb", "php", "lua", "vue",
];

/// 支持抽取的 Office Open XML 格式（docx/xlsx/pptx，本质都是 zip 包）。
/// 老式二进制格式 doc/xls/ppt 不在白名单里，天然被跳过——不做兼容。
const OFFICE_EXTS: &[&str] = &["docx", "xlsx", "pptx"];

/// 这个文件是否属于能抽取文本的类型（只看扩展名，不读文件，很便宜）。
/// 启动对账用它过滤：非文本类型的文件从来不会进索引，没必要参与三态比对——
/// 否则每次对账都把它们当"新增"白跑一趟、还把对账统计数字撑虚。
pub(crate) fn is_extractable(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    ext == "pdf" || TEXT_EXTS.contains(&ext.as_str()) || OFFICE_EXTS.contains(&ext.as_str())
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
        "pdf" => extract_pdf(path),
        "docx" => extract_docx(path),
        "xlsx" => extract_xlsx(path),
        "pptx" => extract_pptx(path),
        e if TEXT_EXTS.contains(&e) => read_text_smart(path),
        _ => None,
    }
}

/// 从 PDF 抽文本。`pdf_extract`（及其底层 `lopdf`）对某些畸形或深层嵌套的 PDF
/// 不是返回 `Err`，而是内部 `unwrap`/`panic!`（甚至栈溢出）——`.ok()` 接不住 panic。
/// 而抽取是在监听/OCR 线程持有共享 `IndexUpdater` 锁时调的：panic 一旦穿过持有的
/// `MutexGuard`，就会把这把共享锁**毒化**，之后所有 `.lock()` 都 panic，连锁拖垮
/// 整条监听 + OCR 管线（对常驻托盘程序是灾难性的）。
///
/// 这里用 `catch_unwind` 把 PDF 抽取的 panic 兜成 `None`，让畸形 PDF 跟其它抽不出
/// 文本的文件一样安静跳过。（`updater.rs`/`watch.rs`/`ocr_worker.rs` 那侧的锁也已
/// 改成抗毒化的 `unwrap_or_else(|e| e.into_inner())`，两层防御各自都能兜住。）
fn extract_pdf(path: &Path) -> Option<String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text(path).ok()
    }))
    .ok()
    .flatten()
}

/// 打开一个 zip 包里的单个条目，读成字节。条目不存在/包本身打不开（损坏、
/// 加密）都走 None——密码保护的 Office 文件会在这里打不开 zip 结构，
/// 自然跳过，不会 panic。
fn read_zip_entry(path: &Path, entry_name: &str) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut entry = archive.by_name(entry_name).ok()?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// docx 是一个 zip 包，正文全部在 word/document.xml 里，文本节点是 `<w:t>`。
/// 段落 `<w:p>` 闭合时补一个换行，避免不同段落的文字连成一整块无法阅读。
fn extract_docx(path: &Path) -> Option<String> {
    let xml = read_zip_entry(path, "word/document.xml")?;
    let text = extract_tagged_text(&xml, b"t", Some(b"p"));
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// xlsx 的单元格文本绝大多数不直接存在 sheet 里，而是去重后存进
/// xl/sharedStrings.xml，sheet 里只留索引号；覆盖不了共享字符串表的是
/// 内联字符串（`<c t="inlineStr"><is><t>...</t></is></c>`），直接写在各
/// xl/worksheets/sheet*.xml 里，两处都要读。两处的文本节点本地名都是 `<t>`。
fn extract_xlsx(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;

    let mut text = String::new();

    if let Ok(mut entry) = archive.by_name("xl/sharedStrings.xml") {
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_ok() {
            drop(entry);
            text.push_str(&extract_tagged_text(&bytes, b"t", None));
            text.push('\n');
        }
    }

    let sheet_names: Vec<String> = archive
        .file_names()
        .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
        .map(|n| n.to_string())
        .collect();
    for name in sheet_names {
        let Ok(mut entry) = archive.by_name(&name) else {
            continue;
        };
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_err() {
            continue;
        }
        drop(entry);
        text.push_str(&extract_tagged_text(&bytes, b"t", None));
        text.push('\n');
    }

    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// pptx 每页幻灯片是独立的 ppt/slides/slideN.xml，文本 run 是 `<a:t>`，
/// 本地名同样是 `t`。幻灯片之间补空行分隔。
fn extract_pptx(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;

    let mut slide_names: Vec<String> = archive
        .file_names()
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .map(|n| n.to_string())
        .collect();
    slide_names.sort();

    let mut text = String::new();
    for name in slide_names {
        let Ok(mut entry) = archive.by_name(&name) else {
            continue;
        };
        let mut bytes = Vec::new();
        if entry.read_to_end(&mut bytes).is_err() {
            continue;
        }
        drop(entry);
        text.push_str(&extract_tagged_text(&bytes, b"t", Some(b"p")));
        text.push('\n');
    }

    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// 流式扫一遍 XML 事件，抠出本地标签名匹配 text_tag 的节点里的文本
/// （按本地名匹配、忽略命名空间前缀，`w:t`/`a:t`/无前缀的 `t` 都能命中）。
/// 不建 DOM 树：Office XML 展开后可能几十万字符，没必要整棵放内存。
/// para_tag 给定时，每闭合一个该标签就补换行，用来标记段落/幻灯片内的换行边界。
/// XML 本身损坏时中途跳出循环，返回已经攒到的文本——调用方那层再统一按
/// "抽不出内容就是 None" 处理，不在这里 panic 或者往外抛错误。
fn extract_tagged_text(xml: &[u8], text_tag: &[u8], para_tag: Option<&[u8]>) -> String {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut out = String::new();
    let mut in_text = false;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                if e.local_name().as_ref() == text_tag {
                    in_text = true;
                }
            }
            Ok(Event::Text(t)) => {
                if in_text
                    && let Ok(decoded) = t.decode()
                    && let Ok(unescaped) = quick_xml::escape::unescape(&decoded)
                {
                    out.push_str(&unescaped);
                }
            }
            Ok(Event::End(e)) => {
                let name = e.local_name();
                if name.as_ref() == text_tag {
                    in_text = false;
                } else if para_tag.is_some_and(|p| name.as_ref() == p) {
                    out.push('\n');
                }
            }
            Ok(Event::Empty(e)) => {
                if para_tag.is_some_and(|p| e.local_name().as_ref() == p) {
                    out.push('\n');
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }

    out
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

#[cfg(test)]
mod tests {
    use std::io::Write;

    use zip::write::SimpleFileOptions;

    use super::*;

    /// 程序化拼一个最小合法 zip 包，条目名到内容的映射即够——
    /// Office 文件本质就是 zip，抽取只关心我们读的那几个条目存在且是合法 XML。
    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (name, content) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn extracts_docx_paragraph_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.docx");
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>季度对账哨兵词</w:t></w:r></w:p>
    <w:p><w:r><w:t>quarterly-sentinel</w:t></w:r></w:p>
  </w:body>
</w:document>"#;
        write_zip(&path, &[("word/document.xml", document_xml)]);

        let text = extract_text(&path).expect("docx 应能抽出文本");
        assert!(text.contains("季度对账哨兵词"));
        assert!(text.contains("quarterly-sentinel"));
    }

    #[test]
    fn extracts_xlsx_shared_and_inline_strings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.xlsx");
        let shared_strings = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">
  <si><t>季度对账哨兵词</t></si>
</sst>"#;
        let sheet1 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="s"><v>0</v></c>
      <c r="B1" t="inlineStr"><is><t>quarterly-sentinel</t></is></c>
    </row>
  </sheetData>
</worksheet>"#;
        write_zip(
            &path,
            &[
                ("xl/sharedStrings.xml", shared_strings),
                ("xl/worksheets/sheet1.xml", sheet1),
            ],
        );

        let text = extract_text(&path).expect("xlsx 应能抽出文本");
        assert!(text.contains("季度对账哨兵词"));
        assert!(text.contains("quarterly-sentinel"));
    }

    #[test]
    fn extracts_pptx_slide_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.pptx");
        let slide1 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp><p:txBody><a:p><a:r><a:t>季度对账哨兵词</a:t></a:r></a:p></p:txBody></p:sp>
      <p:sp><p:txBody><a:p><a:r><a:t>quarterly-sentinel</a:t></a:r></a:p></p:txBody></p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;
        write_zip(&path, &[("ppt/slides/slide1.xml", slide1)]);

        let text = extract_text(&path).expect("pptx 应能抽出文本");
        assert!(text.contains("季度对账哨兵词"));
        assert!(text.contains("quarterly-sentinel"));
    }

    #[test]
    fn corrupted_docx_is_skipped_without_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.docx");
        fs::write(&path, b"this is not a zip file at all").unwrap();

        assert!(extract_text(&path).is_none());
    }
}
