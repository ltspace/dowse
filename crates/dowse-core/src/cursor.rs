//! USN 游标：判定一个持久化的读取位置是否还能用来回放追平 USN Journal
//! （[`cursor_is_usable`]），以及多卷并发读取线程和 `run_watch` 消费者之间
//! "游标只能在对应变更确认提交后才允许前移"的同步机制（[`CursorSync`]）。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::mpsc::{SendError, Sender};

use serde::{Deserialize, Serialize};

use crate::events::WatchEvent;

/// 一个卷的标识。用盘符（大写、带冒号，如 `"C:"`）就够——本里程碑不做全盘/
/// 跨机迁移场景，盘符在同一台机器上稳定。
pub(crate) type VolumeKey = String;

/// meta.json 里存的游标：Journal ID + 下一个待读的 USN 位置（设计文档第四节）。
/// Journal ID 变了（USN Journal 被删除重建过）说明历史日志已经不可信，
/// 必须整卷退回 mtime 对账——`ensure_usable` 就是做这个判断的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UsnCursor {
    pub journal_id: u64,
    pub next_usn: i64,
}

/// 拿当前卷查询到的 Journal 状态跟持久化的游标比对，判定游标是否还能用来
/// 回放追平，还是必须退回全扫（设计文档"启动时"一节）。
///
/// - `journal_id` 对不上：Journal 被删除重建过，历史全部作废。
/// - 游标已经落在 `lowest_valid_usn` 之前：日志滚动把这段历史冲掉了
///   （磁盘写太快、日志配额太小），游标"过期"。
/// - 游标比 `next_usn`（当前日志写到的位置）还靠前：正常情况，直接从游标
///   读到 `next_usn` 就追平了。
pub(crate) fn cursor_is_usable(
    cursor: UsnCursor,
    live_journal_id: u64,
    lowest_valid_usn: i64,
) -> bool {
    cursor.journal_id == live_journal_id && cursor.next_usn >= lowest_valid_usn
}

/// 一批变更提交后，才允许把游标往前推——防止"游标先落盘、索引提交还没完成
/// 就崩溃"导致的漏账（设计文档"游标持久化与批量 commit 的原子性"）。
///
/// 实现思路：USN 事件源可能有多个卷的读取线程，都往同一个 `run_watch` 的
/// channel 里塞事件；`run_watch` 的主循环单线程消费（`rx.recv_timeout`），
/// 严格按发送顺序处理。只要读取线程发送事件之前，先把这条记录的
/// (卷, usn) 原子地推进同一把锁保护的队列，`run_watch` 每消费一个事件就
/// 出队一个，那么"游标出队时点"必然发生在"这条事件被送进防抖队列"之后、
/// 且严格早于"这一批的 commit"——commit 成功的回调（`on_committed`）里
/// 读到的就是"这一批实际吃进去的所有事件"对应的最新 usn，绝不会超前。
///
/// 达不到这个前提的事件（比如没有配对成功、翻译层判定 None 没有产出
/// WatchEvent 的记录）根本不会调用 `record_and_send`，所以游标天然不会
/// 越过一个还没解析完的重命名——这正是"宁可重放一批，不可漏一批"要的效果：
/// 崩溃后从更早的游标重放，Upsert 幂等，最多多做一次无害的重复工作。
pub(crate) struct CursorSync {
    inner: Mutex<CursorSyncState>,
}

#[derive(Default)]
struct CursorSyncState {
    /// FIFO：按事件发送顺序记录 (卷, usn)。
    pending: VecDeque<(VolumeKey, i64)>,
    /// 已经被 run_watch 消费（drain 进防抖队列）但还没确认 commit 的位置。
    read: HashMap<VolumeKey, i64>,
    /// 已经确认 commit、可以安全持久化的位置。
    safe: HashMap<VolumeKey, i64>,
}

impl CursorSync {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(CursorSyncState::default()),
        }
    }

    /// 读取线程侧：登记"这个 usn 对应的事件"并把它发进 channel，两步在同一把
    /// 锁内完成——这是多卷并发读取线程共用同一个 tx 时的关键：如果登记和
    /// 发送分成两次独立加锁，线程 A/B 交错执行时，"登记进 pending 队列的顺序"
    /// 和"事件实际送达 channel 的顺序"可能对不上（A 先登记、B 先发送），
    /// 会导致 `on_received` 按 FIFO 出队时张冠李戴，把 A 卷的 usn 错记成
    /// B 卷刚收到的事件对应的位置——这正是要严防的"游标越过未提交事件"。
    /// 锁在整个 push+send 期间只保护一次内存操作（mpsc unbounded send 不
    /// 阻塞），不会成为多卷吞吐的瓶颈。
    pub fn record_and_send(
        &self,
        volume: VolumeKey,
        usn: i64,
        event: WatchEvent,
        tx: &Sender<WatchEvent>,
    ) -> Result<(), SendError<WatchEvent>> {
        let mut guard = self.inner.lock().expect("cursor sync mutex poisoned");
        guard.pending.push_back((volume, usn));
        // send 必须在锁还持有的时候做——见上面的文档：push 和 send 要是同一个
        // 不可分割的临界区，多卷线程之间才不会交错出"登记顺序"和"送达顺序"
        // 对不上的问题。mpsc unbounded 的 send 只是入队，不阻塞，持锁开销可控。
        tx.send(event)
    }

    /// run_watch 消费侧：每收到（drain 进防抖队列）一个事件调一次
    /// （对应 `WatchProgress::Received`）。按 FIFO 出队，把这个 usn 标记成
    /// "已读、等 commit 确认"。
    pub fn on_received(&self) {
        let mut guard = self.inner.lock().expect("cursor sync mutex poisoned");
        if let Some((volume, usn)) = guard.pending.pop_front() {
            guard.read.insert(volume, usn);
        }
    }

    /// run_watch 消费侧：一批成功 commit 后调一次（对应
    /// `WatchProgress::Committed`）。把当前"已读"位置提升为"安全可持久化"
    /// 位置，返回这次提升后的快照，调用方拿去写 meta.json。
    pub fn on_committed(&self) -> HashMap<VolumeKey, i64> {
        let mut guard = self.inner.lock().expect("cursor sync mutex poisoned");
        for (volume, usn) in guard.read.clone() {
            guard.safe.insert(volume, usn);
        }
        guard.safe.clone()
    }
}

impl Default for CursorSync {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::mpsc;

    fn dummy_event(name: &str) -> WatchEvent {
        WatchEvent::Upsert(PathBuf::from(name))
    }

    #[test]
    fn cursor_usable_when_journal_id_matches_and_within_retention() {
        let cursor = UsnCursor {
            journal_id: 42,
            next_usn: 1000,
        };
        assert!(cursor_is_usable(cursor, 42, 500));
        assert!(cursor_is_usable(cursor, 42, 1000)); // 边界：等于也算没过期
    }

    #[test]
    fn cursor_unusable_when_journal_id_changed() {
        let cursor = UsnCursor {
            journal_id: 42,
            next_usn: 1000,
        };
        assert!(!cursor_is_usable(cursor, 43, 0));
    }

    #[test]
    fn cursor_unusable_when_expired_past_retention() {
        let cursor = UsnCursor {
            journal_id: 42,
            next_usn: 1000,
        };
        assert!(!cursor_is_usable(cursor, 42, 1001));
    }

    #[test]
    fn committed_snapshot_reflects_all_received_events_in_order() {
        let sync = CursorSync::new();
        let (tx, rx) = mpsc::channel();
        sync.record_and_send("C:".to_string(), 10, dummy_event("a"), &tx)
            .unwrap();
        sync.record_and_send("C:".to_string(), 20, dummy_event("b"), &tx)
            .unwrap();

        rx.recv().unwrap();
        sync.on_received(); // 消费 usn=10
        rx.recv().unwrap();
        sync.on_received(); // 消费 usn=20

        let safe = sync.on_committed();
        assert_eq!(safe.get("C:"), Some(&20));
    }

    #[test]
    fn uncommitted_reads_are_not_exposed_as_safe() {
        let sync = CursorSync::new();
        let (tx, rx) = mpsc::channel();
        sync.record_and_send("C:".to_string(), 10, dummy_event("a"), &tx)
            .unwrap();
        rx.recv().unwrap();
        sync.on_received();
        // 还没 commit：safe 应该还是空的
        let safe_before = {
            let guard = sync.inner.lock().unwrap();
            guard.safe.clone()
        };
        assert!(safe_before.is_empty());
    }

    #[test]
    fn multi_volume_interleaving_tracks_each_volume_independently() {
        let sync = CursorSync::new();
        let (tx, rx) = mpsc::channel();
        sync.record_and_send("C:".to_string(), 100, dummy_event("a"), &tx)
            .unwrap();
        sync.record_and_send("D:".to_string(), 200, dummy_event("b"), &tx)
            .unwrap();
        sync.record_and_send("C:".to_string(), 101, dummy_event("c"), &tx)
            .unwrap();

        rx.recv().unwrap();
        sync.on_received(); // C: -> 100
        rx.recv().unwrap();
        sync.on_received(); // D: -> 200
        let safe = sync.on_committed();
        assert_eq!(safe.get("C:"), Some(&100));
        assert_eq!(safe.get("D:"), Some(&200));

        rx.recv().unwrap();
        sync.on_received(); // C: -> 101
        let safe2 = sync.on_committed();
        assert_eq!(safe2.get("C:"), Some(&101));
        assert_eq!(safe2.get("D:"), Some(&200));
    }

    #[test]
    fn events_with_no_pending_record_do_not_advance_cursor() {
        // 对应"翻译层判定 None、没有产出 WatchEvent"的记录：读取线程根本不会
        // 调 record_and_send，所以 on_received 空转，不推进任何东西。
        let sync = CursorSync::new();
        sync.on_received();
        let safe = sync.on_committed();
        assert!(safe.is_empty());
    }

    /// 验证 record_and_send 的"登记顺序 == 发送顺序"不变式本身
    /// （单线程场景下天然成立，多线程下靠持锁跨两步操作保证，见方法文档）：
    /// 按顺序调用几次，pending 队列出队顺序应该跟发送顺序完全一致。
    #[test]
    fn record_and_send_preserves_pending_order_matching_send_order() {
        let sync = CursorSync::new();
        let (tx, rx) = mpsc::channel();
        for i in 0..5 {
            sync.record_and_send("C:".to_string(), i, dummy_event(&format!("f{i}")), &tx)
                .unwrap();
        }
        for expected_usn in 0..5 {
            let event = rx.recv().unwrap();
            assert_eq!(event, dummy_event(&format!("f{expected_usn}")));
            sync.on_received();
        }
        let safe = sync.on_committed();
        assert_eq!(safe.get("C:"), Some(&4));
    }
}
