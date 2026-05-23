//! Office 文档（docx/xlsx/pptx）抽取的集成测试：程序化构造最小合法文件，
//! 走 rebuild_index 全链路，断言埋入的哨兵词可搜到；损坏文件单独一个用例，
//! 断言被跳过而不是把整条索引流水线带崩。

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use dowse_core::{Searcher, rebuild_index};
use zip::write::SimpleFileOptions;

/// 建索引用的目标目录名不能带 "." 前缀——walk_index_files 会整棵跳过隐藏目录，
/// 而 tempfile 默认给临时目录起 ".tmpXXXX" 这种名字。
fn target_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("dowse-office-")
        .tempdir()
        .unwrap()
}

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

fn count_hits(index_dir: &Path, query: &str) -> usize {
    let searcher = Searcher::open(index_dir).unwrap();
    searcher.search(query, 50).unwrap().len()
}

#[test]
fn office_documents_are_indexed_and_searchable() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let target = target_dir();

    let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p><w:r><w:t>季度对账哨兵词 docx</w:t></w:r></w:p></w:body>
</w:document>"#;
    write_zip(
        &target.path().join("report.docx"),
        &[("word/document.xml", document_xml)],
    );

    let shared_strings = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">
  <si><t>季度对账哨兵词 xlsx</t></si>
</sst>"#;
    write_zip(
        &target.path().join("data.xlsx"),
        &[("xl/sharedStrings.xml", shared_strings)],
    );

    let slide1 = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>季度对账哨兵词 pptx</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld>
</p:sld>"#;
    write_zip(
        &target.path().join("slides.pptx"),
        &[("ppt/slides/slide1.xml", slide1)],
    );

    let stats = rebuild_index(index_dir.path(), target.path())?;
    assert_eq!(stats.indexed, 3, "三个 Office 文件都应被索引");

    assert_eq!(
        count_hits(index_dir.path(), "季度对账哨兵词"),
        3,
        "中文哨兵词应三种格式都命中"
    );
    assert_eq!(
        count_hits(index_dir.path(), "不存在的词零零零"),
        0,
        "不存在的词不应命中"
    );

    Ok(())
}

#[test]
fn corrupted_docx_is_skipped_not_indexed() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let target = target_dir();

    fs::write(target.path().join("broken.docx"), b"not a real zip file")?;
    fs::write(
        target.path().join("good.md"),
        "正常文件应该照常入索引 alpha",
    )?;

    let stats = rebuild_index(index_dir.path(), target.path())?;
    assert_eq!(
        stats.indexed, 1,
        "损坏的 docx 应被跳过，只有 good.md 入索引"
    );
    assert_eq!(stats.skipped, 1);

    assert_eq!(count_hits(index_dir.path(), "alpha"), 1);
    Ok(())
}
