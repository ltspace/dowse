use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 静默窗口（毫秒）：编辑器保存一次会连发写临时文件/改名/改属性等好几个事件，
/// 编译或下载一秒能产出几百个。攒够这段时间没有新事件，再把这一批一起处理。
/// 这个常量给宿主的事件循环设 recv 超时用——防抖队列本身不碰时钟，方便纯逻辑测试。
pub const QUIET_WINDOW_MS: u64 = 500;

/// 水位阈值：待处理的 path 攒到这么多就立刻刷一批，不再等静默窗口，防内存膨胀。
pub const WATER_LEVEL: usize = 5000;

/// 文件系统监听的原始事件。事件源（notify，或将来里程碑 6 的 USN Journal）
/// 把各平台的原生事件归一成这几类再喂进防抖队列。刻意只带路径、不带时间戳/inode
/// 等平台细节，让防抖/合并逻辑是纯函数，不依赖真实文件系统。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// 文件新建或修改。两者索引操作相同（先删后加，天然幂等），归成一类。
    Upsert(PathBuf),
    /// 单个文件删除。
    Remove(PathBuf),
    /// 目录删除、或改名移出监听范围：整棵子树要从索引里前缀圈选删除。
    RemoveDir(PathBuf),
    /// 重命名，from/to 都由事件源给出。防抖队列按"删旧名 + 加新名"拆开处理；
    /// 只有一边落在监听根内时（移入/移出），就退化成单边的加/删。
    Rename { from: PathBuf, to: PathBuf },
}

/// 一个 path 在防抖窗口内合并后的最终意图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOp {
    /// 先删后加。新建、修改、重命名的新名都落到这里。
    Upsert,
    /// 删除单个文件的索引文档。
    Remove,
    /// 前缀圈选删除整棵子树。
    RemoveTree,
}

/// 防抖合并后交给更新器的一条变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChange {
    pub path: PathBuf,
    pub op: PendingOp,
}

/// 防抖队列：500ms 静默窗口内合并同一 path 的多次事件，只保留最终状态；
/// 水位到 WATER_LEVEL 就提示宿主强制刷批。纯内存逻辑，不碰时钟也不碰磁盘——
/// 时间窗口由宿主的事件循环（watch.rs）用 recv 超时驱动。
pub struct Debouncer {
    /// 监听根列表，用来判定 rename 的某一端是否落在监听范围内。
    roots: Vec<PathBuf>,
    /// path -> 最终意图。用 BTreeMap 让 drain 出来的批次顺序确定，方便测试和复现。
    pending: BTreeMap<PathBuf, PendingOp>,
}

impl Debouncer {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self {
            roots,
            pending: BTreeMap::new(),
        }
    }

    /// 判定路径是否落在任一监听根内。根列表为空时不做过滤（视为都在范围内），
    /// 避免退化成"什么都不处理"。Path::starts_with 是按路径组件比对的，
    /// `/watch` 不会误配 `/watchother`。
    fn in_scope(&self, path: &Path) -> bool {
        self.roots.is_empty() || self.roots.iter().any(|r| path.starts_with(r))
    }

    /// 吃进一个事件，合并进队列。返回 true 表示水位到了、宿主应立即刷一批。
    pub fn push(&mut self, event: WatchEvent) -> bool {
        match event {
            WatchEvent::Upsert(p) => {
                self.pending.insert(p, PendingOp::Upsert);
            }
            WatchEvent::Remove(p) => {
                self.pending.insert(p, PendingOp::Remove);
            }
            WatchEvent::RemoveDir(p) => {
                self.pending.insert(p, PendingOp::RemoveTree);
            }
            WatchEvent::Rename { from, to } => {
                // 删旧名 + 加新名；某一端不在监听范围内就只处理另一端。
                if self.in_scope(&from) {
                    self.pending.insert(from, PendingOp::Remove);
                }
                if self.in_scope(&to) {
                    self.pending.insert(to, PendingOp::Upsert);
                }
            }
        }
        self.pending.len() >= WATER_LEVEL
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// 取走当前所有待处理变更并清空队列。按 path 排序返回，顺序确定。
    pub fn drain(&mut self) -> Vec<PendingChange> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(path, op)| PendingChange { path, op })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        // 用不依赖真实文件系统的绝对路径，纯逻辑测试。
        if cfg!(windows) {
            PathBuf::from(r"C:\watch")
        } else {
            PathBuf::from("/watch")
        }
    }

    fn under(root: &Path, rel: &str) -> PathBuf {
        root.join(rel)
    }

    #[test]
    fn multiple_events_on_same_path_merge_into_one() {
        let r = root();
        let mut d = Debouncer::new(vec![r.clone()]);
        let p = under(&r, "a.md");

        d.push(WatchEvent::Upsert(p.clone()));
        d.push(WatchEvent::Upsert(p.clone()));
        d.push(WatchEvent::Remove(p.clone()));
        d.push(WatchEvent::Upsert(p.clone()));

        let batch = d.drain();
        assert_eq!(
            batch,
            vec![PendingChange {
                path: p,
                op: PendingOp::Upsert,
            }],
            "同一 path 的多次事件应合并成一条，保留最后状态"
        );
        assert!(d.is_empty(), "drain 之后队列应清空");
    }

    #[test]
    fn create_then_delete_ends_as_remove() {
        let r = root();
        let mut d = Debouncer::new(vec![r.clone()]);
        let p = under(&r, "tmp.md");
        d.push(WatchEvent::Upsert(p.clone()));
        d.push(WatchEvent::Remove(p.clone()));
        assert_eq!(d.drain(), vec![PendingChange { path: p, op: PendingOp::Remove }]);
    }

    #[test]
    fn rename_both_sides_in_scope_becomes_remove_old_upsert_new() {
        let r = root();
        let mut d = Debouncer::new(vec![r.clone()]);
        let from = under(&r, "old.md");
        let to = under(&r, "new.md");

        d.push(WatchEvent::Rename {
            from: from.clone(),
            to: to.clone(),
        });

        let batch = d.drain();
        assert_eq!(batch.len(), 2, "双边都在监听内：删旧 + 加新，共两条");
        assert!(batch.contains(&PendingChange { path: from, op: PendingOp::Remove }));
        assert!(batch.contains(&PendingChange { path: to, op: PendingOp::Upsert }));
    }

    #[test]
    fn rename_only_target_in_scope_becomes_upsert() {
        // 文件从监听范围外移进来：只有新名在范围内，当"新增"处理。
        let r = root();
        let mut d = Debouncer::new(vec![r.clone()]);
        let outside = if cfg!(windows) {
            PathBuf::from(r"C:\elsewhere\old.md")
        } else {
            PathBuf::from("/elsewhere/old.md")
        };
        let to = under(&r, "moved-in.md");

        d.push(WatchEvent::Rename {
            from: outside,
            to: to.clone(),
        });

        assert_eq!(
            d.drain(),
            vec![PendingChange { path: to, op: PendingOp::Upsert }],
            "只有新名在监听内，应只产生一条 Upsert"
        );
    }

    #[test]
    fn rename_only_source_in_scope_becomes_remove() {
        // 文件被移出监听范围：只有旧名在范围内，当"删除"处理。
        let r = root();
        let mut d = Debouncer::new(vec![r.clone()]);
        let from = under(&r, "moved-out.md");
        let outside = if cfg!(windows) {
            PathBuf::from(r"C:\elsewhere\new.md")
        } else {
            PathBuf::from("/elsewhere/new.md")
        };

        d.push(WatchEvent::Rename {
            from: from.clone(),
            to: outside,
        });

        assert_eq!(
            d.drain(),
            vec![PendingChange { path: from, op: PendingOp::Remove }],
            "只有旧名在监听内，应只产生一条 Remove"
        );
    }

    #[test]
    fn water_level_triggers_forced_flush() {
        let r = root();
        let mut d = Debouncer::new(vec![r.clone()]);

        let mut forced = false;
        for i in 0..WATER_LEVEL {
            forced = d.push(WatchEvent::Upsert(under(&r, &format!("f{i}.md"))));
        }
        assert!(forced, "攒到水位阈值时最后一次 push 应返回 true 提示强制刷批");
        assert_eq!(d.len(), WATER_LEVEL);

        // 水位以下不触发
        let mut d2 = Debouncer::new(vec![r.clone()]);
        let mut forced2 = false;
        for i in 0..(WATER_LEVEL - 1) {
            forced2 = d2.push(WatchEvent::Upsert(under(&r, &format!("g{i}.md"))));
        }
        assert!(!forced2, "水位以下不应触发强制刷批");
    }

    #[test]
    fn remove_dir_maps_to_remove_tree() {
        let r = root();
        let mut d = Debouncer::new(vec![r.clone()]);
        let dir = under(&r, "sub");
        d.push(WatchEvent::RemoveDir(dir.clone()));
        assert_eq!(
            d.drain(),
            vec![PendingChange { path: dir, op: PendingOp::RemoveTree }]
        );
    }
}
