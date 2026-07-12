use std::path::Path;

/// 支持 OCR 的图片扩展名（不含点，小写）。见设计文档第三节"索引侧变化"。
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp"];

/// 单张图片体积上限，沿用文本抽取的 20MB 预算（extract.rs::MAX_FILE_BYTES 同值，
/// 两边独立定义是因为图片和文本走的是两条完全不同的管线，没必要为了共享一个常量
/// 硬把 extract.rs 的私有常量拉成 pub(crate)）。
pub(crate) const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 这个文件是否落在 OCR 管线的处理范围内（只看扩展名，不读文件，跟
/// extract.rs::is_extractable 是同款设计：启动对账拿它做便宜的预过滤）。
pub(crate) fn is_image(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    IMAGE_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

/// 一个字符是否算 CJK 表意文字。只覆盖常用汉字区、扩展 A 区、兼容表意文字区——
/// 清洗规则只关心"两侧是不是汉字"，没必要连生僻的补充平面区块也考虑进来。
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF)
}

/// 清洗 OCR 原始输出：Windows OCR 引擎习惯在每个词/字之间插一个空格，包括中文
/// 单字之间（实测 `test_zh.png` 识别结果是 "分 布 式 限 流 器 ..."，逐字带空格）。
/// 只删"两侧都是 CJK 字符"的空格，把它还原成 "分布式限流器"；英文单词间的空格
/// （"milestone 4 spike"）必须保留，删了英文就连成一坨读不出来、也搜不到了。
pub(crate) fn clean_cjk_spaces(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' {
            let prev = out.chars().last();
            let next = chars.get(i + 1).copied();
            if let (Some(p), Some(n)) = (prev, next)
                && is_cjk(p)
                && is_cjk(n)
            {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// 双形态入索引内容：OCR 原始输出 + 去 CJK 间空格拼接后的形态一起写进 content，
/// 用空间换召回——左右结构汉字偶发拆字（池→氵也）之类的系统性误差没法自动纠正，
/// 两种形态哪个能命中都算搜到。清洗后跟原文一模一样（纯英文场景，没有 CJK 间空格
/// 可删）时不重复堆一份，省一点索引体积。
///
/// 空输入（识别不出任何文字）返回空字符串，调用方按"已处理但无文字"记录，不重试。
pub(crate) fn dual_form_content(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    let cleaned = clean_cjk_spaces(raw);
    if cleaned == raw {
        raw.to_string()
    } else {
        format!("{raw}\n{cleaned}")
    }
}

#[cfg(windows)]
mod engine_impl {
    use std::path::{Path, PathBuf};

    use windows::Graphics::Imaging::{BitmapDecoder, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::{FileAccessMode, StorageFile};
    use windows::core::{HSTRING, RuntimeType};
    use windows_future::IAsyncOperation;

    /// 用 pollster 驱动 WinRT 的 IAsyncOperation，不用 spike 里验证过的 busy-poll。
    /// windows-future 0.3 给 IAsyncOperation<T> 实现了可以直接喂给 pollster::block_on
    /// 的 Future——已经在独立 spike 里验证过这个调用链能跑通并识别出正确文字。
    fn block_on<T: RuntimeType + 'static>(
        op: windows::core::Result<IAsyncOperation<T>>,
    ) -> windows::core::Result<T> {
        pollster::block_on(op?)
    }

    /// 探测系统是否有任何可用的 OCR 语言包。管线启动前调一次；没有就整体停用，
    /// 不崩溃（见设计文档"降级与错误处理"一节）。
    pub fn is_available() -> bool {
        OcrEngine::TryCreateFromUserProfileLanguages().is_ok()
    }

    /// 一个不跨线程共享的 OCR 引擎句柄。worker 池每个线程各自创建一份，绝不共享
    /// 同一个实例——OcrEngine 和 SoftwareBitmap 都不是线程安全的（设计文档第二节）。
    pub struct Engine(OcrEngine);

    /// 创建引擎：用 TryCreateFromUserProfileLanguages 而不是精确语言标签匹配——
    /// spike 已验证标签写死 "zh-Hans" 会因为系统实际标签是 "zh-Hans-CN" 而匹配失败。
    pub fn create_engine() -> anyhow::Result<Engine> {
        OcrEngine::TryCreateFromUserProfileLanguages()
            .map(Engine)
            .map_err(|e| anyhow::anyhow!("创建 OCR 引擎失败（可能没有任何语言包）: {e}"))
    }

    /// `StorageFile::GetFileFromPathAsync` 不认 `\\?\` 扩展长度前缀路径——
    /// `Path::canonicalize()` 在 Windows 上产出的正是这种前缀路径（dowse-cli
    /// 的 `Command::Index` 就这么调），传给它会报"指定的路径过长"，其实不是真的
    /// 路径太长，是 WinRT 的路径解析根本不接受这种 Win32 扩展前缀语法。真机验证
    /// 时用 `dowse index` 建索引直接踩中了这个坑（三张图全部识别失败），这里剥掉
    /// 前缀、退回普通 Win32 路径语法再喂给它。
    ///
    /// 剥前缀的规则跟 `lib.rs::display_path` 完全一样（那边是给展示层用），
    /// 这里直接委托过去，不重复一份同样的字符串处理逻辑。
    fn strip_extended_prefix(path: &Path) -> PathBuf {
        PathBuf::from(crate::display_path(&path.to_string_lossy()))
    }

    fn load_software_bitmap(path: &Path) -> windows::core::Result<SoftwareBitmap> {
        let normalized = strip_extended_prefix(path);
        let hpath = HSTRING::from(normalized.as_os_str());
        let file = block_on(StorageFile::GetFileFromPathAsync(&hpath))?;
        let stream = block_on(file.OpenAsync(FileAccessMode::Read))?;
        let decoder = block_on(BitmapDecoder::CreateAsync(&stream))?;
        block_on(decoder.GetSoftwareBitmapAsync())
    }

    /// 对一张图片跑 OCR，返回原始识别文本（未清洗，调用方自己过 dual_form_content）。
    pub fn recognize(engine: &Engine, path: &Path) -> anyhow::Result<String> {
        let bitmap = load_software_bitmap(path)
            .map_err(|e| anyhow::anyhow!("加载图片失败 {}: {e}", path.display()))?;
        let result = block_on(engine.0.RecognizeAsync(&bitmap))
            .map_err(|e| anyhow::anyhow!("OCR 识别失败 {}: {e}", path.display()))?;
        let text = result.Text()?.to_string();
        eprintln!(
            "[诊断][recognize] 文件={:?} 文本字节数={} 行数={} 线程={:?}",
            path.file_name().unwrap_or_default(),
            text.len(),
            text.lines().count(),
            std::thread::current().id()
        );
        Ok(text)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn strip_extended_prefix_removes_drive_prefix() {
            assert_eq!(
                strip_extended_prefix(Path::new(r"\\?\C:\a\b.png")),
                PathBuf::from(r"C:\a\b.png")
            );
        }

        #[test]
        fn strip_extended_prefix_converts_unc_form() {
            assert_eq!(
                strip_extended_prefix(Path::new(r"\\?\UNC\server\share\a.png")),
                PathBuf::from(r"\\server\share\a.png")
            );
        }

        #[test]
        fn strip_extended_prefix_leaves_normal_paths_untouched() {
            assert_eq!(
                strip_extended_prefix(Path::new(r"C:\a\b.png")),
                PathBuf::from(r"C:\a\b.png")
            );
        }
    }
}

#[cfg(not(windows))]
mod engine_impl {
    use std::path::Path;

    /// 非 Windows 平台的桩实现：OCR 管线整个不可用。dowse 是 Windows 专用工具，
    /// 这里存在只是让 dowse-core 在非 Windows 平台上也能 `cargo check` 过，
    /// 不需要在每个调用点散布额外的 cfg 分支。
    pub fn is_available() -> bool {
        false
    }

    pub struct Engine;

    pub fn create_engine() -> anyhow::Result<Engine> {
        anyhow::bail!("OCR 仅支持 Windows")
    }

    pub fn recognize(_engine: &Engine, _path: &Path) -> anyhow::Result<String> {
        anyhow::bail!("OCR 仅支持 Windows")
    }
}

/// 探测系统是否有可用的 OCR 语言包。管线整体启用与否的开关，UI 侧也可以拿它
/// 提前决定要不要显示"OCR 不可用"的引导文案。
pub fn is_available() -> bool {
    engine_impl::is_available()
}

pub(crate) use engine_impl::{create_engine, recognize};

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn is_image_matches_supported_extensions_case_insensitively() {
        assert!(is_image(Path::new("shot.png")));
        assert!(is_image(Path::new("shot.PNG")));
        assert!(is_image(Path::new("photo.JPEG")));
        assert!(is_image(Path::new("a.webp")));
        assert!(is_image(Path::new("a.bmp")));
        assert!(!is_image(Path::new("note.txt")));
        assert!(!is_image(Path::new("noext")));
    }

    /// 纯中文场景：OCR 逐字输出、字间带空格（实测 test_zh.png 的真实样本），
    /// 清洗后应该完全贴回不带空格的连续汉字。
    #[test]
    fn clean_cjk_spaces_removes_spaces_between_cjk_characters() {
        let raw = "分 布 式 限 流 器 的 令 牌 桶 实 现";
        assert_eq!(clean_cjk_spaces(raw), "分布式限流器的令牌桶实现");
    }

    /// 中英混排场景（实测 test_mixed.png 的真实样本）：CJK-CJK 间的空格删掉，
    /// 英文单词之间、以及英文和 CJK 交界处的空格必须保留，否则英文单词会连成一坨。
    #[test]
    fn clean_cjk_spaces_keeps_spaces_touching_latin_text() {
        let raw = "Rust 调 用 Windows.Media.Ocr API OcrEngine.TryCreateFromLanguage(zh-Hans) 截 图 文 字 进 索 引 milestone 4 spike test";
        let cleaned = clean_cjk_spaces(raw);
        assert_eq!(
            cleaned,
            "Rust 调用 Windows.Media.Ocr API OcrEngine.TryCreateFromLanguage(zh-Hans) 截图文字进索引 milestone 4 spike test"
        );
    }

    /// 纯英文场景：不存在 CJK-CJK 间的空格，清洗应该是恒等变换。
    #[test]
    fn clean_cjk_spaces_is_identity_for_pure_latin_text() {
        let raw = "milestone 4 spike test with multiple english words";
        assert_eq!(clean_cjk_spaces(raw), raw);
    }

    #[test]
    fn dual_form_content_stacks_raw_and_cleaned_when_they_differ() {
        let raw = "分 布 式 限 流 器";
        let content = dual_form_content(raw);
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines, vec!["分 布 式 限 流 器", "分布式限流器"]);
    }

    #[test]
    fn dual_form_content_does_not_duplicate_when_cleaning_is_a_no_op() {
        let raw = "milestone 4 spike test";
        assert_eq!(dual_form_content(raw), raw);
    }

    #[test]
    fn dual_form_content_of_blank_text_is_empty() {
        assert_eq!(dual_form_content("   \n  "), "");
        assert_eq!(dual_form_content(""), "");
    }
}
