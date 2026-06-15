use std::sync::Mutex;

use dowse_core::Searcher;

/// 索引 reader 常驻内存——这是 80ms 搜索预算的底气，每次搜索都开新 reader
/// 划不来。索引还没建过，或者目录被删/损坏，`searcher` 就是 None，
/// 前端据此显示"选个目录开始建索引"的引导。
pub struct SearchState(pub Mutex<Option<Searcher>>);

impl SearchState {
    /// 启动时尝试打开已有索引；打不开（没建过/损坏）不算错误，留空即可。
    pub fn load_initial() -> Self {
        let searcher = crate::config::index_dir()
            .ok()
            .and_then(|dir| Searcher::open(&dir).ok());
        Self(Mutex::new(searcher))
    }

    pub fn replace(&self, searcher: Searcher) {
        let mut guard = self.0.lock().expect("search state mutex poisoned");
        *guard = Some(searcher);
    }
}
