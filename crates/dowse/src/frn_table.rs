//! FRN（文件参照号）表：MFT/USN 记录只带"这一层"的信息（父是谁、自己叫
//! 什么），完整路径要顺着父链拼上去。[`FrnTable`] 维护 FRN → (父 FRN, 名字)
//! 的内存表，配一份"监听根 FRN → 根路径"的锚点表，从任意 FRN 往上找到锚点
//! 就能拼出完整路径（[`FrnTable::reconstruct_path`]）；链断了返回 `None`，
//! 调用方自己决定是丢弃还是用 FRN 打开句柄反查兜底。

use std::collections::HashMap;
use std::path::PathBuf;

/// 一条 FRN 表记录：父 FRN + 这一层的名字 + 是不是目录。
/// MFT 记录和 USN 记录给的都是"这一层"的信息（父是谁、自己叫什么），
/// 完整路径要顺着父链一路拼上去（见 [`FrnTable::reconstruct_path`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrnEntry {
    pub parent_frn: u64,
    pub name: String,
    pub is_dir: bool,
}

/// FRN（文件参照号）→ (父 FRN, 名字) 的内存表，MFT 快速枚举时批量建好，
/// USN 事件源运行期间增量维护。设计文档第三节："USN 记录给的是 FRN + 文件名，
/// 不是全路径：维护 FRN → 路径的缓存表"。
///
/// 只保留落在已注册监听根内的条目（设计文档"明确不做"一节：全盘模式不做），
/// 所以还额外记一份"根 FRN → 根路径"的锚点表：path 重建从任意 FRN 往上找父，
/// 找到锚点就停,拼出来的就是完整路径；找不到锚点（链断了，比如中间目录没被
/// MFT 枚举收进来，或者压根不在任何注册根下）就返回 None，调用方自己决定
/// 是丢弃还是用 FRN 打开句柄反查兜底（设计文档"缓存 miss...兜底"）。
#[derive(Debug, Default)]
pub(crate) struct FrnTable {
    entries: HashMap<u64, FrnEntry>,
    /// 监听根自身的 FRN → 根路径。是路径重建的锚点，也是"这条链是否落在监听
    /// 范围内"的判定依据。
    roots: HashMap<u64, PathBuf>,
}

impl FrnTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个监听根的 FRN 和它的绝对路径。MFT 枚举/USN 源启动时，先把每个
    /// 注册根自己的 FRN 解析出来，登记成锚点。
    pub fn register_root(&mut self, frn: u64, path: PathBuf) {
        self.roots.insert(frn, path);
    }

    /// 插入或覆盖一条记录。MFT 枚举时批量插入；USN 事件到达时增量更新——
    /// 每次都用记录自带的 (parent_frn, name) 覆盖旧值，天然幂等，不用先查再改。
    pub fn upsert(&mut self, frn: u64, entry: FrnEntry) {
        self.entries.insert(frn, entry);
    }

    /// 只在测试里用来直接查一条记录做断言；生产代码只关心
    /// [`FrnTable::reconstruct_path`] 拼出来的完整路径，不需要单独查记录本身。
    #[cfg(test)]
    pub fn get(&self, frn: u64) -> Option<&FrnEntry> {
        self.entries.get(&frn)
    }

    /// 一个 FRN 从表里彻底移除（对应文件被删）。
    pub fn remove(&mut self, frn: u64) -> Option<FrnEntry> {
        self.entries.remove(&frn)
    }

    /// 从 frn 往上顺着父链拼出完整路径。命中锚点（某个注册根自己的 FRN）就
    /// 停下，拼出锚点路径 + 一路收集到的各层名字。
    ///
    /// 链断了（某个祖先的 FRN 不在表里，也不是锚点）返回 None——可能是这个
    /// FRN 压根不在任何监听根下（正常，调用方应该丢弃），也可能是缓存没跟上
    /// （真正的 miss，调用方应该用 FRN 打开句柄反查兜底）。这个函数本身不
    /// 区分这两种情况：区分靠平台层的兜底逻辑，纯逻辑层只负责"能拼就拼，
    /// 拼不出来就说拼不出来"。
    ///
    /// 最多爬 MAX_DEPTH 层就放弃——防御环形父链（理论上不该出现，但别让
    /// 一条坏数据写成死循环）。
    pub fn reconstruct_path(&self, frn: u64) -> Option<PathBuf> {
        const MAX_DEPTH: usize = 512;

        if let Some(root) = self.roots.get(&frn) {
            return Some(root.clone());
        }

        let mut names: Vec<&str> = Vec::new();
        let mut current = frn;
        for _ in 0..MAX_DEPTH {
            if let Some(root) = self.roots.get(&current) {
                let mut path = root.clone();
                for name in names.iter().rev() {
                    path.push(name);
                }
                return Some(path);
            }
            let entry = self.entries.get(&current)?;
            names.push(entry.name.as_str());
            current = entry.parent_frn;
        }
        None
    }

    /// frn 是否落在任一注册根下——判断的是"当前表里已知的位置"，不代表磁盘上
    /// 的真实状态（那是上层事件翻译该操心的事）。
    pub fn in_scope(&self, frn: u64) -> bool {
        self.reconstruct_path(frn).is_some()
    }

    /// MFT 枚举先把整卷记录都塞进一张临时表（否则任意顺序到达的父子记录没法
    /// 互相解析），枚举完毕后调用这个方法瘦身：只留下落在已注册监听根内的
    /// 条目——常驻内存的 FRN 表大小只跟"监听根下有多少文件"成正比，不跟
    /// "整卷有多少文件"成正比（设计文档"性能预算"一节的内存上限就是照这个
    /// 假设定的）。锚点（roots）本身不受影响。
    pub fn retain_in_scope(&mut self) {
        let keep: std::collections::HashSet<u64> = self
            .entries
            .keys()
            .copied()
            .filter(|&frn| self.in_scope(frn))
            .collect();
        self.entries.retain(|frn, _| keep.contains(frn));
    }

    /// 表里当前登记的条目数（不含 roots 锚点），MFT 枚举统计用。
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// 遍历所有条目（不含 roots 锚点）。MFT 枚举完毕后用来收集"这个根子树下
    /// 所有文件的路径清单"喂给索引管线。
    pub fn iter(&self) -> impl Iterator<Item = (&u64, &FrnEntry)> {
        self.entries.iter()
    }
}

/// 从一个绝对路径的各层组件推导出"如果要把它挂进 FrnTable，各层叫什么"——
/// 只在测试里用来构造期望路径，不是产品逻辑（产品里名字来自 MFT/USN 记录，
/// 不是从路径反推）。
#[cfg(test)]
fn last_component(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_path() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\watch")
        } else {
            PathBuf::from("/watch")
        }
    }

    #[test]
    fn reconstructs_path_for_root_itself() {
        let mut table = FrnTable::new();
        table.register_root(1, root_path());
        assert_eq!(table.reconstruct_path(1), Some(root_path()));
    }

    #[test]
    fn reconstructs_nested_path_through_parent_chain() {
        let mut table = FrnTable::new();
        table.register_root(1, root_path());
        table.upsert(
            2,
            FrnEntry {
                parent_frn: 1,
                name: "sub".to_string(),
                is_dir: true,
            },
        );
        table.upsert(
            3,
            FrnEntry {
                parent_frn: 2,
                name: "a.md".to_string(),
                is_dir: false,
            },
        );

        assert_eq!(
            table.reconstruct_path(3),
            Some(root_path().join("sub").join("a.md"))
        );
        assert_eq!(
            last_component(&root_path().join("sub").join("a.md")),
            "a.md"
        );
    }

    #[test]
    fn missing_ancestor_breaks_the_chain_and_returns_none() {
        let mut table = FrnTable::new();
        table.register_root(1, root_path());
        // frn 3 的父 frn 2 从没被登记过：链断了。
        table.upsert(
            3,
            FrnEntry {
                parent_frn: 2,
                name: "orphan.md".to_string(),
                is_dir: false,
            },
        );
        assert_eq!(table.reconstruct_path(3), None);
        assert!(!table.in_scope(3));
    }

    #[test]
    fn frn_outside_any_registered_root_returns_none() {
        let mut table = FrnTable::new();
        table.register_root(1, root_path());
        table.upsert(
            99,
            FrnEntry {
                parent_frn: 42, // 42 既不是锚点也没登记过
                name: "elsewhere.md".to_string(),
                is_dir: false,
            },
        );
        assert_eq!(table.reconstruct_path(99), None);
    }

    #[test]
    fn upsert_overwrites_previous_entry_for_same_frn() {
        let mut table = FrnTable::new();
        table.register_root(1, root_path());
        table.upsert(
            2,
            FrnEntry {
                parent_frn: 1,
                name: "old-name.md".to_string(),
                is_dir: false,
            },
        );
        assert_eq!(
            table.reconstruct_path(2),
            Some(root_path().join("old-name.md"))
        );

        table.upsert(
            2,
            FrnEntry {
                parent_frn: 1,
                name: "new-name.md".to_string(),
                is_dir: false,
            },
        );
        assert_eq!(
            table.reconstruct_path(2),
            Some(root_path().join("new-name.md"))
        );
        assert_eq!(table.entry_count(), 1);
    }

    #[test]
    fn remove_drops_entry_and_breaks_descendants() {
        let mut table = FrnTable::new();
        table.register_root(1, root_path());
        table.upsert(
            2,
            FrnEntry {
                parent_frn: 1,
                name: "sub".to_string(),
                is_dir: true,
            },
        );
        table.upsert(
            3,
            FrnEntry {
                parent_frn: 2,
                name: "a.md".to_string(),
                is_dir: false,
            },
        );

        table.remove(2);
        assert_eq!(table.reconstruct_path(2), None);
        // 子节点的父链现在断了
        assert_eq!(table.reconstruct_path(3), None);
    }

    #[test]
    fn retain_in_scope_drops_entries_whose_chain_never_reaches_a_root() {
        let mut table = FrnTable::new();
        table.register_root(1, root_path());
        // 落在监听根内
        table.upsert(
            2,
            FrnEntry {
                parent_frn: 1,
                name: "sub".to_string(),
                is_dir: true,
            },
        );
        table.upsert(
            3,
            FrnEntry {
                parent_frn: 2,
                name: "a.md".to_string(),
                is_dir: false,
            },
        );
        // 不落在任何监听根内（模拟整卷 MFT 枚举捎带回来的无关文件）
        table.upsert(
            100,
            FrnEntry {
                parent_frn: 999,
                name: "unrelated.md".to_string(),
                is_dir: false,
            },
        );

        assert_eq!(table.entry_count(), 3);
        table.retain_in_scope();
        assert_eq!(table.entry_count(), 2);
        assert_eq!(
            table.reconstruct_path(3),
            Some(root_path().join("sub").join("a.md"))
        );
        assert_eq!(table.reconstruct_path(100), None);
    }

    #[test]
    fn circular_parent_chain_terminates_instead_of_hanging() {
        let mut table = FrnTable::new();
        table.register_root(1, root_path());
        // 故意造一个环：2 的父是 3，3 的父是 2，谁都不是锚点。
        table.upsert(
            2,
            FrnEntry {
                parent_frn: 3,
                name: "a".to_string(),
                is_dir: true,
            },
        );
        table.upsert(
            3,
            FrnEntry {
                parent_frn: 2,
                name: "b".to_string(),
                is_dir: true,
            },
        );
        assert_eq!(table.reconstruct_path(2), None);
    }
}
