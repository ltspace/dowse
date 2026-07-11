use std::collections::HashMap;
use std::path::PathBuf;

use crate::frn_table::{FrnEntry, FrnTable};

/// USN 记录里的 Reason 位——照抄 Win32 USN_REASON_* 的语义,但不依赖 windows crate
/// 的类型,让这个模块在任何平台上都能编译、测试（设计文档"USN 事件源"一节
/// 点名的坑要能在 CI 上跑纯逻辑单测,不能绑定 Windows 才能编译）。
/// 只挑翻译逻辑关心的几个位,协议里其余的位（DATA_EXTEND、COMPRESSION_CHANGE
/// 等）统一归为"内容/属性变更"，见 [`translate_record`] 的兜底分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct UsnReason {
    pub file_create: bool,
    pub file_delete: bool,
    pub rename_old_name: bool,
    pub rename_new_name: bool,
}

/// 一条归一化后的 USN 记录：只留翻译逻辑需要的字段，剥掉 Win32 结构体的
/// 内存布局细节（RecordLength/MajorVersion 等）。平台层解析完原始缓冲区后
/// 转成这个类型再喂给 [`translate_record`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsnRecord {
    pub usn: i64,
    pub frn: u64,
    pub parent_frn: u64,
    pub name: String,
    pub is_dir: bool,
    pub reason: UsnReason,
}

const USN_REASON_FILE_CREATE: u32 = 0x100;
const USN_REASON_FILE_DELETE: u32 = 0x200;
const USN_REASON_RENAME_OLD_NAME: u32 = 0x1000;
const USN_REASON_RENAME_NEW_NAME: u32 = 0x2000;
const FILE_ATTRIBUTE_DIRECTORY_BIT: u32 = 0x10;

/// 解析一条原始 USN_RECORD_V2 字节切片（调用方已经用记录自带的 RecordLength
/// 字段把这条记录从 DeviceIoControl 写回的裸缓冲区里切出来）。
///
/// 纯字节解析，不依赖 windows crate 的结构体定义/内存对齐假设——手动按固定
/// 偏移读每个字段，这样最容易出 off-by-one 的解析逻辑能在任何平台上单测
/// （用合成的字节数组构造测试用例，不需要真的调 Win32 API）。mft.rs（MFT
/// 枚举）和 usn.rs（USN Journal 读取/游标补账）共用这一份解析，两处的裸缓冲区
/// 格式是同一个 USN_RECORD_V2。
///
/// 布局（USN_RECORD_V2，小端）：
/// `RecordLength:u32 MajorVersion:u16 MinorVersion:u16 FileReferenceNumber:u64
/// ParentFileReferenceNumber:u64 Usn:i64 TimeStamp:i64 Reason:u32
/// SourceInfo:u32 SecurityId:u32 FileAttributes:u32 FileNameLength:u16
/// FileNameOffset:u16 FileName:[u16]`（ReFS 的 V3/V4 用 128 位 FRN，
/// 本里程碑明确不支持 ReFS，不处理）。
pub(crate) fn parse_usn_record_v2_bytes(record: &[u8]) -> Option<UsnRecord> {
    if record.len() < 60 {
        return None;
    }
    let frn = u64::from_ne_bytes(record[8..16].try_into().ok()?);
    let parent_frn = u64::from_ne_bytes(record[16..24].try_into().ok()?);
    let usn = i64::from_ne_bytes(record[24..32].try_into().ok()?);
    let reason_bits = u32::from_ne_bytes(record[40..44].try_into().ok()?);
    let file_attributes = u32::from_ne_bytes(record[52..56].try_into().ok()?);
    let name_length = u16::from_ne_bytes(record[56..58].try_into().ok()?) as usize;
    let name_offset = u16::from_ne_bytes(record[58..60].try_into().ok()?) as usize;

    if name_offset.checked_add(name_length)? > record.len() {
        return None;
    }
    let name_bytes = &record[name_offset..name_offset + name_length];
    let name_u16: Vec<u16> = name_bytes
        .chunks_exact(2)
        .map(|c| u16::from_ne_bytes([c[0], c[1]]))
        .collect();
    let name = String::from_utf16_lossy(&name_u16);

    Some(UsnRecord {
        usn,
        frn,
        parent_frn,
        name,
        is_dir: file_attributes & FILE_ATTRIBUTE_DIRECTORY_BIT != 0,
        reason: UsnReason {
            file_create: reason_bits & USN_REASON_FILE_CREATE != 0,
            file_delete: reason_bits & USN_REASON_FILE_DELETE != 0,
            rename_old_name: reason_bits & USN_REASON_RENAME_OLD_NAME != 0,
            rename_new_name: reason_bits & USN_REASON_RENAME_NEW_NAME != 0,
        },
    })
}

/// 一条记录翻译后的结果。跟 [`crate::events::WatchEvent`] 的区别是多带了
/// `is_dir`：USN/MFT 记录自带文件属性，不用像 notify 那样临时 stat 磁盘才知道
/// 是不是目录。是不是目录决定了平台层要不要展开子树（见 usn.rs 的
/// 用法说明）——这一层不做真实文件系统 IO，只管"根据记录判断该发什么"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsnOutcome {
    Upsert {
        path: PathBuf,
        is_dir: bool,
    },
    Remove {
        path: PathBuf,
        is_dir: bool,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        to_is_dir: bool,
    },
    /// 这条记录没有可以立即产出的事件——最典型的是重命名的前半（OLD_NAME），
    /// 要等配对的 NEW_NAME 记录才知道往哪发。
    None,
}

/// 重命名配对 + FRN 路径表的状态机。一个卷一个实例（USN Journal 按卷独立）。
///
/// 核心不变式（设计文档点名的坑就是靠它防的）：**FrnTable 在每条记录处理时
/// 立刻更新到记录反映的最新状态，绝不滞后**。这样即使"重命名后紧跟删除"
/// 这种快进序列杀到，删除记录到达时表里已经是新名字/新位置，删除操作解析出
/// 的路径是对的，不会因为表还停留在旧名字上而删错地方、留下搜不掉的孤儿文档。
pub(crate) struct UsnTranslator {
    table: FrnTable,
    /// 等待配对的重命名：FRN → 旧路径。OLD_NAME 记录到达时写入，
    /// NEW_NAME 记录到达时取出配对；如果配对之前先等到这个 FRN 的删除记录
    /// （"重命名后紧跟删除"），就把它当"退回旧名的删除"处理，不吞事件。
    pending_renames: HashMap<u64, PathBuf>,
}

impl UsnTranslator {
    pub fn new(table: FrnTable) -> Self {
        Self {
            table,
            pending_renames: HashMap::new(),
        }
    }

    /// 只在测试里用来直接断言表的当前状态；生产代码（usn.rs）只需要
    /// [`UsnTranslator::into_table`] 在补账结束后把整张表接力交还。
    #[cfg(test)]
    pub fn table(&self) -> &FrnTable {
        &self.table
    }

    /// 消费掉整个翻译器，只要回它更新过的 FrnTable——游标补账用完临时的
    /// UsnTranslator 后，把接力棒（表）交还给调用方，继续传给 live 监听。
    pub fn into_table(self) -> FrnTable {
        self.table
    }

    /// 处理一条记录，返回要发的事件（可能是 None）。
    ///
    /// 路径解析失败（FrnTable 里链断了，通常是这个 FRN 不在任何监听根下，
    /// 也可能是缓存 miss）时返回 `UsnOutcome::None`——纯逻辑层不知道怎么
    /// 兜底（要开卷句柄反查，平台相关），交给调用方（usn.rs）决定要不要用
    /// FRN 反查路径后重试。
    pub fn translate(&mut self, record: UsnRecord) -> UsnOutcome {
        if record.reason.rename_old_name {
            return self.handle_rename_old_name(record);
        }
        if record.reason.rename_new_name {
            return self.handle_rename_new_name(record);
        }
        if record.reason.file_delete {
            return self.handle_delete(record);
        }
        // 创建 / 内容修改 / 属性修改，统统按 upsert 处理（跟 notify 侧
        // emit_upsert 的"先删后加天然幂等"是同一语义，见 watch.rs）。
        self.handle_upsert(record)
    }

    fn handle_upsert(&mut self, record: UsnRecord) -> UsnOutcome {
        // 重建这个 FRN 之前，如果它正巧有个悬而未决的重命名（理论上不该发生：
        // 一个 FRN 不会又是"待配对的 OLD_NAME"又同时来一条创建记录——但 FRN
        // 在文件被删除后可能被 NTFS 回收复用给全新的文件，防御性地把旧的
        // pending 丢弃，不让它污染新文件的状态。
        self.pending_renames.remove(&record.frn);

        self.table.upsert(
            record.frn,
            FrnEntry {
                parent_frn: record.parent_frn,
                name: record.name.clone(),
                is_dir: record.is_dir,
            },
        );
        match self.table.reconstruct_path(record.frn) {
            Some(path) => UsnOutcome::Upsert {
                path,
                is_dir: record.is_dir,
            },
            None => UsnOutcome::None,
        }
    }

    fn handle_rename_old_name(&mut self, record: UsnRecord) -> UsnOutcome {
        // 先按这条记录自带的位置刷新表（它反映的是重命名前的名字/父目录），
        // 再重建路径——这就是"旧路径"。
        self.table.upsert(
            record.frn,
            FrnEntry {
                parent_frn: record.parent_frn,
                name: record.name.clone(),
                is_dir: record.is_dir,
            },
        );
        if let Some(from_path) = self.table.reconstruct_path(record.frn) {
            self.pending_renames.insert(record.frn, from_path);
        }
        // OLD_NAME 单独不产出事件，等 NEW_NAME 配对。
        UsnOutcome::None
    }

    fn handle_rename_new_name(&mut self, record: UsnRecord) -> UsnOutcome {
        // 立刻把表更新到新位置——这是防"重命名后紧跟删除"吞事件/删错地方的
        // 关键一步：这行之后，任何针对这个 FRN 的后续记录（包括紧跟着的删除）
        // 都会解析到新路径，不会用到已经过期的旧路径。
        self.table.upsert(
            record.frn,
            FrnEntry {
                parent_frn: record.parent_frn,
                name: record.name.clone(),
                is_dir: record.is_dir,
            },
        );
        let Some(to_path) = self.table.reconstruct_path(record.frn) else {
            // 新位置解析不出来（比如移出了所有监听根）：清掉悬挂的 pending，
            // 旧名那一侧当作删除处理——文件确实离开了监听范围。
            if let Some(from_path) = self.pending_renames.remove(&record.frn) {
                return UsnOutcome::Remove {
                    path: from_path,
                    is_dir: record.is_dir,
                };
            }
            return UsnOutcome::None;
        };

        match self.pending_renames.remove(&record.frn) {
            Some(from_path) => {
                if from_path == to_path {
                    // 理论上不该发生（改名总该改点什么），但万一发生，
                    // 当无事发生处理，别产出一个 from==to 的假重命名。
                    return UsnOutcome::None;
                }
                UsnOutcome::Rename {
                    from: from_path,
                    to: to_path,
                    to_is_dir: record.is_dir,
                }
            }
            // 没有配对的 OLD_NAME：要么是刚开始监听时错过了前半（比如 USN 源
            // 启动时游标正好卡在两条记录中间），要么是极端情况下顺序被打乱。
            // 尽力而为，当新增处理——跟 notify 侧 emit_best_effort 的"分不清
            // 就按能确定的那部分处理"是同一原则。
            None => UsnOutcome::Upsert {
                path: to_path,
                is_dir: record.is_dir,
            },
        }
    }

    fn handle_delete(&mut self, record: UsnRecord) -> UsnOutcome {
        // 这就是设计文档点名的坑："同一 FRN 上重命名后紧跟删除"：OLD_NAME
        // 记录来过、NEW_NAME 还没来，删除记录就到了。用配对时记下的旧路径
        // （唯一确凿已知的位置）当作删除对象，不能凭空丢掉这个事件。
        if let Some(from_path) = self.pending_renames.remove(&record.frn) {
            self.table.remove(record.frn);
            return UsnOutcome::Remove {
                path: from_path,
                is_dir: record.is_dir,
            };
        }

        // 正常路径：用删除记录自带的信息刷新一次表（防止表本来就没同步过），
        // 拿到路径后再整个移除。
        self.table.upsert(
            record.frn,
            FrnEntry {
                parent_frn: record.parent_frn,
                name: record.name.clone(),
                is_dir: record.is_dir,
            },
        );
        let path = self.table.reconstruct_path(record.frn);
        self.table.remove(record.frn);
        match path {
            Some(path) => UsnOutcome::Remove {
                path,
                is_dir: record.is_dir,
            },
            None => UsnOutcome::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按 USN_RECORD_V2 的真实字节布局手工拼一条合成记录，用来测字节解析层——
    /// 不依赖 Windows，纯粹验证偏移量/字节序算对了。
    fn build_usn_record_v2_bytes(
        frn: u64,
        parent_frn: u64,
        usn: i64,
        reason_bits: u32,
        file_attributes: u32,
        name: &str,
    ) -> Vec<u8> {
        let name_u16: Vec<u16> = name.encode_utf16().collect();
        let name_bytes_len = name_u16.len() * 2;
        let name_offset = 60u16;
        let record_length = name_offset as usize + name_bytes_len;

        let mut buf = Vec::with_capacity(record_length);
        buf.extend_from_slice(&(record_length as u32).to_ne_bytes()); // RecordLength
        buf.extend_from_slice(&2u16.to_ne_bytes()); // MajorVersion
        buf.extend_from_slice(&0u16.to_ne_bytes()); // MinorVersion
        buf.extend_from_slice(&frn.to_ne_bytes());
        buf.extend_from_slice(&parent_frn.to_ne_bytes());
        buf.extend_from_slice(&usn.to_ne_bytes());
        buf.extend_from_slice(&0i64.to_ne_bytes()); // TimeStamp，翻译层不关心
        buf.extend_from_slice(&reason_bits.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes()); // SourceInfo
        buf.extend_from_slice(&0u32.to_ne_bytes()); // SecurityId
        buf.extend_from_slice(&file_attributes.to_ne_bytes());
        buf.extend_from_slice(&(name_bytes_len as u16).to_ne_bytes()); // FileNameLength
        buf.extend_from_slice(&name_offset.to_ne_bytes()); // FileNameOffset
        for unit in &name_u16 {
            buf.extend_from_slice(&unit.to_ne_bytes());
        }
        assert_eq!(
            buf.len(),
            record_length,
            "构造的字节数应该跟算出的 RecordLength 一致"
        );
        buf
    }

    #[test]
    fn parses_plain_file_record() {
        let bytes = build_usn_record_v2_bytes(42, 1, 999, USN_REASON_FILE_CREATE, 0, "a.md");
        let record = parse_usn_record_v2_bytes(&bytes).expect("应该解析成功");
        assert_eq!(record.frn, 42);
        assert_eq!(record.parent_frn, 1);
        assert_eq!(record.usn, 999);
        assert_eq!(record.name, "a.md");
        assert!(!record.is_dir);
        assert!(record.reason.file_create);
        assert!(!record.reason.file_delete);
    }

    #[test]
    fn parses_directory_attribute_flag() {
        let bytes = build_usn_record_v2_bytes(
            7,
            1,
            1,
            USN_REASON_FILE_CREATE,
            FILE_ATTRIBUTE_DIRECTORY_BIT,
            "sub",
        );
        let record = parse_usn_record_v2_bytes(&bytes).expect("应该解析成功");
        assert!(record.is_dir);
    }

    #[test]
    fn parses_rename_reason_bits() {
        let bytes = build_usn_record_v2_bytes(7, 1, 5, USN_REASON_RENAME_OLD_NAME, 0, "old.md");
        let record = parse_usn_record_v2_bytes(&bytes).expect("应该解析成功");
        assert!(record.reason.rename_old_name);
        assert!(!record.reason.rename_new_name);

        let bytes2 = build_usn_record_v2_bytes(7, 1, 6, USN_REASON_RENAME_NEW_NAME, 0, "new.md");
        let record2 = parse_usn_record_v2_bytes(&bytes2).expect("应该解析成功");
        assert!(record2.reason.rename_new_name);
    }

    #[test]
    fn parses_cjk_filename_correctly() {
        let bytes =
            build_usn_record_v2_bytes(9, 1, 1, USN_REASON_FILE_CREATE, 0, "分布式限流器.md");
        let record = parse_usn_record_v2_bytes(&bytes).expect("应该解析成功");
        assert_eq!(record.name, "分布式限流器.md");
    }

    #[test]
    fn truncated_record_shorter_than_fixed_header_fails_gracefully() {
        let bytes = vec![0u8; 30]; // 不到 60 字节的固定头
        assert_eq!(parse_usn_record_v2_bytes(&bytes), None);
    }

    #[test]
    fn name_offset_beyond_buffer_fails_gracefully_instead_of_panicking() {
        let mut bytes = build_usn_record_v2_bytes(1, 1, 1, USN_REASON_FILE_CREATE, 0, "a.md");
        // 篡改 FileNameOffset，让它指向缓冲区之外。
        bytes[58..60].copy_from_slice(&5000u16.to_ne_bytes());
        assert_eq!(parse_usn_record_v2_bytes(&bytes), None);
    }

    fn root_path() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\watch")
        } else {
            PathBuf::from("/watch")
        }
    }

    fn translator_with_root() -> (UsnTranslator, u64) {
        let root_frn = 1;
        let mut table = FrnTable::new();
        table.register_root(root_frn, root_path());
        (UsnTranslator::new(table), root_frn)
    }

    fn reason(f: impl FnOnce(&mut UsnReason)) -> UsnReason {
        let mut r = UsnReason::default();
        f(&mut r);
        r
    }

    #[test]
    fn create_record_emits_upsert_with_is_dir_flag() {
        let (mut t, root) = translator_with_root();
        let outcome = t.translate(UsnRecord {
            usn: 10,
            frn: 2,
            parent_frn: root,
            name: "a.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        assert_eq!(
            outcome,
            UsnOutcome::Upsert {
                path: root_path().join("a.md"),
                is_dir: false,
            }
        );
    }

    #[test]
    fn plain_content_change_without_reason_bits_still_upserts() {
        // 内容/属性修改记录常常只带 DATA_EXTEND 之类我们没建模的位，
        // 翻译层的兜底分支应该照样当 upsert 处理。
        let (mut t, root) = translator_with_root();
        let outcome = t.translate(UsnRecord {
            usn: 5,
            frn: 2,
            parent_frn: root,
            name: "a.md".to_string(),
            is_dir: false,
            reason: UsnReason::default(),
        });
        assert_eq!(
            outcome,
            UsnOutcome::Upsert {
                path: root_path().join("a.md"),
                is_dir: false,
            }
        );
    }

    #[test]
    fn delete_record_emits_remove_and_forgets_frn() {
        let (mut t, root) = translator_with_root();
        t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "a.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        let outcome = t.translate(UsnRecord {
            usn: 2,
            frn: 2,
            parent_frn: root,
            name: "a.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_delete = true),
        });
        assert_eq!(
            outcome,
            UsnOutcome::Remove {
                path: root_path().join("a.md"),
                is_dir: false,
            }
        );
        assert!(t.table().get(2).is_none());
    }

    #[test]
    fn directory_delete_flags_is_dir_true() {
        let (mut t, root) = translator_with_root();
        t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "sub".to_string(),
            is_dir: true,
            reason: reason(|r| r.file_create = true),
        });
        let outcome = t.translate(UsnRecord {
            usn: 2,
            frn: 2,
            parent_frn: root,
            name: "sub".to_string(),
            is_dir: true,
            reason: reason(|r| r.file_delete = true),
        });
        assert_eq!(
            outcome,
            UsnOutcome::Remove {
                path: root_path().join("sub"),
                is_dir: true,
            }
        );
    }

    #[test]
    fn rename_old_name_alone_produces_no_event() {
        let (mut t, root) = translator_with_root();
        t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        let outcome = t.translate(UsnRecord {
            usn: 2,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_old_name = true),
        });
        assert_eq!(outcome, UsnOutcome::None);
    }

    #[test]
    fn old_name_then_new_name_pairs_into_rename_event() {
        let (mut t, root) = translator_with_root();
        t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        t.translate(UsnRecord {
            usn: 2,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_old_name = true),
        });
        let outcome = t.translate(UsnRecord {
            usn: 3,
            frn: 2,
            parent_frn: root,
            name: "new.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_new_name = true),
        });
        assert_eq!(
            outcome,
            UsnOutcome::Rename {
                from: root_path().join("old.md"),
                to: root_path().join("new.md"),
                to_is_dir: false,
            }
        );
        // 表已经指向新名字
        assert_eq!(
            t.table().reconstruct_path(2),
            Some(root_path().join("new.md"))
        );
    }

    /// 设计文档点名的坑："同一 FRN 上重命名后紧跟删除"：OLD_NAME 记录来过，
    /// 还没等到 NEW_NAME，删除记录就到了。终态要求：旧名新名都搜不到——
    /// 也就是这里必须真正产出一个 Remove（对旧名），不能因为"配对没完成"
    /// 就把这条记录悄悄吞掉。
    #[test]
    fn rename_immediately_followed_by_delete_before_pairing_completes_is_not_swallowed() {
        let (mut t, root) = translator_with_root();
        t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        let rename_outcome = t.translate(UsnRecord {
            usn: 2,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_old_name = true),
        });
        assert_eq!(rename_outcome, UsnOutcome::None, "OLD_NAME 本身不产出事件");

        // NEW_NAME 还没到，删除记录先到了。
        let delete_outcome = t.translate(UsnRecord {
            usn: 3,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(), // 删除记录本身的 name 字段在这种交错场景下不可信，翻译层应该用配对记下的旧路径
            is_dir: false,
            reason: reason(|r| r.file_delete = true),
        });
        assert_eq!(
            delete_outcome,
            UsnOutcome::Remove {
                path: root_path().join("old.md"),
                is_dir: false,
            },
            "配对未完成时被删除，必须退回用旧路径发一次 Remove，不能吞事件"
        );
        assert!(t.table().get(2).is_none(), "FRN 应该被彻底清出表");

        // 如果 NEW_NAME 记录后来仍然（乱序）姗姗来迟，不应该复活一个已经
        // 删除的文件——pending 已经被上面的删除清空，NEW_NAME 找不到配对，
        // 只能按"尽力而为"当独立的 upsert 处理（这是已知的极端乱序情形，
        // 翻译层不吞事件，交回给上层的幂等 upsert/delete 语义去兜底）。
        let late_new_name = t.translate(UsnRecord {
            usn: 4,
            frn: 2,
            parent_frn: root,
            name: "new.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_new_name = true),
        });
        assert_eq!(
            late_new_name,
            UsnOutcome::Upsert {
                path: root_path().join("new.md"),
                is_dir: false,
            }
        );
    }

    /// 快速连续"改名→再删除已完成配对的新名字"：这条路径不该触发 pitfall
    /// 分支（pending 已经在配对时清空），必须解析到新路径而不是旧路径——
    /// 这正是 handle_rename_new_name 里"立刻更新表"这一步要防的退化情形。
    #[test]
    fn rename_completes_then_delete_targets_the_new_path_not_the_old_one() {
        let (mut t, root) = translator_with_root();
        t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        t.translate(UsnRecord {
            usn: 2,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_old_name = true),
        });
        let rename = t.translate(UsnRecord {
            usn: 3,
            frn: 2,
            parent_frn: root,
            name: "new.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_new_name = true),
        });
        assert_eq!(
            rename,
            UsnOutcome::Rename {
                from: root_path().join("old.md"),
                to: root_path().join("new.md"),
                to_is_dir: false,
            }
        );

        let delete = t.translate(UsnRecord {
            usn: 4,
            frn: 2,
            parent_frn: root,
            name: "new.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_delete = true),
        });
        assert_eq!(
            delete,
            UsnOutcome::Remove {
                path: root_path().join("new.md"),
                is_dir: false,
            },
            "配对已完成，删除必须对准新路径，不能残留指向旧路径的孤儿文档"
        );
    }

    /// 交错序列：两个不相关文件的重命名操作的记录在日志里穿插到一起
    /// （FRN A 的 OLD_NAME、FRN B 的 OLD_NAME、FRN A 的 NEW_NAME、
    /// FRN B 的 NEW_NAME），配对必须按各自的 FRN 独立完成，不能互相串味。
    #[test]
    fn interleaved_renames_of_two_different_frns_pair_independently() {
        let (mut t, root) = translator_with_root();
        t.translate(UsnRecord {
            usn: 1,
            frn: 10,
            parent_frn: root,
            name: "a-old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        t.translate(UsnRecord {
            usn: 2,
            frn: 20,
            parent_frn: root,
            name: "b-old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });

        // A 的 OLD_NAME
        t.translate(UsnRecord {
            usn: 3,
            frn: 10,
            parent_frn: root,
            name: "a-old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_old_name = true),
        });
        // B 的 OLD_NAME 插进来
        t.translate(UsnRecord {
            usn: 4,
            frn: 20,
            parent_frn: root,
            name: "b-old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_old_name = true),
        });
        // A 的 NEW_NAME 先配对完成
        let a_rename = t.translate(UsnRecord {
            usn: 5,
            frn: 10,
            parent_frn: root,
            name: "a-new.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_new_name = true),
        });
        // B 的 NEW_NAME 后到
        let b_rename = t.translate(UsnRecord {
            usn: 6,
            frn: 20,
            parent_frn: root,
            name: "b-new.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_new_name = true),
        });

        assert_eq!(
            a_rename,
            UsnOutcome::Rename {
                from: root_path().join("a-old.md"),
                to: root_path().join("a-new.md"),
                to_is_dir: false,
            }
        );
        assert_eq!(
            b_rename,
            UsnOutcome::Rename {
                from: root_path().join("b-old.md"),
                to: root_path().join("b-new.md"),
                to_is_dir: false,
            }
        );
    }

    #[test]
    fn new_name_without_prior_old_name_is_best_effort_upsert() {
        // 模拟从游标中间接上日志：只看到 NEW_NAME，没看到配对的 OLD_NAME。
        let (mut t, root) = translator_with_root();
        let outcome = t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "new.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_new_name = true),
        });
        assert_eq!(
            outcome,
            UsnOutcome::Upsert {
                path: root_path().join("new.md"),
                is_dir: false,
            }
        );
    }

    #[test]
    fn rename_moving_out_of_scope_resolves_to_remove_of_old_path() {
        // 新位置在任何监听根之外（parent_frn 是个陌生 FRN，重建不出路径）：
        // 旧名那一侧当删除处理，等价于 events.rs 里 Debouncer 对
        // "只有旧名在监听内" 的处理语义。
        let (mut t, root) = translator_with_root();
        t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        t.translate(UsnRecord {
            usn: 2,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_old_name = true),
        });
        let outcome = t.translate(UsnRecord {
            usn: 3,
            frn: 2,
            parent_frn: 9999, // 陌生父 FRN，解析不出监听范围内的路径
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_new_name = true),
        });
        assert_eq!(
            outcome,
            UsnOutcome::Remove {
                path: root_path().join("old.md"),
                is_dir: false,
            }
        );
    }

    #[test]
    fn frn_reused_after_delete_does_not_leak_stale_pending_rename() {
        let (mut t, root) = translator_with_root();
        // FRN 2 先经历一次"改名到一半就没等到 NEW_NAME"（留下 pending）。
        t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        t.translate(UsnRecord {
            usn: 2,
            frn: 2,
            parent_frn: root,
            name: "old.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_old_name = true),
        });
        // 这里不发 NEW_NAME，模拟 NTFS 把同一个 FRN 编号回收复用给全新文件
        // （现实中序列号会变，这里只用来验证 pending 不会污染新记录）。
        let outcome = t.translate(UsnRecord {
            usn: 3,
            frn: 2,
            parent_frn: root,
            name: "brand-new.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        assert_eq!(
            outcome,
            UsnOutcome::Upsert {
                path: root_path().join("brand-new.md"),
                is_dir: false,
            }
        );
        // 之后这个 FRN 被删除，应该删的是 brand-new.md，不是残留的 old.md pending。
        let delete = t.translate(UsnRecord {
            usn: 4,
            frn: 2,
            parent_frn: root,
            name: "brand-new.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_delete = true),
        });
        assert_eq!(
            delete,
            UsnOutcome::Remove {
                path: root_path().join("brand-new.md"),
                is_dir: false,
            }
        );
    }

    #[test]
    fn rename_to_identical_path_is_a_no_op() {
        let (mut t, root) = translator_with_root();
        t.translate(UsnRecord {
            usn: 1,
            frn: 2,
            parent_frn: root,
            name: "same.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.file_create = true),
        });
        t.translate(UsnRecord {
            usn: 2,
            frn: 2,
            parent_frn: root,
            name: "same.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_old_name = true),
        });
        let outcome = t.translate(UsnRecord {
            usn: 3,
            frn: 2,
            parent_frn: root,
            name: "same.md".to_string(),
            is_dir: false,
            reason: reason(|r| r.rename_new_name = true),
        });
        assert_eq!(outcome, UsnOutcome::None);
    }
}
