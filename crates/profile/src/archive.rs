//! 最小 tar / gzip 解包器：离线安装 npm `pack` 产物（`.tgz`）用。
//!
//! 不引入 `tar` crate（离线依赖清单不允许新增），只实现安装所需的最小子集：
//!
//! - ustar 头 + 512 字节块对齐 + 头 checksum 校验；
//! - 常规文件（typeflag `0`/`\0`）与目录（`5`）；
//! - PAX 扩展头（`x`）里的 `path=` 与 GNU 长文件名（`L`）；
//! - gzip（`.tgz`）与未压缩 tar 都能读。
//!
//! **安全边界**：拒绝绝对路径、`..`、反斜杠、Windows 保留设备名，以及任何
//! 符号链接/硬链接/设备文件条目；解包总大小、条目数、单文件大小都有上限。
//! 解包目标必须是新建的空目录（调用方负责），因此不会覆盖用户已有文件。

use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;

const BLOCK: usize = 512;
const MAX_PAX_HEADER_BYTES: u64 = 1024 * 1024;

/// 解包限制，默认值足够安装普通 npm 包，同时挡住明显异常的压缩包。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpackLimits {
    /// 最多条目数（文件 + 目录）。
    pub max_entries: usize,
    /// 单文件解压后大小上限。
    pub max_file_bytes: u64,
    /// 所有文件解压后总大小上限。
    pub max_total_bytes: u64,
}

impl Default for UnpackLimits {
    fn default() -> Self {
        Self {
            max_entries: 20_000,
            max_file_bytes: 64 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
        }
    }
}

/// 解包结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpackedArchive {
    /// npm pack 约定的包根（通常是 `<staging>/package`，直接 tar 也可能是 staging）。
    pub root: PathBuf,
    /// 解出的文件相对路径（审计/调试用）。
    pub files: Vec<PathBuf>,
}

/// 解包 `.tgz`（或未压缩 `.tar`）到 `destination`。
///
/// `destination` 应当是**新建的空目录**：本函数不做覆盖安全检查之外的清理。
pub fn unpack_tgz(archive: &Path, destination: &Path) -> Result<UnpackedArchive, String> {
    let file = fs::File::open(archive)
        .map_err(|err| format!("打开压缩包 {} 失败: {err}", archive.display()))?;
    let mut reader = BufReader::new(file);
    let is_gzip = {
        let buffered = reader
            .fill_buf()
            .map_err(|err| format!("读取压缩包 {} 失败: {err}", archive.display()))?;
        buffered.len() >= 2 && buffered[0] == 0x1f && buffered[1] == 0x8b
    };
    fs::create_dir_all(destination)
        .map_err(|err| format!("创建解包目录 {} 失败: {err}", destination.display()))?;

    let files = if is_gzip {
        unpack_tar(GzDecoder::new(reader), destination)?
    } else {
        unpack_tar(reader, destination)?
    };
    let root = if destination.join("package").join("package.json").is_file() {
        destination.join("package")
    } else if destination.join("package.json").is_file() {
        destination.to_path_buf()
    } else {
        return Err(format!(
            "压缩包 {} 内找不到 package/package.json（不是 npm pack 产物？）",
            archive.display()
        ));
    };
    Ok(UnpackedArchive { root, files })
}

fn unpack_tar<R: Read>(source: R, destination: &Path) -> Result<Vec<PathBuf>, String> {
    let limits = UnpackLimits::default();
    let mut reader = BufReader::new(source);
    let mut header = [0u8; BLOCK];
    let mut pending_path: Option<String> = None;
    let mut extracted = Vec::new();
    let mut total_bytes = 0u64;
    let mut entries = 0usize;

    loop {
        if !read_block(&mut reader, &mut header)? {
            break;
        }
        if header.iter().all(|byte| *byte == 0) {
            // tar 结束标记：一个全零块即可（规范是两块，gzip 流可能提前结束）。
            break;
        }
        entries += 1;
        if entries > limits.max_entries {
            return Err(format!("压缩包条目数超过上限 {}", limits.max_entries));
        }
        verify_checksum(&header)?;
        let typeflag = header[156];
        let size = parse_octal(&header[124..136])?;

        match typeflag {
            // PAX 扩展头 / GNU 长文件名：只用于给下一个条目提供名字。
            b'x' | b'g' => {
                let data = read_extension_data(&mut reader, size)?;
                if typeflag == b'x' {
                    if let Some(path) = pax_path(&data) {
                        pending_path = Some(path);
                    }
                }
                continue;
            }
            b'L' => {
                let data = read_extension_data(&mut reader, size)?;
                pending_path = Some(trim_nul(&data));
                continue;
            }
            b'K' => {
                let _ = read_extension_data(&mut reader, size)?;
                continue;
            }
            _ => {}
        }

        let raw_name = match pending_path.take() {
            Some(path) => path,
            None => tar_name(&header)?,
        };
        let relative = normalize_entry_path(&raw_name)?;

        match typeflag {
            b'0' | 0 => {
                if size > limits.max_file_bytes {
                    return Err(format!(
                        "文件 {raw_name} 超过单文件上限 {} 字节",
                        limits.max_file_bytes
                    ));
                }
                total_bytes = total_bytes.saturating_add(size);
                if total_bytes > limits.max_total_bytes {
                    return Err(format!(
                        "解压总大小超过上限 {} 字节",
                        limits.max_total_bytes
                    ));
                }
                let target = destination.join(&relative);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|err| format!("创建目录 {} 失败: {err}", parent.display()))?;
                }
                let mut file = fs::File::create(&target)
                    .map_err(|err| format!("创建文件 {} 失败: {err}", target.display()))?;
                copy_exact(&mut reader, &mut file, size)?;
                skip_padding(&mut reader, size)?;
                extracted.push(relative);
            }
            b'5' => {
                fs::create_dir_all(destination.join(&relative))
                    .map_err(|err| format!("创建目录 {} 失败: {err}", relative.display()))?;
                skip_padding(&mut reader, size)?;
            }
            b'1' | b'2' => {
                return Err(format!(
                    "拒绝链接条目（{raw_name}）：离线安装不允许符号链接/硬链接"
                ));
            }
            other => {
                return Err(format!("不支持的 tar 条目类型 0x{other:02x}（{raw_name}）"));
            }
        }
    }
    Ok(extracted)
}

fn read_block<R: Read>(reader: &mut R, header: &mut [u8; BLOCK]) -> Result<bool, String> {
    let mut filled = 0;
    while filled < BLOCK {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(String::from("tar 头被截断")),
            Ok(read) => filled += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(format!("读取 tar 头失败: {err}")),
        }
    }
    Ok(true)
}

fn verify_checksum(header: &[u8; BLOCK]) -> Result<(), String> {
    let stored =
        parse_octal(&header[148..156]).map_err(|_| String::from("tar 头 checksum 字段非法"))?;
    let mut sum = 0u64;
    for (index, byte) in header.iter().enumerate() {
        let value = if (148..156).contains(&index) {
            u64::from(b' ')
        } else {
            u64::from(*byte)
        };
        sum += value;
    }
    if sum != stored {
        return Err(format!("tar 头 checksum 校验失败（{sum} != {stored}）"));
    }
    Ok(())
}

fn parse_octal(field: &[u8]) -> Result<u64, String> {
    let text = String::from_utf8_lossy(field);
    let text = text.trim_matches(|character: char| character == '\0' || character == ' ');
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|_| format!("非法八进制字段: {text:?}"))
}

fn tar_name(header: &[u8; BLOCK]) -> Result<String, String> {
    let name = field_string(&header[0..100]);
    let prefix = field_string(&header[345..500]);
    let magic = &header[257..263];
    let full = if !prefix.is_empty() && magic.starts_with(b"ustar") {
        format!("{prefix}/{name}")
    } else {
        name
    };
    if full.is_empty() {
        return Err(String::from("tar 条目名为空"));
    }
    Ok(full)
}

fn field_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}

fn trim_nul(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}

/// PAX 扩展头是 `"<长度> <key>=<value>\n"` 的重复记录；只取 `path`。
fn pax_path(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    for record in text.split('\n') {
        let Some((_, key_value)) = record.split_once(' ') else {
            continue;
        };
        if let Some(value) = key_value.strip_prefix("path=") {
            return Some(value.to_owned());
        }
    }
    None
}

fn copy_exact<R: Read>(reader: &mut R, writer: &mut impl Write, size: u64) -> Result<(), String> {
    let mut remaining = size;
    let mut buffer = [0u8; 8192];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = reader
            .read(&mut buffer[..want])
            .map_err(|err| format!("读取 tar 数据失败: {err}"))?;
        if read == 0 {
            return Err(String::from("tar 数据被截断"));
        }
        writer
            .write_all(&buffer[..read])
            .map_err(|err| format!("写入解包文件失败: {err}"))?;
        remaining -= read as u64;
    }
    Ok(())
}

fn skip_padding<R: Read>(reader: &mut R, size: u64) -> Result<(), String> {
    let remainder = (size % BLOCK as u64) as usize;
    if remainder == 0 {
        return Ok(());
    }
    let skip = BLOCK - remainder;
    let mut buffer = [0u8; BLOCK];
    reader
        .read_exact(&mut buffer[..skip])
        .map_err(|err| format!("跳过 tar 对齐填充失败: {err}"))
}

fn read_extension_data<R: Read>(reader: &mut R, size: u64) -> Result<Vec<u8>, String> {
    if size > MAX_PAX_HEADER_BYTES {
        return Err(format!("tar 扩展头过大（{size} 字节）"));
    }
    let mut data = vec![0u8; size as usize];
    reader
        .read_exact(&mut data)
        .map_err(|err| format!("读取 tar 扩展头失败: {err}"))?;
    skip_padding(reader, size)?;
    Ok(data)
}

/// 校验并归一化 tar 条目路径（相对路径，无逃逸、无反斜杠、无 Windows 设备名）。
fn normalize_entry_path(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() {
        return Err(String::from("tar 条目名为空"));
    }
    if raw.contains('\\') {
        return Err(format!("拒绝含反斜杠的 tar 路径: {raw}"));
    }
    let mut normalized = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => {
                if is_windows_reserved(part) {
                    return Err(format!("拒绝 Windows 保留设备名: {raw}"));
                }
                normalized.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("拒绝路径逃逸（..）: {raw}"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("拒绝绝对路径: {raw}"));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(format!("拒绝空路径条目: {raw}"));
    }
    Ok(normalized)
}

fn is_windows_reserved(part: &OsStr) -> bool {
    let Some(name) = part.to_str() else {
        return false;
    };
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit()
}

/// 测试辅助：在本文件测试与 `install.rs` 测试里构造内联小 tar/tgz。
#[cfg(test)]
pub(crate) mod test_support {
    use super::BLOCK;
    use std::io::Write;

    pub(crate) fn write_octal(field: &mut [u8], value: u64) {
        let width = field.len() - 1;
        let text = format!("{value:0width$o}");
        let bytes = text.as_bytes();
        let start = bytes.len().saturating_sub(width);
        let digits = &bytes[start..];
        let offset = width - digits.len();
        field[..offset].fill(b'0');
        field[offset..width].copy_from_slice(digits);
        field[width] = 0;
    }

    pub(crate) fn tar_header(name: &str, size: u64, typeflag: u8) -> [u8; BLOCK] {
        let mut header = [0u8; BLOCK];
        let name_bytes = name.as_bytes();
        let count = name_bytes.len().min(100);
        header[..count].copy_from_slice(&name_bytes[..count]);
        write_octal(&mut header[100..108], 0o644);
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        write_octal(&mut header[124..136], size);
        write_octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = typeflag;
        header[257..262].copy_from_slice(b"ustar");
        header[263..265].copy_from_slice(b"00");
        let sum: u64 = header.iter().map(|byte| u64::from(*byte)).sum();
        let checksum = format!("{sum:06o}");
        header[148..154].copy_from_slice(&checksum.as_bytes()[..6]);
        header[154] = 0;
        header[155] = b' ';
        header
    }

    pub(crate) fn append_entry(tar: &mut Vec<u8>, name: &str, typeflag: u8, data: &[u8]) {
        tar.extend_from_slice(&tar_header(name, data.len() as u64, typeflag));
        tar.extend_from_slice(data);
        let padding = (BLOCK - data.len() % BLOCK) % BLOCK;
        tar.extend(std::iter::repeat_n(0u8, padding));
    }

    pub(crate) fn finish_tar(tar: &mut Vec<u8>) {
        tar.extend_from_slice(&[0u8; BLOCK]);
        tar.extend_from_slice(&[0u8; BLOCK]);
    }

    /// 构造 tar（不入 gzip）。
    pub(crate) fn build_tar(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, typeflag, data) in entries {
            append_entry(&mut tar, name, *typeflag, data);
        }
        finish_tar(&mut tar);
        tar
    }

    pub(crate) fn gzip(data: &[u8]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).expect("gzip write");
        encoder.finish().expect("gzip finish")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dshr-archive-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&root).expect("temp root");
        root
    }

    fn write_archive(root: &Path, name: &str, tar: &[u8], compress: bool) -> PathBuf {
        let path = root.join(name);
        let bytes = if compress { gzip(tar) } else { tar.to_vec() };
        fs::write(&path, bytes).expect("write archive");
        path
    }

    fn sample_tar() -> Vec<u8> {
        let mut tar = Vec::new();
        append_entry(&mut tar, "package/", b'5', b"");
        append_entry(
            &mut tar,
            "package/package.json",
            b'0',
            br#"{"name":"@deepseek-ai/dsh-tool-demo"}"#,
        );
        append_entry(
            &mut tar,
            "package/lib/index.js",
            b'0',
            b"export const ok = true\n",
        );
        finish_tar(&mut tar);
        tar
    }

    #[test]
    fn unpacks_gzipped_npm_tarball() {
        let root = temp_root("tgz");
        let archive = write_archive(&root, "demo.tgz", &sample_tar(), true);
        let staging = root.join("staging");
        let unpacked = unpack_tgz(&archive, &staging).expect("unpack");
        assert_eq!(unpacked.root, staging.join("package"));
        assert!(unpacked.root.join("package.json").is_file());
        assert!(unpacked.root.join("lib/index.js").is_file());
        assert!(unpacked
            .files
            .contains(&PathBuf::from("package/lib/index.js")));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unpacks_plain_tar_too() {
        let root = temp_root("tar");
        let archive = write_archive(&root, "demo.tar", &sample_tar(), false);
        let staging = root.join("staging");
        let unpacked = unpack_tgz(&archive, &staging).expect("unpack plain tar");
        assert!(unpacked.root.join("package.json").is_file());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_path_escape_absolute_paths_and_symlinks() {
        let root = temp_root("escape");
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("parent", {
                let mut tar = Vec::new();
                append_entry(&mut tar, "package/../../evil.txt", b'0', b"x");
                finish_tar(&mut tar);
                tar
            }),
            ("absolute", {
                let mut tar = Vec::new();
                append_entry(&mut tar, "/evil.txt", b'0', b"x");
                finish_tar(&mut tar);
                tar
            }),
            ("backslash", {
                let mut tar = Vec::new();
                append_entry(&mut tar, "package\\..\\evil.txt", b'0', b"x");
                finish_tar(&mut tar);
                tar
            }),
            ("symlink", {
                let mut tar = Vec::new();
                append_entry(&mut tar, "package/link", b'2', b"");
                finish_tar(&mut tar);
                tar
            }),
            ("hardlink", {
                let mut tar = Vec::new();
                append_entry(&mut tar, "package/link", b'1', b"");
                finish_tar(&mut tar);
                tar
            }),
            ("device", {
                let mut tar = Vec::new();
                append_entry(&mut tar, "package/dev", b'3', b"");
                finish_tar(&mut tar);
                tar
            }),
        ];
        for (name, tar) in cases {
            let archive = write_archive(&root, &format!("{name}.tgz"), &tar, true);
            let staging = root.join(format!("staging-{name}"));
            let error = unpack_tgz(&archive, &staging).expect_err(name);
            assert!(!error.is_empty(), "{name}: 应当有错误信息");
            assert!(
                !root.join("evil.txt").exists(),
                "{name}: 不允许写到解包目录之外"
            );
        }
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rejects_windows_reserved_device_names() {
        let mut tar = Vec::new();
        append_entry(&mut tar, "package/CON.txt", b'0', b"x");
        finish_tar(&mut tar);
        assert!(normalize_entry_path("package/CON.txt").is_err());
        assert!(normalize_entry_path("package/ok.txt").is_ok());
        assert!(normalize_entry_path("package/com1.log").is_err());
    }

    #[test]
    fn rejects_missing_package_manifest_and_bad_checksum() {
        let root = temp_root("missing");
        let mut tar = Vec::new();
        append_entry(&mut tar, "other/file.txt", b'0', b"x");
        finish_tar(&mut tar);
        let archive = write_archive(&root, "missing.tgz", &tar, true);
        let error = unpack_tgz(&archive, &root.join("staging-missing")).expect_err("missing");
        assert!(error.contains("package.json"), "{error}");

        let mut bad = tar_header("package/package.json", 2, b'0');
        bad[100] ^= 0x01; // 破坏 checksum 覆盖的字段
        let mut tar = Vec::new();
        tar.extend_from_slice(&bad);
        tar.extend_from_slice(b"{}\0");
        tar.resize(BLOCK * 2, 0);
        let archive = write_archive(&root, "checksum.tgz", &tar, true);
        let error = unpack_tgz(&archive, &root.join("staging-checksum")).expect_err("checksum");
        assert!(error.contains("checksum"), "{error}");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn pax_long_path_is_applied() {
        let root = temp_root("pax");
        // 超过 100 字节的路径必须走 PAX path。
        let long_dir = "nested/".repeat(20);
        let long_path = format!("package/lib/{long_dir}index.js");
        assert!(long_path.len() > 100);

        let record = {
            let body = format!(" path={long_path}\n");
            let mut len = body.len() + 1;
            loop {
                let candidate = format!("{len}{body}");
                if candidate.len() == len {
                    break candidate;
                }
                len = candidate.len();
            }
        };

        let mut tar = Vec::new();
        append_entry(&mut tar, "package/package.json", b'0', br#"{"name":"pax"}"#);
        append_entry(&mut tar, "PaxHeaders.0/long", b'x', record.as_bytes());
        // 头里的短名字会被 PAX path 覆盖。
        append_entry(&mut tar, "package/lib/short.js", b'0', b"export {}\n");
        finish_tar(&mut tar);

        let archive = write_archive(&root, "pax.tgz", &tar, true);
        let staging = root.join("staging");
        let unpacked = unpack_tgz(&archive, &staging).expect("unpack pax");
        assert!(
            unpacked
                .root
                .join(long_path.trim_start_matches("package/"))
                .is_file(),
            "PAX 长路径应落到磁盘"
        );
        fs::remove_dir_all(root).ok();
    }
}
