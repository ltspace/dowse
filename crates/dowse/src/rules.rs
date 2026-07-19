//! 索引规则配置（[`IndexRules`]）：把原本硬编码在 `extract.rs`/`indexer.rs`
//! 里的三件事抽出来做成可配置项——排除目录名列表、追加的文本扩展名列表、
//! 单文件体积上限（MB）。规则持久化在索引目录旁的 `<index_dir>-rules.json`
//! （跟 meta.json 并列的兄弟文件，不是塞进索引目录里——全量重建会
//! `remove_dir_all` 整个索引目录，放里面会被连带删掉，用户配置得能扛过重建）。
//!
//! **无规则文件 / 字段缺失 / 解析失败一律回落默认值**，且默认值逐字节等于
//! 抽出前的硬编码行为（见各 `DEFAULT_*` 常量）——没配过规则的老索引、老用户
//! 感知不到任何差别。
//!
//! 生效方式选的是**进程级全局**（[`active_rules`]）而不是逐层穿参：`extract_text`/
//! `rebuild_index` 等是 `pub` 且被 dowse-app 依赖的稳定入口，改签名会破坏向后
//! 兼容；`walk_index_files`/`is_extractable` 又深埋在 USN 实时翻译、对账等多条
//! 调用链底部，逐个穿参改动面极大。改成"库的写入类入口（`rebuild_index_with_progress`/
//! `IndexUpdater::open`）在开工前把当前索引目录的规则加载进全局，底层函数读全局"
//! 这一种，改动最小，且让 dowse-app 无需改一行代码就自动尊重 rules.json。
//! 底层真正做判断的逻辑都拆成接收 `&IndexRules` 的纯函数（`*_with`），全局只是
//! 给零参公开入口兜底的环境态，纯函数本身可脱离全局单测。

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 默认排除目录：依赖/构建产物或仓库内部数据，整棵跳过。等于抽出前
/// `indexer.rs` 里硬编码的 `SKIP_DIRS`。
const DEFAULT_EXCLUDE_DIRS: &[&str] = &["node_modules", "target", ".git", ".venv", "__pycache__"];

/// 默认单文件体积上限（MB）：超过就跳过，防止索引一个巨型日志把内存吃穿。
/// 等于抽出前 `extract.rs` 里硬编码的 `MAX_FILE_BYTES`（20MB）。
const DEFAULT_MAX_FILE_MB: u64 = 20;

/// 可配置的索引规则。三个字段分别对应抽出前散落在两个模块里的三处硬编码，
/// 默认值逐字节等于原行为（见 [`IndexRules::default`]）。
///
/// 字段各自带 `#[serde(default)]`：老 rules.json 缺某个字段、或者手写时漏了
/// 一项，反序列化时该字段静默补默认值，而不是整份解析失败——容错优先。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRules {
    /// 整棵跳过的目录名列表（精确名匹配）。另外，任何以 `.` 开头的目录始终
    /// 被跳过（内建结构规则，不受这个列表增删影响，见 [`IndexRules::is_dir_excluded`]）。
    #[serde(default = "default_exclude_dirs")]
    pub exclude_dirs: Vec<String>,
    /// 在内建文本扩展名白名单之外**追加**认定为纯文本的扩展名（不含点、小写）。
    /// 只追加、不覆盖内建白名单，命中的文件按纯文本读（自动探测编码）。
    #[serde(default)]
    pub extra_text_exts: Vec<String>,
    /// 单文件体积上限（MB）。超过上限的文件不抽取、跳过。
    #[serde(default = "default_max_file_mb")]
    pub max_file_mb: u64,
}

fn default_exclude_dirs() -> Vec<String> {
    DEFAULT_EXCLUDE_DIRS.iter().map(|s| s.to_string()).collect()
}

fn default_max_file_mb() -> u64 {
    DEFAULT_MAX_FILE_MB
}

impl Default for IndexRules {
    /// 默认规则 = 抽出前的硬编码行为，逐字节一致：排除
    /// node_modules/target/.git/.venv/__pycache__、不追加任何扩展名、单文件
    /// 上限 20MB。
    fn default() -> Self {
        Self {
            exclude_dirs: default_exclude_dirs(),
            extra_text_exts: Vec::new(),
            max_file_mb: DEFAULT_MAX_FILE_MB,
        }
    }
}

impl IndexRules {
    /// 把用户/文件里可能不规整的写法拉直：目录名去空白、去空项、去重（保序）；
    /// 追加扩展名去空白、剥掉可能带的前导点、统一小写、去空项、去重（保序）。
    /// 加载和保存两头都过一遍，索引期比对时不用再关心大小写/带不带点这些差异。
    pub fn normalize(&mut self) {
        self.exclude_dirs = dedup_preserving_order(
            self.exclude_dirs
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        );
        self.extra_text_exts = dedup_preserving_order(
            self.extra_text_exts
                .iter()
                .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|s| !s.is_empty()),
        );
    }

    /// 单文件体积上限换算成字节。`max_file_mb` 超大时用饱和乘法兜底，不 panic
    /// （现实里没人会配到 u64 溢出，但输入是用户可控的 JSON，稳妥起见）。
    pub(crate) fn max_file_bytes(&self) -> u64 {
        self.max_file_mb.saturating_mul(1024 * 1024)
    }

    /// 这个目录名是否该整棵跳过：命中排除列表，或者以 `.` 开头（隐藏目录）。
    /// 后者是内建结构规则，等于抽出前 `walk_index_files` 里 `name.starts_with('.')`
    /// 那一条，跟可配置的排除列表叠加生效。
    pub(crate) fn is_dir_excluded(&self, name: &str) -> bool {
        name.starts_with('.') || self.exclude_dirs.iter().any(|d| d == name)
    }

    /// 这个扩展名是否在追加白名单里（调用方保证传进来的是小写、不含点）。
    pub(crate) fn is_extra_text_ext(&self, ext: &str) -> bool {
        self.extra_text_exts.iter().any(|e| e == ext)
    }

    /// `path` 从 `root` 往下数，中间是否穿过了任一被排除的目录。给 MFT 快速
    /// 枚举那条路径用：它不像 walkdir 那样能在下钻时按目录剪枝，只能拿到重建好
    /// 的完整文件路径再逐条判定，这样才能让"快车道"跟 `walk_index_files` 的排除
    /// 口径一致（否则 node_modules 之类在快车道会照进索引）。只看中间目录段，
    /// 不看文件名本身。
    pub(crate) fn path_under_excluded_dir(&self, path: &Path, root: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(root) else {
            return false;
        };
        let comps: Vec<Component> = rel.components().collect();
        // 最后一段是文件名本身，不算目录，排除判定只看它前面的目录段。
        for comp in comps.iter().take(comps.len().saturating_sub(1)) {
            if let Component::Normal(name) = comp
                && self.is_dir_excluded(&name.to_string_lossy())
            {
                return true;
            }
        }
        false
    }
}

/// 保序去重：Vec 通常只有个位数项，线性 `contains` 足够，不值得引额外依赖。
fn dedup_preserving_order(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// 规则文件放在索引目录的**兄弟**位置（`<index_dir>-rules.json`），跟 meta.json
/// 同一套摆法、同一个理由：全量重建 `remove_dir_all` 整个索引目录，放里面会被
/// 一起删掉，而规则是要扛过重建的用户配置。
fn rules_path(index_dir: &Path) -> PathBuf {
    let stem = index_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dowse-index");
    index_dir.with_file_name(format!("{stem}-rules.json"))
}

/// 从索引目录读规则，容错优先：
/// - 文件不存在（从没配过规则，最常见）→ 静默返回默认规则；
/// - 解析失败（文件被改坏）→ 打一行告警后返回默认规则，不让一份坏配置把
///   建索引/监听整条流水线卡死；
/// - 字段缺失 → 由 `#[serde(default)]` 逐字段补默认值。
///
/// 读到的规则统一 `normalize` 一遍再返回。
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use dowse::load_rules;
///
/// // 没有 rules.json 时返回默认规则，永不报错。
/// let rules = load_rules(Path::new("./my-index"));
/// assert_eq!(rules.max_file_mb, 20);
/// ```
pub fn load_rules(index_dir: &Path) -> IndexRules {
    let path = rules_path(index_dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        // 没有规则文件就是"用默认规则"，不是错误，不打日志。
        Err(_) => return IndexRules::default(),
    };
    match serde_json::from_slice::<IndexRules>(&bytes) {
        Ok(mut rules) => {
            rules.normalize();
            rules
        }
        Err(err) => {
            eprintln!(
                "规则文件 {} 解析失败，本次回落到默认规则: {err}",
                path.display()
            );
            IndexRules::default()
        }
    }
}

/// 把规则写进索引目录旁的 rules.json（写前先 `normalize`）。CLI 的
/// `dowse rules set` 用它落盘。
///
/// # Examples
///
/// ```no_run
/// # fn main() -> anyhow::Result<()> {
/// use std::path::Path;
/// use dowse::{IndexRules, save_rules};
///
/// let mut rules = IndexRules::default();
/// rules.max_file_mb = 50;
/// save_rules(Path::new("./my-index"), &rules)?;
/// # Ok(())
/// # }
/// ```
pub fn save_rules(index_dir: &Path, rules: &IndexRules) -> Result<()> {
    let mut rules = rules.clone();
    rules.normalize();
    let path = rules_path(index_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(&rules)?;
    std::fs::write(&path, bytes).context("写规则文件失败")?;
    Ok(())
}

/// 进程级当前生效规则。用 `Arc` 包住，[`active_rules`] 取的时候只 bump 一次
/// 引用计数、不深拷贝——建索引热循环里每个文件都会取一次，深拷贝几个 Vec<String>
/// 在几十万文件规模上是白白的开销。
///
/// 边界假定：**单进程同时只服务一个活动索引目录**。CLI 和 dowse-app 都满足
/// （固定用 `%LOCALAPPDATA%\dowse` 下的单一索引，多根共用一份）。库消费者若在
/// 同一进程内对两个不同索引目录并发 `IndexUpdater::open`/`rebuild_index`，后
/// 开工的会覆盖这份全局，先开工的后续读到的是别人的规则——需要那种用法时应
/// 把规则穿参下沉（各 `*_with(&IndexRules)` 纯函数已备好），而不是继续用全局。
fn active_lock() -> &'static RwLock<Arc<IndexRules>> {
    static ACTIVE: OnceLock<RwLock<Arc<IndexRules>>> = OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(Arc::new(IndexRules::default())))
}

/// 取当前进程生效的规则（廉价克隆：只 bump `Arc` 引用计数）。底层的
/// `extract_text`/`is_extractable`/`walk_index_files` 等零参公开入口读它兜底。
/// 锁中毒时取内层值继续用，跟本 crate 其它锁一致，不因一次 panic 连锁瘫痪。
pub(crate) fn active_rules() -> Arc<IndexRules> {
    active_lock()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// 设置当前进程生效的规则。
pub(crate) fn set_active_rules(rules: IndexRules) {
    *active_lock().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(rules);
}

/// 从索引目录加载规则并设为当前进程生效规则，返回加载到的规则。库的写入类
/// 入口（`rebuild_index_with_progress`/`IndexUpdater::open`）在开工前调一次，
/// 让本次建索引/监听尊重这个索引目录旁的 rules.json。
pub(crate) fn load_active_rules(index_dir: &Path) -> IndexRules {
    let rules = load_rules(index_dir);
    set_active_rules(rules.clone());
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_pre_config_hardcoded_behavior() {
        let rules = IndexRules::default();
        assert_eq!(
            rules.exclude_dirs,
            vec!["node_modules", "target", ".git", ".venv", "__pycache__"]
        );
        assert!(rules.extra_text_exts.is_empty());
        assert_eq!(rules.max_file_mb, 20);
        // 逐字节等于抽出前 extract.rs 的 MAX_FILE_BYTES。
        assert_eq!(rules.max_file_bytes(), 20 * 1024 * 1024);
    }

    #[test]
    fn missing_rules_file_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        // 从没写过 rules.json：返回默认，不报错。
        assert_eq!(load_rules(dir.path()), IndexRules::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("index");
        let rules = IndexRules {
            exclude_dirs: vec!["node_modules".into(), "dist".into()],
            extra_text_exts: vec!["rst".into(), "adoc".into()],
            max_file_mb: 64,
        };
        save_rules(&index_dir, &rules).unwrap();
        assert_eq!(load_rules(&index_dir), rules);
    }

    #[test]
    fn missing_field_in_json_falls_back_per_field() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("idx");
        std::fs::create_dir_all(&index_dir).unwrap();
        // 只写了 max_file_mb，另两个字段缺失——应各自补默认值，而不是整份失败。
        let path = rules_path(&index_dir);
        std::fs::write(&path, r#"{"max_file_mb": 7}"#).unwrap();

        let rules = load_rules(&index_dir);
        assert_eq!(rules.max_file_mb, 7);
        assert_eq!(rules.exclude_dirs, default_exclude_dirs());
        assert!(rules.extra_text_exts.is_empty());
    }

    #[test]
    fn corrupted_json_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("idx");
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(rules_path(&index_dir), b"this is not json at all {[").unwrap();

        assert_eq!(load_rules(&index_dir), IndexRules::default());
    }

    #[test]
    fn normalize_lowercases_strips_dots_and_dedups() {
        let mut rules = IndexRules {
            exclude_dirs: vec![
                " node_modules ".into(),
                "dist".into(),
                "dist".into(),
                "".into(),
            ],
            extra_text_exts: vec![".RST".into(), "adoc".into(), "ADOC".into(), " ".into()],
            max_file_mb: 10,
        };
        rules.normalize();
        assert_eq!(rules.exclude_dirs, vec!["node_modules", "dist"]);
        assert_eq!(rules.extra_text_exts, vec!["rst", "adoc"]);
    }

    #[test]
    fn is_dir_excluded_covers_list_and_dot_prefix() {
        let rules = IndexRules {
            exclude_dirs: vec!["build".into()],
            extra_text_exts: vec![],
            max_file_mb: 20,
        };
        assert!(rules.is_dir_excluded("build"));
        assert!(rules.is_dir_excluded(".git"), "点开头的隐藏目录始终排除");
        assert!(!rules.is_dir_excluded("src"));
        // 不在自定义列表里的默认目录名不再被排除——列表是整体替换语义。
        assert!(!rules.is_dir_excluded("node_modules"));
    }

    #[test]
    fn path_under_excluded_dir_checks_intermediate_dirs_only() {
        let rules = IndexRules {
            exclude_dirs: vec!["node_modules".into()],
            extra_text_exts: vec![],
            max_file_mb: 20,
        };
        let root = Path::new("/proj");
        assert!(rules.path_under_excluded_dir(Path::new("/proj/node_modules/x/a.js"), root));
        assert!(!rules.path_under_excluded_dir(Path::new("/proj/src/a.rs"), root));
        // 文件名恰好叫 node_modules 不该被误伤——只看中间目录段。
        assert!(!rules.path_under_excluded_dir(Path::new("/proj/src/node_modules"), root));
    }
}
