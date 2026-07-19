use std::fs;
use std::io::Read;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::rules::IndexRules;

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
///
/// 读进程级当前生效规则：追加扩展名（`extra_text_exts`）里的类型也算可抽取。
pub(crate) fn is_extractable(path: &Path) -> bool {
    is_extractable_with(path, &crate::rules::active_rules())
}

/// [`is_extractable`] 的纯函数版：判定逻辑接收显式规则，不碰进程级全局，便于
/// 单测。零参的 `is_extractable` 只是用当前生效规则调它。
pub(crate) fn is_extractable_with(path: &Path, rules: &IndexRules) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    ext == "pdf"
        || TEXT_EXTS.contains(&ext.as_str())
        || OFFICE_EXTS.contains(&ext.as_str())
        || rules.is_extra_text_ext(&ext)
}

/// 从文件里抽出可索引的纯文本。
/// 返回 Option 而不是 Result：抽不出来（不支持的格式/太大/损坏）不算错误，
/// 调用方只需要知道"这个文件没有文本"，跳过即可。
///
/// 体积上限和追加文本扩展名取自进程级当前生效规则（见 `rules` 模块）。
pub fn extract_text(path: &Path) -> Option<String> {
    extract_text_with(path, &crate::rules::active_rules())
}

/// [`extract_text`] 的纯函数版：体积上限、追加扩展名都从显式规则读，不碰进程级
/// 全局，便于单测。零参的 `extract_text` 只是用当前生效规则调它。
fn extract_text_with(path: &Path, rules: &IndexRules) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();

    let meta = fs::metadata(path).ok()?;
    if meta.len() > rules.max_file_bytes() {
        return None;
    }

    // 各格式的抽取都可能在底层库里 panic：PDF 走的 `pdf_extract`（及其底层
    // `lopdf`）对某些畸形/深层嵌套的 PDF 不是返回 `Err`，而是内部 `unwrap`/`panic!`；
    // docx/xlsx/pptx 共用的 zip + XML 解析同样可能对畸形输入 panic。而抽取是在
    // 监听/OCR 线程持有共享 `IndexUpdater` 锁时调的：panic 一旦穿过持有的
    // `MutexGuard`，就会把这把共享锁**毒化**，之后所有 `.lock()` 都 panic，连锁
    // 拖垮整条监听 + OCR 管线（对常驻托盘程序是灾难性的）。
    //
    // 这里用 `catch_unwind` 把**任意**格式抽取过程中的 panic 兜成 `None`，让畸形
    // 文件跟其它抽不出文本的文件一样安静跳过。（`updater.rs`/`watch.rs`/
    // `ocr_worker.rs` 那侧的锁也已改成抗毒化的 `unwrap_or_else(|e| e.into_inner())`，
    // 两层防御各自都能兜住。）
    //
    // 注意 `catch_unwind` 只能接住**栈展开式**的 panic（`unwrap`/`panic!` 这类）。
    // 若畸形输入触发深度递归把线程栈撑爆，那是栈溢出，走的是 SIGABRT/进程 abort，
    // `catch_unwind` 接不住，进程仍会整体退出——这一类只能靠上游解析库自身的
    // 递归深度限制来防，不在本函数的兜底范围内。
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match ext.as_str() {
        "pdf" => extract_pdf(path),
        "docx" => extract_docx(path),
        "xlsx" => extract_xlsx(path),
        "pptx" => extract_pptx(path),
        e if TEXT_EXTS.contains(&e) => read_text_smart(path),
        // 追加白名单里的扩展名当纯文本读（自动探测编码），跟内建文本类型同路。
        e if rules.is_extra_text_ext(e) => read_text_smart(path),
        _ => None,
    }))
    .ok()
    .flatten()
}

/// 从 PDF 抽文本。畸形 PDF 触发的 panic 由 `extract_text` 那层统一的
/// `catch_unwind` 兜底（见其文档），这里只走正常路径：`pdf_extract` 返回 `Err`
/// 时（可正常识别的错误）用 `.ok()` 落成 `None`。
fn extract_pdf(path: &Path) -> Option<String> {
    pdf_extract::extract_text(path).ok()
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

    // --- 编码探测（read_text_smart）---

    #[test]
    fn gbk_encoded_chinese_txt_is_decoded_to_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gbk_sample.txt");
        // 一段足够长的中文，探测器才有把握判成 GBK 而不是别的单字节编码。
        let content = "这是一段用于编码探测的中文测试文本，\
包含足够多的汉字以便字符集识别器把它判定为国标编码而不是别的编码，\
季度对账哨兵词也在这里出现。";
        let (gbk_bytes, _, unmappable) = encoding_rs::GBK.encode(content);
        assert!(!unmappable, "测试文本应能完整编码成 GBK");
        fs::write(&path, &gbk_bytes).unwrap();

        let text = read_text_smart(&path).expect("GBK 文件应能解码出文本");
        assert!(text.contains("季度对账哨兵词"), "应还原出原始中文: {text}");
        assert!(text.contains("国标编码"));
    }

    #[test]
    fn utf8_bom_file_is_decoded_without_leading_bom_char() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bom_sample.txt");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("带 BOM 的内容 marker-bom".as_bytes());
        fs::write(&path, &bytes).unwrap();

        let text = read_text_smart(&path).expect("带 BOM 的 UTF-8 文件应能解码");
        assert!(text.contains("marker-bom"));
        assert!(text.contains("带 BOM 的内容"));
        assert!(!text.starts_with('\u{FEFF}'), "BOM 应被剥掉，不进正文");
    }

    #[test]
    fn empty_text_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty_sample.txt");
        fs::write(&path, b"").unwrap();
        assert!(read_text_smart(&path).is_none());
    }

    #[test]
    fn file_over_size_limit_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversized_sample.txt");
        let rules = IndexRules::default();
        // 用 set_len 造一个刚超过上限的稀疏文件，只撑大小、不真写 20MB 数据。
        let f = fs::File::create(&path).unwrap();
        f.set_len(rules.max_file_bytes() + 1).unwrap();
        drop(f);
        assert!(
            extract_text_with(&path, &rules).is_none(),
            "超过体积上限的文件应被跳过"
        );
    }

    #[test]
    fn custom_lower_size_limit_skips_file_default_would_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mid.txt");
        // 造一个 2MB 的文件：默认 20MB 上限下能抽取，但把上限调到 1MB 就该跳过。
        let f = fs::File::create(&path).unwrap();
        f.set_len(2 * 1024 * 1024).unwrap();
        drop(f);

        let default_rules = IndexRules::default();
        assert!(
            extract_text_with(&path, &default_rules).is_some(),
            "默认 20MB 上限下 2MB 文件应能抽取"
        );

        let tight = IndexRules {
            max_file_mb: 1,
            ..IndexRules::default()
        };
        assert!(
            extract_text_with(&path, &tight).is_none(),
            "上限收紧到 1MB 后 2MB 文件应被跳过"
        );
    }

    #[test]
    fn extra_text_ext_becomes_extractable_and_reads_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.rst");
        fs::write(&path, "追加扩展名内容 extra-ext-marker").unwrap();

        let default_rules = IndexRules::default();
        assert!(
            !is_extractable_with(&path, &default_rules),
            "默认规则下 .rst 不是可抽取类型"
        );
        assert!(
            extract_text_with(&path, &default_rules).is_none(),
            "默认规则下 .rst 抽不出文本"
        );

        let with_rst = IndexRules {
            extra_text_exts: vec!["rst".into()],
            ..IndexRules::default()
        };
        assert!(
            is_extractable_with(&path, &with_rst),
            "追加 rst 后 .rst 应算可抽取"
        );
        let text = extract_text_with(&path, &with_rst).expect("追加 rst 后应能抽出文本");
        assert!(text.contains("extra-ext-marker"));
    }

    #[test]
    fn misdetected_encoding_falls_back_to_lossy_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("misdetect_sample.txt");
        // 先放一大段合法 GBK 中文，让探测器有把握判成 GBK；结尾附一个非法的 GBK
        // 多字节序列（0x81 是前导字节，0x20 不是合法后继），逼 encoding_rs 解码时
        // had_errors=true，从而走 read_text_smart 里"猜错后有损回退"那条分支。
        let (gbk, _, _) = encoding_rs::GBK
            .encode("编码探测有损回退分支覆盖用例需要一段足够长的国标中文来稳定命中 GBK 判定");
        let mut buf = b"FALLBACK_MARKER_".to_vec();
        buf.extend_from_slice(&gbk);
        buf.extend_from_slice(&[0x81, 0x20]);
        fs::write(&path, &buf).unwrap();

        let text = read_text_smart(&path).expect("回退分支也要返回 Some");
        // 有损回退把整段字节当 UTF-8 读：ASCII 前缀存活，GBK 中文变成替换符，
        // 因此不会出现正确解出来的中文——以此证明走的是回退而非正常 GBK 解码。
        assert!(text.contains("FALLBACK_MARKER_"));
        assert!(
            !text.contains("国标中文"),
            "若走了正常 GBK 解码就会出现真中文，说明没命中回退分支: {text}"
        );
    }

    // --- 畸形 Office 文件 ---

    #[test]
    fn docx_missing_document_entry_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_document.docx");
        // 合法 zip，但缺少 word/document.xml 这个正文条目。
        write_zip(&path, &[("docProps/core.xml", "<coreProperties/>")]);
        assert!(extract_text(&path).is_none());
    }

    #[test]
    fn docx_with_truncated_xml_keeps_text_before_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated.docx");
        // 正文先给一段合法内容，再接一个对不上的结束标签，触发 XML 解析中途出错
        // （extract_tagged_text 里的 Err(_) => break 兜底分支）。
        let document_xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>partial-before-corruption</w:t></w:r></w:p></w:bogusEnd>"#;
        write_zip(&path, &[("word/document.xml", document_xml)]);

        let text = extract_text(&path).expect("损坏前已抽到的文本应保留");
        assert!(text.contains("partial-before-corruption"));
    }

    #[test]
    fn whitespace_only_docx_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blank.docx");
        let document_xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>    </w:t></w:r></w:p></w:body></w:document>"#;
        write_zip(&path, &[("word/document.xml", document_xml)]);
        assert!(extract_text(&path).is_none());
    }

    #[test]
    fn xlsx_skips_unreadable_sheet_and_processes_the_rest() {
        use zip::CompressionMethod;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial_sheets.xlsx");

        // sheet1 用 Stored（不压缩）写入，稍后直接改坏它的明文字节，读取时 CRC
        // 校验失败 → read_to_end 返回 Err → 命中 extract_xlsx 里的 continue 分支。
        // sheet2 正常，验证坏掉一个 sheet 不影响其它 sheet 的抽取。
        {
            let file = fs::File::create(&path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zip.start_file("xl/worksheets/sheet1.xml", stored).unwrap();
            zip.write_all(
                br#"<worksheet><sheetData><row><c t="inlineStr"><is><t>CORRUPTME_SHEET1_UNIQUE</t></is></c></row></sheetData></worksheet>"#,
            )
            .unwrap();
            zip.start_file("xl/worksheets/sheet2.xml", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(
                br#"<worksheet><sheetData><row><c t="inlineStr"><is><t>survivor-sheet2-text</t></is></c></row></sheetData></worksheet>"#,
            )
            .unwrap();
            zip.finish().unwrap();
        }

        // 把 sheet1 的 Stored 明文里某个字节改掉，让它和存档记录的 CRC 对不上。
        let mut raw = fs::read(&path).unwrap();
        let needle = b"CORRUPTME_SHEET1_UNIQUE";
        let pos = raw
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("应能在 zip 明文里找到 sheet1 的标记");
        raw[pos] ^= 0xFF;
        fs::write(&path, &raw).unwrap();

        let text = extract_text(&path).expect("坏掉一个 sheet 后其它 sheet 仍应产出文本");
        assert!(
            text.contains("survivor-sheet2-text"),
            "健康 sheet 的文本应被抽出"
        );
        assert!(
            !text.contains("CORRUPTME_SHEET1_UNIQUE"),
            "坏掉的 sheet 内容不应出现在结果里"
        );
    }
}
