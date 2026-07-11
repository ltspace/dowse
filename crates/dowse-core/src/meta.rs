use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// schema 版本号：字段定义每次不兼容变更就 +1。里程碑 3 给 schema 加了
/// mtime/size 两个字段，从里程碑 1 的隐式 v1 升到 v2。打开索引时版本对不上
/// 就要求重建，不做静默迁移、不做自动升级——旧字段布局搜出来的结果不可靠，
/// 宁可让用户重建一次。
pub(crate) const SCHEMA_VERSION: u32 = 2;

/// 索引目录旁的元数据：schema 版本号 + 已注册的索引根目录列表。
/// 索引根列表是启动对账和托盘"重建索引"的依据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMeta {
    pub schema_version: u32,
    pub roots: Vec<PathBuf>,
}

/// meta.json 放在索引目录的**兄弟**位置（`<index_dir>-meta.json`），不是塞进
/// 索引目录里：tantivy 自己在索引目录里也维护一个 `meta.json`，同名会撞车。
fn meta_path(index_dir: &Path) -> PathBuf {
    let stem = index_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dowse-index");
    index_dir.with_file_name(format!("{stem}-meta.json"))
}

pub(crate) fn load_meta(index_dir: &Path) -> Result<IndexMeta> {
    let path = meta_path(index_dir);
    let bytes = std::fs::read(&path).with_context(|| {
        format!(
            "读不到索引元数据 {}——索引可能是旧版本或已损坏，请重建索引",
            path.display()
        )
    })?;
    serde_json::from_slice(&bytes).context("索引元数据解析失败，请重建索引")
}

pub(crate) fn save_meta(index_dir: &Path, meta: &IndexMeta) -> Result<()> {
    let path = meta_path(index_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(meta)?;
    std::fs::write(&path, bytes).context("写索引元数据失败")?;
    Ok(())
}

/// 打开索引前校验 schema 版本。不匹配就报明确错误、提示重建，不静默兼容。
/// 校验通过时把 meta 返回给调用方复用（省一次读盘）。
pub(crate) fn ensure_schema_version(index_dir: &Path) -> Result<IndexMeta> {
    let meta = load_meta(index_dir)?;
    if meta.schema_version != SCHEMA_VERSION {
        bail!(
            "索引 schema 版本是 {}，当前程序需要 {}——字段定义已升级，请重建索引\
             （托盘菜单或 CLI 的 `dowse index`）。",
            meta.schema_version,
            SCHEMA_VERSION
        );
    }
    Ok(meta)
}

/// 已注册的索引根目录列表，供启动对账和文件监听使用。
/// 顺带校验 schema 版本——版本不对时直接报错，不返回一份不可信的根列表。
pub fn registered_roots(index_dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(ensure_schema_version(index_dir)?.roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_writes_meta_with_current_version_and_root() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "内容")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        let meta = load_meta(index_dir.path())?;
        assert_eq!(meta.schema_version, SCHEMA_VERSION);
        assert_eq!(meta.roots, vec![target_dir.path().to_path_buf()]);

        // registered_roots 是对外读根列表的入口，应给出同样的结果
        assert_eq!(registered_roots(index_dir.path())?, meta.roots);
        Ok(())
    }

    #[test]
    fn open_index_with_mismatched_schema_version_errors() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "内容")?;
        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        // 手动把 meta.json 改成一个未来的版本号，模拟字段定义升级过、索引没重建
        save_meta(
            index_dir.path(),
            &IndexMeta {
                schema_version: SCHEMA_VERSION + 1,
                roots: vec![target_dir.path().to_path_buf()],
            },
        )?;

        let err = match crate::Searcher::open(index_dir.path()) {
            Ok(_) => panic!("版本不匹配时打开索引应当报错，而不是静默兼容"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("重建"),
            "错误信息应提示用户重建索引，实际: {err}"
        );
        Ok(())
    }

    #[test]
    fn open_index_without_meta_errors() -> Result<()> {
        // 里程碑 1 建的旧索引没有 meta.json：打开时应报错提示重建，而不是当好索引用。
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "内容")?;
        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        // 删掉 meta.json 模拟旧版本索引
        std::fs::remove_file(meta_path(index_dir.path()))?;

        assert!(crate::Searcher::open(index_dir.path()).is_err());
        Ok(())
    }
}
