use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::cursor::{UsnCursor, VolumeKey};

/// schema 版本号：字段定义每次不兼容变更就 +1。里程碑 3 给 schema 加了
/// mtime/size 两个字段，从里程碑 1 的隐式 v1 升到 v2。v0.5.0 给 mtime/size
/// 补上 FAST 属性（排序器需要）、新增 kind 字段（为里程碑 4 OCR 预留），
/// 再从 v2 升到 v3。打开索引时版本对不上就要求重建，不做静默迁移、不做
/// 自动升级——旧字段布局搜出来的结果不可靠，宁可让用户重建一次。
pub(crate) const SCHEMA_VERSION: u32 = 3;

/// 索引目录旁的元数据：schema 版本号 + 已注册的索引根目录列表 + 每个卷的
/// USN 游标（里程碑 6）。
///
/// `usn_cursors` 特意不参与 schema 版本号——它是"能不能走快速追平"的可选
/// 优化信息，不是索引字段布局，读不到/读到旧格式（没这个字段）都不该逼用户
/// 重建索引，`#[serde(default)]` 让旧 meta.json 静默补一个空表，退回到
/// mtime 全扫对账，行为上等价于这个卷从来没走过快速路径。
///
/// `roots` 的写入语义在多根索引（里程碑 7）之后从"单根整体替换"升级为
/// "逐根增删"：全量重建（`finish_rebuild`）仍然整份覆盖，但 [`append_root`]/
/// [`remove_root`] 只增删列表里的一项，其余字段原样保留。字段本身的类型/
/// 序列化格式完全没变（一直就是 `Vec<PathBuf>`），旧版本写的单根 meta.json
/// 不需要任何迁移，读出来就是一个长度为 1 的列表，向后兼容是自动的。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMeta {
    pub schema_version: u32,
    pub roots: Vec<PathBuf>,
    #[serde(default)]
    pub usn_cursors: HashMap<VolumeKey, UsnCursor>,
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

/// 读出已注册的 USN 游标（里程碑 6，按卷）。纯粹是"能不能走快速追平"的
/// 优化信号，读不到就当没有——不返回 Err、不影响调用方的主流程（对照
/// `usn_cursors` 字段本身"可选优化信息"的定位）。
pub(crate) fn load_usn_cursors(index_dir: &Path) -> HashMap<VolumeKey, UsnCursor> {
    load_meta(index_dir)
        .map(|meta| meta.usn_cursors)
        .unwrap_or_default()
}

/// 更新单个卷的 USN 游标，其余字段原样保留。读不到现有 meta（索引还没建过、
/// 或者刚好在被并发重建）就放弃，只打日志——游标持久化失败最多导致下次
/// 启动退回 mtime 全扫对账，不是数据安全问题，不值得把调用方搞挂。
pub(crate) fn save_usn_cursor(index_dir: &Path, volume: &VolumeKey, cursor: UsnCursor) {
    let mut meta = match load_meta(index_dir) {
        Ok(meta) => meta,
        Err(err) => {
            eprintln!("持久化 USN 游标失败（读不到索引元数据，{volume}）: {err}");
            return;
        }
    };
    meta.usn_cursors.insert(volume.clone(), cursor);
    if let Err(err) = save_meta(index_dir, &meta) {
        eprintln!("持久化 USN 游标失败（{volume}）: {err}");
    }
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

/// 对一条路径做尽力而为的归一，把它拉到一种稳定拼法，供嵌套前缀比较使用：
/// 1. 先直接 `canonicalize()`——路径在磁盘上存在时这一步就把短文件名（8.3）
///    展开成长名、并加上扩展长度前缀。
/// 2. 路径还不存在（如指向一个尚未创建的子目录）时 `canonicalize()` 会失败，
///    退而求其次：沿祖先向上找到最深的那个真实存在的目录 canonicalize，再把
///    剩下不存在的那截路径拼回去。
/// 3. 连祖先都解析不了就保留原样，不报错。
/// 4. 最后统一剥掉 `\\?\`/`\\?\UNC\` 前缀（见 [`crate::display_path`]），跟
///    其余比较口径对齐。
///
/// 存在的意义：Windows 短文件名在部分账户/机器上会让同一目录的原始路径和
/// canonicalize 结果分属两种拼法（如 `RUNNERA~1` 对 `runneradmin`）。已有根
/// 往往落在磁盘上（能直接展开成长名），而候选可能指向一个还不存在的子目录
/// （`canonicalize` 直接失败、只能保留短名），两侧拼法一错位，纯前缀比较就
/// 漏判真实存在的嵌套。给两侧都跑同一套归一，才能把它们拉到同一种拼法再比。
fn best_effort_normalize(path: &Path) -> PathBuf {
    let resolved = path.canonicalize().unwrap_or_else(|_| {
        for ancestor in path.ancestors() {
            if let (Ok(base), Ok(rest)) = (ancestor.canonicalize(), path.strip_prefix(ancestor)) {
                return base.join(rest);
            }
        }
        path.to_path_buf()
    });
    PathBuf::from(crate::display_path(&resolved.to_string_lossy()))
}

/// 校验候选根跟已有根之间不存在嵌套关系（含双向：候选是某个已有根的子目录，
/// 或者候选是某个已有根的父目录），也拒绝跟已有根完全相同的重复添加。
/// 嵌套会让 `delete_tree` 的前缀圈选删除和对账的孤儿清理语义变成泥潭
/// （删 B 会连带删掉嵌在它里面的 A、反之亦然），产品上也没有正当需求
/// （设计文档"核心操作语义"一节）。
///
/// 不同调用路径传入的根写法本身不统一：全量重建（`finish_rebuild`）存的是
/// 未经 canonicalize 的原始路径，`add_root`/`append_root` 存的是 canonicalize
/// 过的路径；候选还可能指向一个磁盘上尚不存在的子目录。直接按原始字符串比较，
/// 会因为扩展长度前缀（`\\?\`/`\\?\UNC\`）带不带、Windows 短文件名（8.3）
/// 展没展开（如 `RUNNERA~1` 对 `runneradmin`）这类拼法差异，漏判真实存在的
/// 嵌套。这里对 `candidate` 和每一个 `existing` 都跑同一套 [`best_effort_normalize`]，
/// 把两侧拉到同一种拼法再比较；报错文案仍然用 `root`/`candidate` 的原始值，
/// 不把内部的归一形态暴露给用户。
pub(crate) fn assert_no_root_nesting(existing: &[PathBuf], candidate: &Path) -> Result<()> {
    let candidate_norm = best_effort_normalize(candidate);
    for root in existing {
        let root_norm = best_effort_normalize(root);
        if root_norm == candidate_norm {
            bail!("目录 {} 已经是索引根，不用重复添加", candidate.display());
        }
        if candidate_norm.starts_with(&root_norm) {
            bail!(
                "目录 {} 是已有根 {} 的子目录，不允许嵌套添加",
                candidate.display(),
                root.display()
            );
        }
        if root_norm.starts_with(&candidate_norm) {
            bail!(
                "目录 {} 是已有根 {} 的父目录，不允许嵌套添加",
                candidate.display(),
                root.display()
            );
        }
    }
    Ok(())
}

/// 把一个新根追加进 meta.json 的 roots 列表，其余字段（schema_version/
/// usn_cursors）原样保留。调用方（`roots::add_root_with_progress`）必须先
/// 完成目录树 upsert、再调这个函数——顺序不能反：设计文档"边界与失败"一节
/// 要求半路崩溃不留幽灵根，只有 upsert 完成后才写 meta 才能保证这一点
/// （半程崩溃时根本还没走到这一步，索引里那些孤儿文档由对账新增的孤儿
/// 清理规则兜底清掉）。
pub(crate) fn append_root(index_dir: &Path, root: &Path) -> Result<()> {
    let mut meta = load_meta(index_dir)?;
    assert_no_root_nesting(&meta.roots, root)?;
    meta.roots.push(root.to_path_buf());
    save_meta(index_dir, &meta)
}

/// 把一个根从 meta.json 的 roots 列表里移除，其余字段原样保留。不存在的根
/// 直接报错（调用方应该先用 `registered_roots` 确认这个根确实注册过，避免
/// 手误传错路径时静默什么都不做）。
///
/// 调用方（`roots::remove_root`）必须先调这个函数、再删文档——顺序同样不能
/// 反：设计文档要求半路崩溃时残留文档由"不属于任何注册根就删"的对账规则
/// 兜底，这要求 meta 先于文档删除完成落盘。
pub(crate) fn remove_root_from_meta(index_dir: &Path, root: &Path) -> Result<()> {
    let mut meta = load_meta(index_dir)?;
    let before = meta.roots.len();
    meta.roots.retain(|r| r != root);
    if meta.roots.len() == before {
        bail!("目录 {} 不是已注册的索引根", root.display());
    }
    save_meta(index_dir, &meta)
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
                usn_cursors: HashMap::new(),
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

    /// 旧版 meta.json（里程碑 6 之前写的，没有 usn_cursors 字段）应该照常解析
    /// 成功，`usn_cursors` 静默补成空表——不能因为字段升级就让老索引读不动。
    #[test]
    fn meta_without_usn_cursors_field_deserializes_with_empty_default() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let path = meta_path(index_dir.path());
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, r#"{"schema_version":3,"roots":["C:\\watch"]}"#)?;

        let meta = load_meta(index_dir.path())?;
        assert!(meta.usn_cursors.is_empty());
        Ok(())
    }

    #[test]
    fn save_and_load_usn_cursor_round_trips() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "内容")?;
        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        let cursor = UsnCursor {
            journal_id: 12345,
            next_usn: 6789,
        };
        save_usn_cursor(index_dir.path(), &"C:".to_string(), cursor);

        let cursors = load_usn_cursors(index_dir.path());
        assert_eq!(cursors.get("C:"), Some(&cursor));

        // roots/schema_version 不应该被这次更新动到
        let meta = load_meta(index_dir.path())?;
        assert_eq!(meta.schema_version, SCHEMA_VERSION);
        assert_eq!(meta.roots, vec![target_dir.path().to_path_buf()]);
        Ok(())
    }

    #[test]
    fn load_usn_cursors_on_missing_index_returns_empty_not_error() {
        let index_dir = tempfile::tempdir().unwrap();
        // 从没建过索引：不应该 panic，也不应该把错误传染给调用方。
        assert!(load_usn_cursors(index_dir.path()).is_empty());
    }

    #[test]
    fn nesting_rejects_child_and_parent_both_directions() {
        let existing = vec![PathBuf::from(r"C:\docs\project")];

        let child = PathBuf::from(r"C:\docs\project\sub");
        assert!(
            assert_no_root_nesting(&existing, &child).is_err(),
            "候选是已有根的子目录应该被拒绝"
        );

        let parent = PathBuf::from(r"C:\docs");
        assert!(
            assert_no_root_nesting(&existing, &parent).is_err(),
            "候选是已有根的父目录应该被拒绝"
        );

        let duplicate = PathBuf::from(r"C:\docs\project");
        assert!(
            assert_no_root_nesting(&existing, &duplicate).is_err(),
            "跟已有根完全相同应该被拒绝"
        );

        let sibling = PathBuf::from(r"C:\docs\other");
        assert!(
            assert_no_root_nesting(&existing, &sibling).is_ok(),
            "兄弟目录不该被误判为嵌套"
        );
    }

    /// 一边带 `\\?\` 扩展长度前缀、一边不带的同一个目录也应该判定为嵌套——
    /// `canonicalize()` 会加这个前缀，托盘"更改索引文件夹"目前不会，两条
    /// 路径产出的根写法不一致，嵌套校验不能因为这个漏判。
    #[test]
    fn nesting_check_normalizes_extended_length_prefix() {
        let existing = vec![PathBuf::from(r"\\?\C:\docs\project")];
        let child = PathBuf::from(r"C:\docs\project\sub");
        assert!(assert_no_root_nesting(&existing, &child).is_err());
    }

    #[test]
    fn append_and_remove_root_round_trip() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "内容")?;
        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        let second = tempfile::Builder::new().prefix("dowse-test2-").tempdir()?;
        append_root(index_dir.path(), second.path())?;

        let meta = load_meta(index_dir.path())?;
        assert_eq!(
            meta.roots,
            vec![target_dir.path().to_path_buf(), second.path().to_path_buf()],
            "追加根不应该动到已有的根"
        );

        remove_root_from_meta(index_dir.path(), target_dir.path())?;
        let meta = load_meta(index_dir.path())?;
        assert_eq!(
            meta.roots,
            vec![second.path().to_path_buf()],
            "移除根不应该动到剩下的根"
        );
        Ok(())
    }

    #[test]
    fn append_root_rejects_nested_candidate() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "内容")?;
        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        let nested = target_dir.path().join("sub");
        let err = append_root(index_dir.path(), &nested)
            .expect_err("子目录应该被拒绝，且不应该写入 meta");

        assert!(err.to_string().contains("嵌套"));
        let meta = load_meta(index_dir.path())?;
        assert_eq!(
            meta.roots,
            vec![target_dir.path().to_path_buf()],
            "校验失败时不应该污染已有的 roots"
        );
        Ok(())
    }

    #[test]
    fn remove_root_from_meta_errors_on_unregistered_root() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "内容")?;
        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        let unrelated = tempfile::tempdir()?;
        assert!(remove_root_from_meta(index_dir.path(), unrelated.path()).is_err());
        Ok(())
    }
}
