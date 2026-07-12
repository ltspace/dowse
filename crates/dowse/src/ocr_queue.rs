//! 图片的 OCR 待处理/已处理状态，随索引目录持久化在
//! `<index_dir>-ocr-queue.json`。[`OcrQueue`] 按索引目录在进程内单例复用
//! （[`OcrQueue::for_index_dir`]），供全量重建、启动对账、增量更新、OCR
//! worker 池共享同一份内存状态——各自独立开一份会导致后写的覆盖先写的进度。
//! 已处理的识别结果连同内容一起缓存，避免全量重建时把所有图片重新 OCR 一遍。

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

/// 单例注册表的查找 key：规范化掉 `\\?\`/`\\?\UNC\` 前缀，让不同写法的
/// 同一个索引目录路径落到同一个单例上（见 `for_index_dir` 的不变量说明）。
fn registry_key(index_dir: &Path) -> PathBuf {
    PathBuf::from(crate::display_path(&index_dir.to_string_lossy()))
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
    ///
    /// 不变量：单例注册表按**规范化后**的路径（剥掉 `\\?\`/`\\?\UNC\` 扩展长度
    /// 前缀，见 [`crate::display_path`]）去重。同一个索引目录可能被不同调用方
    /// 用不同的路径语法传进来——比如 `E:\index` 和 `Path::canonicalize()` 在
    /// Windows 上天生带前缀的 `\\?\E:\index`，两者指向磁盘上同一个目录。如果
    /// key 原样比较，会各建出一份独立的 `OcrQueue`，谁后 `save()` 就把谁的
    /// 进度覆盖掉。只有 map 的 key 做规范化；存进单例本体、参与文件 IO 的
    /// `index_dir` 字段仍然是第一次创建它的调用方传入的原始路径——长路径场景
    /// 要靠 `\\?\` 前缀绕开 Win32 MAX_PATH，不能剥。
    pub fn for_index_dir(index_dir: &Path) -> Arc<OcrQueue> {
        let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let key = registry_key(index_dir);
        let mut map = registry.lock().expect("ocr queue registry mutex poisoned");
        if let Some(existing) = map.get(&key) {
            return existing.clone();
        }
        let state = load_state(index_dir);
        let queue = Arc::new(OcrQueue {
            index_dir: index_dir.to_path_buf(),
            state: Mutex::new(state),
        });
        map.insert(key, queue.clone());
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

    /// 按当前扫描根裁剪队列：不落在 `roots` 任一根之下、或者对应文件已经不在
    /// 磁盘上的 pending/processed 条目一律丢弃。
    ///
    /// 这个队列是进程级单例（见 [`Self::for_index_dir`]），rebuild_index 每次
    /// 全量重建时会把内存里已有的历史条目原样存回——如果目标目录换过、或者
    /// 有文件在两次重建之间被删掉，这些条目没有任何清理路径，只增不减、永久
    /// 堆积。rebuild_index 在最终落盘前调用这个方法压实一次。
    pub fn compact(&self, roots: &[PathBuf]) {
        let mut state = self.state.lock().expect("ocr queue mutex poisoned");
        let keep = |key: &String| -> bool {
            let path = Path::new(key);
            roots.iter().any(|r| path.starts_with(r)) && path.exists()
        };
        state.pending.retain(|key, _| keep(key));
        state.processed.retain(|key, _| keep(key));
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

    /// 同一个索引目录用两种路径写法（有无 `\\?\` 扩展长度前缀）传进来，应该
    /// 落到同一个单例上——不然后保存的会把先保存的进度覆盖掉（P3 审查项）。
    #[test]
    fn for_index_dir_normalizes_extended_prefix_to_same_singleton() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("index");
        let prefixed = PathBuf::from(format!(r"\\?\{}", index_dir.display()));

        let a = OcrQueue::for_index_dir(&index_dir);
        let b = OcrQueue::for_index_dir(&prefixed);

        assert!(
            Arc::ptr_eq(&a, &b),
            "两种路径写法应该命中同一个进程内单例，不能各建一份互相覆盖"
        );
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

    /// compact 应该只留下"落在给定根之下、且文件仍在磁盘上"的 pending 条目——
    /// 换过目标目录（root 之外）、或者文件已经被删掉的历史条目都该丢弃。
    #[test]
    fn compact_drops_pending_outside_roots_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("index");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        let kept = root.join("kept.png");
        std::fs::write(&kept, b"x").unwrap();
        let gone = root.join("gone.png");
        std::fs::write(&gone, b"x").unwrap();
        let outside = dir.path().join("outside.png");
        std::fs::write(&outside, b"x").unwrap();

        let queue = OcrQueue::for_index_dir(&index_dir);
        queue.enqueue(kept.clone(), 1, 2);
        queue.enqueue(gone.clone(), 1, 2);
        queue.enqueue(outside.clone(), 1, 2);
        std::fs::remove_file(&gone).unwrap(); // 模拟"文件已删"

        queue.compact(std::slice::from_ref(&root));

        assert_eq!(
            queue.pending_len(),
            1,
            "只应保留 root 下、文件仍存在的 kept 一条"
        );
    }

    /// 同上，但针对已处理结果缓存（processed）：换目标目录/文件已删的旧
    /// 识别结果缓存也该被裁掉，不能永久占着。
    #[test]
    fn compact_drops_processed_cache_outside_roots_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("index");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();

        let kept = root.join("kept.png");
        std::fs::write(&kept, b"x").unwrap();
        let gone = root.join("gone.png");
        std::fs::write(&gone, b"x").unwrap();
        let outside = dir.path().join("outside.png");
        std::fs::write(&outside, b"x").unwrap();

        let queue = OcrQueue::for_index_dir(&index_dir);
        queue.mark_processed(kept.clone(), 1, 2, "内容".to_string());
        queue.mark_processed(gone.clone(), 1, 2, "内容".to_string());
        queue.mark_processed(outside.clone(), 1, 2, "内容".to_string());
        std::fs::remove_file(&gone).unwrap();

        queue.compact(std::slice::from_ref(&root));

        assert!(
            queue.cached_content(&kept, 1, 2).is_some(),
            "root 下且文件仍在应保留缓存"
        );
        assert!(
            queue.cached_content(&gone, 1, 2).is_none(),
            "文件已删应丢弃缓存"
        );
        assert!(
            queue.cached_content(&outside, 1, 2).is_none(),
            "root 之外应丢弃缓存"
        );
    }
}
