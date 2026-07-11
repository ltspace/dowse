use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 一条待处理记录：入队时的 (mtime, size)。worker 处理完之后拿当时的三元组去核对——
/// 如果文件在排队期间又变了，仍然按最新状态重新识别（见 OcrQueue::enqueue 的覆盖语义）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Stat {
    mtime: i64,
    size: u64,
}

/// 一条已处理记录：连同识别结果（双形态清洗后的 content）一起缓存。全量重建索引会
/// 把 tantivy 索引整个删掉重建，如果没有这份缓存，每次重建都要把所有图片重新 OCR
/// 一遍——量级上完全不可接受，所以"已处理"不只是个布尔标记，要把内容也存住。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Processed {
    mtime: i64,
    size: u64,
    content: String,
}

/// 落盘/加载的完整状态。用 String 而不是 PathBuf 做 map key——PathBuf 在 Windows
/// 上含反斜杠，直接当 serde_json 的 map key类型没问题，但 String 是这个代码库
/// 一贯的路径序列化方式（tantivy 的 path 字段也是存 to_string_lossy 的结果），
/// 保持一致，也省得依赖 PathBuf 的 Serialize 在 map key 位置的实现细节。
#[derive(Debug, Default, Serialize, Deserialize)]
struct QueueState {
    pending: HashMap<String, Stat>,
    processed: HashMap<String, Processed>,
}

fn key_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// OCR 队列：图片的 (path,mtime,size) 待处理表 + 已处理结果缓存，随索引目录持久化
/// 在 `<index_dir>-ocr-queue.json`（跟 meta.rs 的 `<index_dir>-meta.json` 同一个
/// "索引目录旁的兄弟文件"套路，不塞进 tantivy 自己管理的索引目录里）。
///
/// 进程内按 index_dir 单例复用（见 for_index_dir）：rebuild_index、reconcile、
/// 增量更新（IndexUpdater::apply）、OCR worker 池，这四处调用方全部共享同一份
/// 内存状态。如果各自独立开一份再各自落盘，后写的会把先写的进度覆盖掉——
/// 队列持久化的意义就没了。
pub struct OcrQueue {
    index_dir: PathBuf,
    state: Mutex<QueueState>,
}

static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<OcrQueue>>>> = OnceLock::new();

impl OcrQueue {
    /// 取（或首次创建）某个索引目录对应的进程内单例。第一次调用时从磁盘加载已持久化
    /// 的状态；磁盘上没有（旧索引、或者这是第一次跑 M4 之后的代码）就是空队列，不算错误。
    pub fn for_index_dir(index_dir: &Path) -> Arc<OcrQueue> {
        let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = registry.lock().expect("ocr queue registry mutex poisoned");
        if let Some(existing) = map.get(index_dir) {
            return existing.clone();
        }
        let state = load_state(index_dir);
        let queue = Arc::new(OcrQueue {
            index_dir: index_dir.to_path_buf(),
            state: Mutex::new(state),
        });
        map.insert(index_dir.to_path_buf(), queue.clone());
        queue
    }

    /// 若该路径在给定 (mtime,size) 下已经识别过，返回缓存的双形态 content，
    /// 免去重新 OCR——全量重建索引时最常走到这条路径。
    pub(crate) fn cached_content(&self, path: &Path, mtime: i64, size: u64) -> Option<String> {
        let state = self.state.lock().expect("ocr queue mutex poisoned");
        state
            .processed
            .get(&key_of(path))
            .filter(|p| p.mtime == mtime && p.size == size)
            .map(|p| p.content.clone())
    }

    /// 把一张图片放进待处理队列（worker 池后台消化）。已经在 pending 里的同路径
    /// 会被新的 (mtime,size) 覆盖——文件在排队等待期间又变了，以最新状态为准。
    pub(crate) fn enqueue(&self, path: PathBuf, mtime: i64, size: u64) {
        let mut state = self.state.lock().expect("ocr queue mutex poisoned");
        state.pending.insert(key_of(&path), Stat { mtime, size });
    }

    /// worker 取一项待处理。空队列返回 None，调用方自己决定要不要小睡。
    pub(crate) fn pop_pending(&self) -> Option<(PathBuf, i64, u64)> {
        let mut state = self.state.lock().expect("ocr queue mutex poisoned");
        let key = state.pending.keys().next().cloned()?;
        let stat = state.pending.remove(&key)?;
        Some((PathBuf::from(key), stat.mtime, stat.size))
    }

    /// 待处理项数，给 CLI 的 drain_ocr_queue 判断"清空了没"用。
    pub fn pending_len(&self) -> usize {
        self.state
            .lock()
            .expect("ocr queue mutex poisoned")
            .pending
            .len()
    }

    /// worker 识别完一张图片后回填：写进 processed 缓存（空字符串代表"已处理但无
    /// 文字"，同样不再重试，见设计文档"范围"一节）。
    pub(crate) fn mark_processed(&self, path: PathBuf, mtime: i64, size: u64, content: String) {
        let mut state = self.state.lock().expect("ocr queue mutex poisoned");
        state.processed.insert(
            key_of(&path),
            Processed {
                mtime,
                size,
                content,
            },
        );
    }

    /// 落盘。rebuild_index/reconcile 批量入队后各调一次；IndexUpdater::apply 在
    /// 一批增量里确实碰到过图片时也调一次；worker 每处理完一张图也调一次——
    /// 一次 JSON 序列化的代价远小于一次 OCR 识别（百毫秒级），可以接受。
    pub fn save(&self) -> Result<()> {
        let state = self.state.lock().expect("ocr queue mutex poisoned");
        save_state(&self.index_dir, &state)
    }
}

fn queue_path(index_dir: &Path) -> PathBuf {
    let stem = index_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dowse-index");
    index_dir.with_file_name(format!("{stem}-ocr-queue.json"))
}

/// 读取失败（文件不存在/损坏）一律按空队列处理，不往外传错误——OCR 队列状态
/// 只是个"能省则省"的进度缓存，丢了大不了重新识别一遍，不该拖累索引整体可用性。
fn load_state(index_dir: &Path) -> QueueState {
    let path = queue_path(index_dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            eprintln!(
                "OCR 队列状态解析失败，按空队列处理 {}: {err}",
                path.display()
            );
            QueueState::default()
        }),
        Err(_) => QueueState::default(),
    }
}

fn save_state(index_dir: &Path, state: &QueueState) -> Result<()> {
    let path = queue_path(index_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(state)?;
    std::fs::write(&path, bytes).context("写 OCR 队列状态失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_state_round_trips_pending_and_processed() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("index");

        let mut state = QueueState::default();
        state
            .pending
            .insert("C:\\shots\\a.png".to_string(), Stat { mtime: 1, size: 2 });
        state.processed.insert(
            "C:\\shots\\b.png".to_string(),
            Processed {
                mtime: 3,
                size: 4,
                content: "识别到的文字".to_string(),
            },
        );
        save_state(&index_dir, &state).unwrap();

        let loaded = load_state(&index_dir);
        assert_eq!(loaded.pending.len(), 1);
        assert_eq!(loaded.pending["C:\\shots\\a.png"].mtime, 1);
        assert_eq!(loaded.processed["C:\\shots\\b.png"].content, "识别到的文字");
    }

    /// 文件不存在（第一次跑 M4 代码的旧索引，或者压根还没有任何图片入过队）时
    /// 应该拿到空队列，而不是报错——这不该阻塞 rebuild_index/reconcile 正常工作。
    #[test]
    fn load_state_missing_file_returns_empty_queue() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("no-such-index");
        let state = load_state(&index_dir);
        assert!(state.pending.is_empty());
        assert!(state.processed.is_empty());
    }

    /// 断点续传的核心链路：入队 -> worker 取走 -> 标记已处理 -> 缓存命中；
    /// stat 变了（文件改过）不该命中旧缓存，得重新识别。
    #[test]
    fn enqueue_pop_and_mark_processed_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("index");
        let queue = OcrQueue::for_index_dir(&index_dir);

        let path = PathBuf::from("shot.png");
        queue.enqueue(path.clone(), 10, 20);
        assert_eq!(queue.pending_len(), 1);
        assert!(queue.cached_content(&path, 10, 20).is_none());

        let (popped_path, mtime, size) = queue.pop_pending().expect("队列非空应该能取出一项");
        assert_eq!(popped_path, path);
        assert_eq!((mtime, size), (10, 20));
        assert_eq!(queue.pending_len(), 0, "取走之后队列应该清空");

        queue.mark_processed(popped_path.clone(), mtime, size, "识别结果".to_string());
        assert_eq!(
            queue.cached_content(&popped_path, 10, 20).as_deref(),
            Some("识别结果")
        );
        assert!(
            queue.cached_content(&popped_path, 999, 20).is_none(),
            "stat 对不上不该命中缓存"
        );
    }

    /// 模拟"程序中途退出再启动"：save 之后不通过 for_index_dir（进程内单例会命中
    /// 内存缓存，绕不过磁盘），直接走底层 load_state 验证确实落了盘、断点数据还在。
    #[test]
    fn pending_progress_survives_save_and_reload_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("index");

        {
            let queue = OcrQueue::for_index_dir(&index_dir);
            queue.enqueue(PathBuf::from("a.png"), 1, 2);
            queue.enqueue(PathBuf::from("b.png"), 3, 4);
            let (path, mtime, size) = queue.pop_pending().unwrap();
            queue.mark_processed(path, mtime, size, "已识别".to_string());
            queue.save().unwrap();
        }

        let reloaded = load_state(&index_dir);
        assert_eq!(
            reloaded.pending.len(),
            1,
            "还剩一张没处理，应该还在 pending 里"
        );
        assert_eq!(
            reloaded.processed.len(),
            1,
            "已处理的那张应该被记住，重启不会重新识别"
        );
    }
}
