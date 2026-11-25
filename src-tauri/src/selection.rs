//! 可选划词搜索：优先通过 UI Automation 只读选区，Ctrl+C 仅为显式回退。
//!
//! 回退不会清空图片、文件、富文本或自定义格式剪贴板；仅快照并恢复系统文本格式。

use serde::Serialize;

#[cfg(windows)]
use windows::{
    core::{Free, GUID, IUnknown, PCWSTR},
    Win32::{
        Foundation::{GetLastError, HANDLE, HGLOBAL, HWND, WIN32_ERROR},
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            DataExchange::{
                CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
                GetClipboardOwner, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE},
            Ole::{CF_LOCALE, CF_OEMTEXT, CF_TEXT, CF_UNICODETEXT},
        },
        UI::{
            Accessibility::{
                IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
            },
            Input::KeyboardAndMouse::{
                SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_C,
                VK_CONTROL,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, GetAncestor, GetForegroundWindow, HWND_MESSAGE,
                GA_ROOT, WS_OVERLAPPED,
            },
        },
    },
};

/// 避免误把整篇文档或超长段落送入查询与界面。
pub const MAX_SELECTION_CHARS: usize = 2000;

#[derive(Debug, Clone, Serialize)]
pub struct CaptureResult {
    pub text: String,
    /// 单调递增请求号；前端用它丢弃重复或乱流的旧划词结果。
    pub request_id: u64,
    /// "uia" 或 "clipboard"，仅用于诊断，不包含选区内容。
    pub method: &'static str,
    pub warning: Option<String>,
}

/// 压缩空白并限制长度；保留 @、+/-、冒号等字符，由字面量检索解释。
pub fn normalize_selection(raw: &str) -> Option<String> {
    let mut out = String::new();
    for word in raw.split_whitespace() {
        let clean: String = word
            .chars()
            .filter(|c| !c.is_control() || c.is_whitespace())
            .collect();
        if clean.is_empty() {
            continue;
        }
        let mut remaining = MAX_SELECTION_CHARS.saturating_sub(out.chars().count());
        if remaining == 0 {
            break;
        }
        if !out.is_empty() {
            if remaining <= 1 {
                break;
            }
            out.push(' ');
            remaining -= 1;
        }
        for c in clean.chars().take(remaining) {
            out.push(c);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// UIA 无选区或应用不支持 TextPattern 时返回空串；错误只用于选择回退路径。
pub fn capture(allow_clipboard_fallback: bool) -> Result<CaptureResult, String> {
    #[cfg(windows)]
    {
        let uia_error = match capture_uia() {
            Ok(raw) => {
                if let Some(text) = normalize_selection(&raw) {
                    return Ok(CaptureResult { text, request_id: 0, method: "uia", warning: None });
                }
                None
            }
            Err(e) => Some(e),
        };

        if !allow_clipboard_fallback {
            if let Some(e) = uia_error {
                crate::kdlog!(warn, "selection", "UI Automation 读取选区失败: {e}");
            }
            return Err("未读到文本选区；可在设置中显式启用 Ctrl+C 回退".into());
        }

        match capture_clipboard() {
            Ok(result) => Ok(result),
            Err(e) => {
                crate::kdlog!(warn, "selection", "Ctrl+C 回退失败: {e}");
                Err(e)
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = allow_clipboard_fallback;
        Err("划词搜索仅支持 Windows".into())
    }
}

#[cfg(windows)]
struct ComApartment {
    initialized: bool,
}

#[cfg(windows)]
impl ComApartment {
    fn new() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Self { initialized: true })
        } else {
            Err(format!("COM 初始化失败: {}", hr.message()))
        }
    }
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(windows)]
fn capture_uia() -> Result<String, String> {
    let _apartment = ComApartment::new()?;
    // CUIAutomation 的 CLSID；当前 windows 绑定未导出该常量。
    let clsid = GUID::from_u128(0xff48dba4_60ef_4201_aa87_54103eef594e);
    let automation: IUIAutomation = unsafe {
        CoCreateInstance(&clsid, None::<&IUnknown>, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("创建 UI Automation 失败: {e}"))?
    };
    let element = unsafe { automation.GetFocusedElement() }
        .map_err(|e| format!("读取焦点元素失败: {e}"))?;
    let pattern: IUIAutomationTextPattern = unsafe {
        element
            .GetCurrentPatternAs(UIA_TextPatternId)
            .map_err(|e| format!("焦点元素不支持 TextPattern: {e}"))?
    };
    let ranges = unsafe { pattern.GetSelection() }
        .map_err(|e| format!("读取选区失败: {e}"))?;
    let count = unsafe { ranges.Length() }.map_err(|e| e.to_string())?.max(0);
    let mut parts = Vec::new();
    for index in 0..count.min(32) {
        let range = unsafe { ranges.GetElement(index) }.map_err(|e| e.to_string())?;
        let text = unsafe { range.GetText(8192) }
            .map_err(|e| e.to_string())?
            .to_string();
        if !text.trim().is_empty() {
            parts.push(text);
        }
    }
    Ok(parts.join("\n"))
}

#[cfg(windows)]
struct ClipboardOwner(HWND);

#[cfg(windows)]
impl ClipboardOwner {
    fn new() -> Result<Self, String> {
        // OpenClipboard(NULL) 后 EmptyClipboard 会把 owner 置空，导致 SetClipboardData 必败。
        // 使用仅在本次采集期间存在的消息窗口，确保恢复时始终是合法 clipboard owner。
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let title: Vec<u16> = "knowledge-dock clipboard\0".encode_utf16().collect();
        let instance = unsafe { GetModuleHandleW(None) }
            .map_err(|e| format!("获取剪贴板模块句柄失败: {e}"))?;
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance.into()),
                None,
            )
        }
        .map_err(|e| format!("创建剪贴板恢复窗口失败: {e}"))?;
        if hwnd.0.is_null() {
            return Err("创建剪贴板恢复窗口失败".into());
        }
        Ok(Self(hwnd))
    }
}

#[cfg(windows)]
impl Drop for ClipboardOwner {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

#[cfg(windows)]
struct ClipboardGuard;

#[cfg(windows)]
impl ClipboardGuard {
    fn open(owner: HWND) -> Result<Self, String> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
                return Ok(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Err("剪贴板被其他程序占用".into())
    }
}

#[cfg(windows)]
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

#[cfg(windows)]
fn is_supported_text_format(format: u32) -> bool {
    matches!(
        format,
        f if f == CF_TEXT.0 as u32
            || f == CF_OEMTEXT.0 as u32
            || f == CF_UNICODETEXT.0 as u32
            || f == CF_LOCALE.0 as u32
    )
}

#[cfg(windows)]
struct ClipboardEntry {
    format: u32,
    data: Vec<u8>,
}

#[cfg(windows)]
struct ClipboardSnapshot {
    entries: Vec<ClipboardEntry>,
}

#[cfg(windows)]
struct OwnedClipboardMemory(HGLOBAL);

#[cfg(windows)]
impl Drop for OwnedClipboardMemory {
    fn drop(&mut self) {
        unsafe { self.0.free() };
    }
}

fn decode_unicode_text(data: &[u8]) -> Result<String, String> {
    if data.len() < 2 || data.len() > 16 * 1024 * 1024 || data.len() % 2 != 0 {
        return Err("Ctrl+C 后文本长度无效".into());
    }
    let words: Vec<u16> = data
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let end = words.iter().position(|word| *word == 0).unwrap_or(words.len());
    String::from_utf16(&words[..end]).map_err(|_| "Ctrl+C 文本编码无效".to_string())
}

#[cfg(windows)]
impl ClipboardSnapshot {
    fn capture(owner: HWND) -> Result<(Self, u32), String> {
        // Windows 可能在 CloseClipboard 时继续提交格式并递增序列号；只有关窗后
        // 序列号仍与快照读取时一致，才把它用作后续并发写入检测的基线。
        for _ in 0..4 {
            let _guard = ClipboardGuard::open(owner)?;
            let sequence = unsafe { GetClipboardSequenceNumber() };
            let snapshot = Self::capture_open()?;
            if unsafe { GetClipboardSequenceNumber() } != sequence {
                continue;
            }
            drop(_guard);
            if unsafe { GetClipboardSequenceNumber() } == sequence {
                return Ok((snapshot, sequence));
            }
        }
        Err("剪贴板序列在快照后持续变化，已拒绝 Ctrl+C 回退".into())
    }

    /// 调用者必须已打开剪贴板；格式枚举、数据读取与序列号在同一个 guard 内完成。
    fn capture_open() -> Result<Self, String> {
        let mut format = 0u32;
        let mut entries = Vec::new();
        for _ in 0..32 {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                let error = unsafe { GetLastError() };
                if error != WIN32_ERROR(0) {
                    return Err(format!("枚举剪贴板格式失败: {error:?}；已拒绝 Ctrl+C 回退"));
                }
                break;
            }
            if !is_supported_text_format(format) {
                return Err("剪贴板含图片、文件、富文本或自定义格式，已拒绝 Ctrl+C 回退".into());
            }
            let handle = unsafe { GetClipboardData(format) }
                .map_err(|e| format!("读取剪贴板格式 {format} 失败: {e}"))?;
            let size = unsafe { GlobalSize(HGLOBAL(handle.0)) };
            if size == 0 || size > 16 * 1024 * 1024 {
                return Err("剪贴板文本过大或为空，已拒绝 Ctrl+C 回退".into());
            }
            let ptr = unsafe { GlobalLock(HGLOBAL(handle.0)) };
            if ptr.is_null() {
                return Err("锁定剪贴板文本失败，已拒绝 Ctrl+C 回退".into());
            }
            let data = unsafe { std::slice::from_raw_parts(ptr as *const u8, size).to_vec() };
            let _ = unsafe { GlobalUnlock(HGLOBAL(handle.0)) };
            entries.push(ClipboardEntry { format, data });
        }
        if format != 0 {
            return Err("剪贴板格式过多，已拒绝 Ctrl+C 回退".into());
        }
        Ok(Self { entries })
    }

    fn unicode_text(&self) -> Result<String, String> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.format == CF_UNICODETEXT.0 as u32)
            .ok_or_else(|| "Ctrl+C 后剪贴板没有 Unicode 文本".to_string())?;
        decode_unicode_text(&entry.data)
    }

    fn allocate_restore_buffers(&self) -> Result<Vec<(u32, OwnedClipboardMemory)>, String> {
        // 调用者持有剪贴板 guard；两套完整副本分别用于首次恢复和失败后的完整重放。
        let mut prepared = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let len = entry.data.len().max(1);
            let memory = OwnedClipboardMemory(unsafe { GlobalAlloc(GMEM_MOVEABLE, len) }
                .map_err(|e| format!("恢复剪贴板分配内存失败: {e}"))?);
            if memory.0.0.is_null() {
                return Err("恢复剪贴板分配内存失败".into());
            }
            let ptr = unsafe { GlobalLock(memory.0) };
            if ptr.is_null() {
                return Err("恢复剪贴板锁定内存失败".into());
            }
            unsafe {
                std::ptr::copy_nonoverlapping(entry.data.as_ptr(), ptr as *mut u8, entry.data.len());
            }
            let _ = unsafe { GlobalUnlock(memory.0) };
            prepared.push((entry.format, memory));
        }
        Ok(prepared)
    }

    fn set_restore_buffers(
        prepared: Vec<(u32, OwnedClipboardMemory)>,
    ) -> Result<(), String> {
        for (format, memory) in prepared {
            if let Err(e) = unsafe { SetClipboardData(format, Some(HANDLE(memory.0.0))) } {
                return Err(format!("恢复剪贴板格式 {format} 失败: {e}"));
            }
            // SetClipboardData 成功后由系统接管句柄。
            std::mem::forget(memory);
        }
        Ok(())
    }

    /// 仅在剪贴板仍是本次 Ctrl+C 的结果、且仍属于原前台窗口时恢复。
    fn restore(
        &self,
        owner: HWND,
        foreground: HWND,
        expected_sequence: u32,
    ) -> Result<(), String> {
        let _guard = ClipboardGuard::open(owner)?;
        if unsafe { GetClipboardSequenceNumber() } != expected_sequence {
            return Err("剪贴板已被其他操作更新，保留最新内容".into());
        }
        if !clipboard_belongs_to_foreground(
            unsafe { GetClipboardOwner() }
                .map_err(|e| format!("读取剪贴板所有者失败: {e}"))?,
            foreground,
        ) {
            return Err("剪贴板所有者已变化，保留最新内容".into());
        }

        // 清空前先备齐主副本和备份副本，避免中途失败后丢失原剪贴板格式。
        let primary = self.allocate_restore_buffers()?;
        let backup = self.allocate_restore_buffers()?;
        unsafe { EmptyClipboard() }.map_err(|e| format!("清空剪贴板失败: {e}"))?;
        if let Err(first_error) = Self::set_restore_buffers(primary) {
            unsafe { EmptyClipboard() }.map_err(|e| {
                format!("{first_error}；恢复重试前清空剪贴板失败: {e}")
            })?;
            Self::set_restore_buffers(backup).map_err(|retry_error| {
                format!("{first_error}；备份重放也失败: {retry_error}")
            })?;
        }
        Ok(())
    }
}

#[cfg(windows)]
fn send_ctrl_c() -> Result<(), String> {
    fn key(vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
    let input = [
        key(VK_CONTROL, false),
        key(VK_C, false),
        key(VK_C, true),
        key(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == input.len() {
        Ok(())
    } else {
        Err(format!("发送 Ctrl+C 失败：仅注入 {sent}/{} 个事件", input.len()))
    }
}

#[cfg(windows)]
enum ClipboardChange {
    Owned(u32),
    Other,
    TimedOut,
}

#[cfg(windows)]
fn clipboard_belongs_to_foreground(owner: HWND, foreground: HWND) -> bool {
    if owner.0.is_null() || foreground.0.is_null() {
        return false;
    }
    owner == foreground || unsafe { GetAncestor(owner, GA_ROOT) } == foreground
}

#[cfg(windows)]
fn wait_for_clipboard_change(
    owner: HWND,
    before: u32,
    foreground: HWND,
) -> Result<ClipboardChange, String> {
    let mut last_open_error = None;
    for _ in 0..24 {
        match ClipboardGuard::open(owner) {
            Ok(_guard) => {
                let sequence = unsafe { GetClipboardSequenceNumber() };
                if sequence != before {
                    let belongs_to_foreground = clipboard_belongs_to_foreground(
                        unsafe { GetClipboardOwner() }
                            .map_err(|e| format!("读取剪贴板所有者失败: {e}"))?,
                        foreground,
                    );
                    drop(_guard);
                    // 等待关窗后的稳定序列号，避免把系统延迟提交的格式误判为
                    // 其他程序的新写入，或拿着已过期序列号去恢复。
                    if unsafe { GetClipboardSequenceNumber() } == sequence {
                        return Ok(if belongs_to_foreground {
                            ClipboardChange::Owned(sequence)
                        } else {
                            ClipboardChange::Other
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    continue;
                }
            }
            Err(e) => last_open_error = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    match last_open_error {
        Some(e) => Err(format!("等待 Ctrl+C 结果时无法打开剪贴板: {e}")),
        None => Ok(ClipboardChange::TimedOut),
    }
}

fn append_restore_error(message: &str, restore: Result<(), String>) -> String {
    match restore {
        Ok(()) => message.to_string(),
        Err(e) => format!("{message}；原剪贴板恢复失败：{e}"),
    }
}

#[cfg(windows)]
fn capture_clipboard() -> Result<CaptureResult, String> {
    // OpenClipboard(NULL) 不能作为恢复窗口：EmptyClipboard 会把 owner 置空，
    // 后续 SetClipboardData 必败。消息窗口只在本次采集期间持有剪贴板。
    let clipboard_owner = ClipboardOwner::new()?;
    let owner = clipboard_owner.0;

    // 关键安全门：快照、序列号在同一 clipboard guard 内读取；任何非纯文本格式
    // 都在按键注入之前拒绝，绝不先清空图片/文件/富文本/自定义剪贴板。
    let (snapshot, before) = ClipboardSnapshot::capture(owner)?;
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0.is_null() {
        return Err("没有可接收 Ctrl+C 的前台窗口，已取消划词回退".into());
    }

    if let Err(send_error) = send_ctrl_c() {
        // 部分注入也可能已经复制；仅确认新内容属于原前台窗口时才恢复。
        let restore_result = match wait_for_clipboard_change(owner, before, foreground) {
            Ok(ClipboardChange::Owned(sequence)) => snapshot.restore(owner, foreground, sequence),
            Ok(ClipboardChange::Other) => {
                return Err(format!("{send_error}；剪贴板含其他操作的新内容，未覆盖"));
            }
            Ok(ClipboardChange::TimedOut) => Ok(()),
            Err(e) => return Err(format!("{send_error}；无法确认 Ctrl+C 结果：{e}")),
        };
        return Err(append_restore_error(&send_error, restore_result));
    }

    let (sequence, copied) = match wait_for_clipboard_change(owner, before, foreground)? {
        ClipboardChange::Owned(sequence) => {
            // 读取时再次复核 sequence + owner，防止轮询与打开之间混入并发复制。
            let snapshot_result = match ClipboardGuard::open(owner) {
                Ok(_guard) => {
                    if unsafe { GetClipboardSequenceNumber() } != sequence
                        || !clipboard_belongs_to_foreground(
                            unsafe { GetClipboardOwner() }
                            .map_err(|e| format!("读取剪贴板所有者失败: {e}"))?,
                            foreground,
                        )
                    {
                        return Err("剪贴板在读取前已被其他操作更新，已保留最新内容".into());
                    }
                    ClipboardSnapshot::capture_open()
                }
                Err(e) => {
                    let restore_result = snapshot.restore(owner, foreground, sequence);
                    return Err(append_restore_error(
                        &format!("读取 Ctrl+C 结果失败: {e}"),
                        restore_result,
                    ));
                }
            };
            let copied = match snapshot_result {
                Ok(copied) => copied,
                Err(e) => {
                    let restore_result = snapshot.restore(owner, foreground, sequence);
                    return Err(append_restore_error(&e, restore_result));
                }
            };
            (sequence, copied)
        }
        ClipboardChange::Other => {
            return Err("检测到剪贴板被其他程序更新，已停止划词搜索并保留最新内容".into());
        }
        ClipboardChange::TimedOut => {
            return Err("Ctrl+C 后 600ms 内未检测到文本；请确认应用支持复制选区".into());
        }
    };

    let text = match copied.unicode_text() {
        Ok(text) => text,
        Err(e) => {
            let restore_result = snapshot.restore(owner, foreground, sequence);
            return Err(append_restore_error(&e, restore_result));
        }
    };
    let warning = snapshot.restore(owner, foreground, sequence).err().map(|e| {
        format!("已搜索，但原剪贴板恢复失败：{e}")
    });
    normalize_selection(&text)
        .map(|text| CaptureResult { text, request_id: 0, method: "clipboard", warning })
        .ok_or_else(|| "Ctrl+C 未复制到文本选区".into())
}

#[cfg(test)]
mod tests {
    use super::{append_restore_error, decode_unicode_text, normalize_selection, MAX_SELECTION_CHARS};

    #[test]
    fn normalizes_multiline_selection_and_caps_length() {
        let raw = format!("  @库名\r\n+必须\t-title:  {}  ", "字".repeat(3000));
        let text = normalize_selection(&raw).unwrap();
        assert!(text.starts_with("@库名 +必须 -title: "));
        assert!(!text.contains('\n'));
        assert_eq!(text.chars().count(), MAX_SELECTION_CHARS);
    }

    #[test]
    fn selection_limit_never_leaves_a_trailing_separator() {
        let raw = format!("{} tail", "字".repeat(MAX_SELECTION_CHARS - 1));
        let text = normalize_selection(&raw).unwrap();
        assert_eq!(text.chars().count(), MAX_SELECTION_CHARS);
        assert!(!text.ends_with(' '));
    }

    #[test]
    fn rejects_empty_and_control_only_selection() {
        assert!(normalize_selection(" \n\t ").is_none());
        assert!(normalize_selection("\0\u{7}").is_none());
    }

    #[test]
    fn preserves_literal_query_punctuation() {
        assert_eq!(normalize_selection("  mtime:7d  ").as_deref(), Some("mtime:7d"));
    }

    #[test]
    fn accepts_only_snapshot_safe_clipboard_formats() {
        #[cfg(windows)]
        {
            assert!(super::is_supported_text_format(super::CF_UNICODETEXT.0 as u32));
            assert!(!super::is_supported_text_format(15)); // CF_HDROP
            assert!(!super::is_supported_text_format(49158)); // 常见自定义格式
        }
    }

    #[test]
    fn decodes_unicode_snapshot_and_stops_at_terminator() {
        let words = [0x0041u16, 0x4e2du16, 0x0000, 0x0042];
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        assert_eq!(decode_unicode_text(&bytes).unwrap(), "A中");
    }

    #[test]
    fn rejects_invalid_unicode_snapshot_length_and_surrogate() {
        assert!(decode_unicode_text(&[0x41]).is_err());
        let unpaired = [0xd800u16].iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<_>>();
        assert!(decode_unicode_text(&unpaired).is_err());
    }

    #[test]
    fn restore_failure_is_never_silently_dropped() {
        assert_eq!(append_restore_error("已搜索", Ok(())), "已搜索");
        assert_eq!(
            append_restore_error("已搜索", Err("序列号已变化".into())),
            "已搜索；原剪贴板恢复失败：序列号已变化"
        );
    }
}
