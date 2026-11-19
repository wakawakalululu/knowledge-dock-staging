//! 零依赖文件日志：logs/kd-YYYY-MM-DD.log 按天滚动（>10MB 换 .old）。
//! 本地时间经 kernel32 GetLocalTime FFI 获取（不引 chrono）。
//! LOGGER 未初始化时仅 eprintln（config 等早期日志不丢失于控制台）。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

struct Logger {
    dir: PathBuf,
    file: Mutex<Option<(String, File)>>, // (当天日期, 文件句柄)
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// 初始化日志目录（setup 最早期调用；失败仅 eprintln，日志退化为仅控制台）
pub fn init(dir: PathBuf) {
    if LOGGER.set(Logger { dir: dir.clone(), file: Mutex::new(None) }).is_err() {
        return;
    }
    log("INFO", "app", "日志系统已初始化");
    // 丢弃旧日志文件（保留昨天/今天共 2 份 + .old）
    if let Ok(rd) = fs::read_dir(&dir) {
        let mut files: Vec<_> = rd.flatten().collect();
        files.sort_by_key(|e| e.file_name());
        if files.len() > 7 {
            for e in files.iter().take(files.len() - 7) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

#[repr(C)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

extern "system" {
    fn GetLocalTime(lp: *mut SystemTime);
}

fn local_now() -> (String, String) {
    let mut t = SystemTime {
        year: 0, month: 0, day_of_week: 0, day: 0, hour: 0, minute: 0, second: 0, milliseconds: 0,
    };
    unsafe { GetLocalTime(&mut t) };
    (
        format!("{:04}-{:02}-{:02}", t.year, t.month, t.day),
        format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second),
    )
}

/// 写一条日志；同时 eprintln 保持控制台可见
pub fn log(level: &str, module: &str, msg: &str) {
    let (date, time) = local_now();
    eprintln!("[{module}] {msg}");
    let Some(l) = LOGGER.get() else { return };
    let mut guard = match l.file.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    // 按天换文件
    let need_open = match guard.as_ref() {
        Some((d, _)) => d != &date,
        None => true,
    };
    if need_open {
        let _ = guard.take();
        let _ = fs::create_dir_all(&l.dir);
        let path = l.dir.join(format!("kd-{date}.log"));
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => *guard = Some((date.clone(), f)),
            Err(e) => {
                eprintln!("[log] 日志文件打开失败: {e}");
                return;
            }
        }
    }
    if let Some((_, f)) = guard.as_mut() {
        let line = format!("[{date} {time}] [{level}] [{module}] {msg}\n");
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
        // 大小轮转：>10MB → .old（覆盖旧 old）
        if let Ok(meta) = f.metadata() {
            if meta.len() > MAX_LOG_BYTES {
                let today = date.clone();
                let old = l.dir.join(format!("kd-{today}.old.log"));
                let _ = fs::rename(l.dir.join(format!("kd-{today}.log")), old);
                *guard = None;
            }
        }
    }
}

/// 读当前日志文件尾部 lines 行（/api/logs 用；无文件返回空）
pub fn read_tail(lines: usize) -> String {
    let Some(l) = LOGGER.get() else { return String::new() };
    let (date, _) = local_now();
    let path = l.dir.join(format!("kd-{date}.log"));
    let Ok(content) = fs::read_to_string(path) else { return String::new() };
    let all: Vec<&str> = content.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

/// 日志目录路径（打开日志目录按钮用）
pub fn dir() -> Option<PathBuf> {
    LOGGER.get().map(|l| l.dir.clone())
}

/// 便捷宏：kdlog!(info, "engine", "同步完成 docs={}", n)
#[macro_export]
macro_rules! kdlog {
    (info, $m:expr, $($arg:tt)*) => { $crate::logging::log("INFO", $m, &format!($($arg)*)) };
    (warn, $m:expr, $($arg:tt)*) => { $crate::logging::log("WARN", $m, &format!($($arg)*)) };
    (error, $m:expr, $($arg:tt)*) => { $crate::logging::log("ERROR", $m, &format!($($arg)*)) };
}
