//! 下载安装包：流式写进临时目录，超过 200 MB 就放弃，下载完先验签再按安装形态检查文件格式。
//!
//! 临时目录是 `%TEMP%\KwikPaste-<版本>-updater-XXXX`（macOS 是 `$TMPDIR` 下同名）。验签或格式检查
//! 失败时整个目录删掉；成功的留给安装交接使用，下次启动时清理一天以前的旧目录。

use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, anyhow, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::target::InstallKind;

/// 安装包大小上限。
pub const MAX_PACKAGE_BYTES: u64 = 200 * 1024 * 1024;
const TEMP_DIR_PREFIX: &str = "KwikPaste-";
const TEMP_DIR_INFIX: &str = "-updater-";
/// 启动时清理多久以前的临时目录。
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// 下载进度；`total` 来自 `Content-Length`，没有时为 `None`。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    /// 0–1，没有总大小时为 `None`。
    pub progress: Option<f64>,
}

/// 下载并验签完成的安装包。
#[derive(Debug, Clone)]
pub struct Package {
    /// 本次更新的临时目录。
    pub dir: PathBuf,
    /// 安装包文件：NSIS 是 `KwikPaste-<v>-installer.exe`，便携版是 zip，macOS 是 `.app.tar.gz`。
    pub file: PathBuf,
    pub(crate) signature: String,
    pub(crate) public_key: String,
    /// 已验签内容落成的最终字节摘要；NSIS 的旧式 zip 会先提取 exe。
    #[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
    pub(crate) digest: [u8; 32],
}

/// 下载 `url` 到新建的临时目录，验签后按 `kind` 检查格式。
pub(crate) async fn download(
    client: &reqwest::Client,
    url: &Url,
    signature: &str,
    public_key: &str,
    version: &str,
    kind: &InstallKind,
    on_progress: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> anyhow::Result<Package> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("{TEMP_DIR_PREFIX}{version}{TEMP_DIR_INFIX}"))
        .tempdir()
        .context("failed to create the update directory")?
        .keep();

    let result = async {
        let part = dir.join(format!("{TEMP_DIR_PREFIX}{version}.download"));
        fetch_to_file(client, url, &part, MAX_PACKAGE_BYTES, on_progress).await?;
        let bytes = fs::read(&part).with_context(|| format!("failed to read {part:?}"))?;
        let signed = crate::verify::verify(&bytes, signature, public_key)?;
        check_signed_name(kind, version, &signed)?;
        let (file, digest) = finalize(&dir, version, kind, &bytes)?;
        fs::remove_file(&part).ok();
        Ok(Package {
            dir: dir.clone(),
            file,
            signature: signature.to_owned(),
            public_key: public_key.to_owned(),
            digest,
        })
    }
    .await;

    if result.is_err() {
        fs::remove_dir_all(&dir).ok();
    }
    result
}

/// 流式写入 `path`，`limit` 字节以内。
pub(crate) async fn fetch_to_file(
    client: &reqwest::Client,
    url: &Url,
    path: &Path,
    limit: u64,
    on_progress: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> anyhow::Result<u64> {
    let mut response = client
        .get(url.clone())
        .send()
        .await
        .context("failed to download update")?;
    let status = response.status();
    if !status.is_success() {
        bail!("download answered {status}");
    }
    let total = response.content_length();
    if total.is_some_and(|total| total > limit) {
        bail!("update package is larger than {limit} bytes");
    }

    let mut file = File::create(path).with_context(|| format!("failed to create {path:?}"))?;
    let mut downloaded = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to download update")?
    {
        downloaded += chunk.len() as u64;
        if downloaded > limit {
            bail!("update package is larger than {limit} bytes");
        }
        file.write_all(&chunk)
            .with_context(|| format!("failed to write {path:?}"))?;
        on_progress(DownloadProgress {
            downloaded,
            total,
            progress: total
                .filter(|total| *total > 0)
                .map(|total| (downloaded as f64 / total as f64).clamp(0.0, 1.0)),
        });
    }
    file.sync_all()
        .with_context(|| format!("failed to flush {path:?}"))?;
    if total.is_some_and(|total| total != downloaded) {
        bail!("download ended early: {downloaded} of {total:?} bytes");
    }
    Ok(downloaded)
}

/// 签名记的文件名必须是这个版本、这种安装形态、本机架构的发布产物：同一把密钥签过的 1.x 安装包、
/// 旧版本、别的形态或扩展换进清单都不认。
fn check_signed_name(kind: &InstallKind, version: &str, signed: &str) -> anyhow::Result<()> {
    let matches = kind.asset_suffix(version).is_some_and(|suffix| {
        signed
            .strip_suffix(suffix.as_str())
            .is_some_and(|product| !product.is_empty())
    });
    if !matches {
        bail!("the signature is for {signed}, not the {version} update of this installation");
    }
    Ok(())
}

/// 按安装形态检查并落成最终文件名。
fn finalize(
    dir: &Path,
    version: &str,
    kind: &InstallKind,
    bytes: &[u8],
) -> anyhow::Result<(PathBuf, [u8; 32])> {
    let (name, contents): (String, std::borrow::Cow<'_, [u8]>) = match kind {
        InstallKind::Nsis => (
            format!("{TEMP_DIR_PREFIX}{version}-installer.exe"),
            nsis_installer(bytes)?.into(),
        ),
        InstallKind::Portable => {
            if !bytes.starts_with(b"PK") {
                bail!("portable update is not a zip archive");
            }
            (
                format!("{TEMP_DIR_PREFIX}{version}-portable.zip"),
                bytes.into(),
            )
        }
        InstallKind::MacApp { .. } => {
            if !bytes.starts_with(&[0x1f, 0x8b]) {
                bail!("macOS update is not a gzip archive");
            }
            (
                format!("{TEMP_DIR_PREFIX}{version}.app.tar.gz"),
                bytes.into(),
            )
        }
        InstallKind::Unmanaged => bail!("this build does not install updates"),
    };

    let path = dir.join(name);
    fs::write(&path, &contents).with_context(|| format!("failed to write {path:?}"))?;
    Ok((path, Sha256::digest(&contents).into()))
}

/// NSIS 安装包：直接是 exe（`MZ` 开头），或者是只含一个 exe 的 zip（Tauri v1 兼容格式）。
fn nsis_installer(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    if bytes.starts_with(b"MZ") {
        return Ok(bytes.to_vec());
    }
    if !bytes.starts_with(b"PK") {
        bail!("installer is neither an exe nor a zip archive");
    }

    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("failed to read installer archive")?;
    let mut found = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .context("failed to read installer archive")?;
        let is_exe = entry.is_file() && entry.name().to_ascii_lowercase().ends_with(".exe");
        if is_exe && found.replace(index).is_some() {
            bail!("installer archive contains more than one executable");
        }
    }
    let index = found.ok_or_else(|| anyhow!("installer archive contains no executable"))?;
    let entry = archive.by_index(index)?;
    let size = entry.size();
    let installer = read_entry(entry, size)?;
    if !installer.starts_with(b"MZ") {
        bail!("installer is not a Windows program");
    }
    Ok(installer)
}

/// 读出压缩包里的一个文件，最多 [`MAX_PACKAGE_BYTES`]。条目头里声称的大小只用来预分配（同样封顶），
/// 实际读到超过上限就报错：改过的包不能让我们按它声称的大小去分配内存（分配失败直接终止进程）。
pub(crate) fn read_entry(entry: impl Read, declared_size: u64) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(declared_size.min(MAX_PACKAGE_BYTES) as usize);
    entry
        .take(MAX_PACKAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read package entry")?;
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        bail!("package entry is larger than {MAX_PACKAGE_BYTES} bytes");
    }
    Ok(bytes)
}

/// 删掉一天以前的 `KwikPaste-*-updater-*` 临时目录（Tauri 的更新器也用这个前缀，它从来不清理）。
pub(crate) fn remove_stale_dirs(temp: &Path, now: SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(temp) else {
        return 0;
    };

    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(TEMP_DIR_PREFIX) || !name.contains(TEMP_DIR_INFIX) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .filter(|metadata| metadata.is_dir())
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STALE_AFTER);
        if stale && fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::Mutex;

    use super::*;
    use crate::http::testing::{Reply, Server};
    use crate::verify::testing::TestKey;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn client() -> reqwest::Client {
        crate::http::updater_client(&semver::Version::new(2, 0, 0)).unwrap()
    }

    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn verified_installer_lands_in_a_temp_dir_with_progress() {
        let rt = runtime();
        let key = TestKey::generate();
        let installer = [b"MZ".as_slice(), &[7u8; 70_000]].concat();
        let server = Server::start(vec![Reply::ok(installer.clone())]);
        let url = Url::parse(&server.url("/setup.exe")).unwrap();
        let seen = Mutex::new(Vec::new());
        let name = format!(
            "KwikPaste{}",
            InstallKind::Nsis.asset_suffix("2.0.1").unwrap()
        );

        let package = rt
            .block_on(download(
                &client(),
                &url,
                &key.sign_as(&installer, &name),
                &key.public_key(),
                "2.0.1",
                &InstallKind::Nsis,
                &|progress| seen.lock().unwrap().push(progress),
            ))
            .unwrap();

        assert_eq!(fs::read(&package.file).unwrap(), installer);
        assert!(
            package
                .file
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("KwikPaste-2.0.1-installer.exe")
        );
        let dir_name = package
            .dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            dir_name.starts_with("KwikPaste-2.0.1-updater-"),
            "{dir_name}"
        );
        let seen = seen.into_inner().unwrap();
        let last = seen.last().unwrap();
        assert_eq!(last.downloaded, installer.len() as u64);
        assert_eq!(last.total, Some(installer.len() as u64));
        assert_eq!(last.progress, Some(1.0));
        fs::remove_dir_all(&package.dir).unwrap();
    }

    /// 别的密钥签的，或者正式密钥签的别的文件（这里是 1.x 安装包）。
    #[test]
    fn bad_signature_removes_the_download() {
        let rt = runtime();
        let key = TestKey::generate();
        let other = TestKey::generate();
        let installer = b"MZ unsigned".to_vec();

        for signature in [
            other.sign(&installer, true),
            key.sign_as(&installer, "KwikPaste_1.4.0_x64-setup.exe"),
        ] {
            let server = Server::start(vec![Reply::ok(installer.clone())]);
            let url = Url::parse(&server.url("/setup.exe")).unwrap();
            let err = rt
                .block_on(download(
                    &client(),
                    &url,
                    &signature,
                    &key.public_key(),
                    "2.0.1-sigtest",
                    &InstallKind::Nsis,
                    &|_| {},
                ))
                .unwrap_err();

            assert!(format!("{err:#}").contains("signature"), "{err:#}");
            let leftovers = fs::read_dir(std::env::temp_dir())
                .unwrap()
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("KwikPaste-2.0.1-sigtest-updater-")
                })
                .count();
            assert_eq!(leftovers, 0);
        }
    }

    #[test]
    fn signatures_must_name_this_version_kind_and_arch() {
        let (windows, other, mac) = if cfg!(target_arch = "aarch64") {
            ("arm64", "x64", "aarch64")
        } else {
            ("x64", "arm64", "x64")
        };
        let mac_app = InstallKind::MacApp {
            bundle: PathBuf::from("/Applications/KwikPaste.app"),
        };
        for (kind, name) in [
            (
                &InstallKind::Nsis,
                format!("KwikPaste_2.0.10_{windows}-setup.exe"),
            ),
            (
                &InstallKind::Nsis,
                format!("KwikPasteBundleTest_2.0.10_{windows}-setup.exe"),
            ),
            (
                &InstallKind::Portable,
                format!("KwikPaste_2.0.10_{windows}_portable.zip"),
            ),
            (&mac_app, format!("KwikPaste_2.0.10_{mac}.app.tar.gz")),
        ] {
            assert!(check_signed_name(kind, "2.0.10", &name).is_ok(), "{name}");
        }

        // 1.x、旧版本、别的形态、别的架构、扩展，以及 bundle 阶段签的不带版本号的 macOS 包。
        for (kind, name) in [
            (
                &InstallKind::Nsis,
                format!("KwikPaste_1.4.0_{windows}-setup.exe"),
            ),
            (
                &InstallKind::Nsis,
                format!("KwikPaste_2.0.9_{windows}-setup.exe"),
            ),
            (
                &InstallKind::Nsis,
                format!("KwikPaste_12.0.10_{windows}-setup.exe"),
            ),
            (
                &InstallKind::Nsis,
                format!("KwikPaste_2.0.10_{other}-setup.exe"),
            ),
            (&InstallKind::Nsis, format!("_2.0.10_{windows}-setup.exe")),
            (
                &InstallKind::Nsis,
                format!("KwikPaste_2.0.10_{windows}_portable.zip"),
            ),
            (
                &InstallKind::Portable,
                format!("KwikPaste_2.0.10_{windows}-setup.exe"),
            ),
            (
                &InstallKind::Portable,
                "KwikPaste-ext-ocr_2.0.10_windows-x86_64.exe".to_owned(),
            ),
            (&mac_app, "KwikPaste.app.tar.gz".to_owned()),
            (
                &InstallKind::Unmanaged,
                format!("KwikPaste_2.0.10_{windows}-setup.exe"),
            ),
        ] {
            assert!(check_signed_name(kind, "2.0.10", &name).is_err(), "{name}");
        }
    }

    #[test]
    fn size_limit_is_enforced_while_streaming() {
        let rt = runtime();
        let dir = tempfile::tempdir().unwrap();
        let body = vec![b'x'; 4096];

        let announced = Server::start(vec![Reply::ok(body.clone())]);
        let url = Url::parse(&announced.url("/big")).unwrap();
        let err = rt
            .block_on(fetch_to_file(
                &client(),
                &url,
                &dir.path().join("a"),
                1024,
                &|_| {},
            ))
            .unwrap_err();
        assert!(err.to_string().contains("larger"), "{err}");

        let ok = Server::start(vec![Reply::ok(body.clone())]);
        let url = Url::parse(&ok.url("/fits")).unwrap();
        let written = rt
            .block_on(fetch_to_file(
                &client(),
                &url,
                &dir.path().join("b"),
                4096,
                &|_| {},
            ))
            .unwrap();
        assert_eq!(written, 4096);

        let missing = Server::start(vec![Reply::status("404 Not Found")]);
        let url = Url::parse(&missing.url("/gone")).unwrap();
        assert!(
            rt.block_on(fetch_to_file(
                &client(),
                &url,
                &dir.path().join("c"),
                4096,
                &|_| {}
            ))
            .is_err()
        );
    }

    #[test]
    fn packages_are_checked_per_install_kind() {
        let dir = tempfile::tempdir().unwrap();

        let exe = finalize(dir.path(), "2.0.1", &InstallKind::Nsis, b"MZ setup").unwrap();
        assert_eq!(fs::read(exe.0).unwrap(), b"MZ setup");
        let zipped = zip(&[("KwikPaste_2.0.1_x64-setup.exe", b"MZ zipped")]);
        let exe = finalize(dir.path(), "2.0.2", &InstallKind::Nsis, &zipped).unwrap();
        assert_eq!(fs::read(exe.0).unwrap(), b"MZ zipped");
        assert!(finalize(dir.path(), "2.0.3", &InstallKind::Nsis, b"#!/bin/sh").is_err());
        assert!(
            finalize(
                dir.path(),
                "2.0.3",
                &InstallKind::Nsis,
                &zip(&[("a.exe", b"MZ a"), ("b.exe", b"MZ b")])
            )
            .is_err()
        );

        let portable = zip(&[("KwikPaste/KwikPaste.exe", b"MZ new")]);
        assert!(finalize(dir.path(), "2.0.1", &InstallKind::Portable, &portable).is_ok());
        assert!(finalize(dir.path(), "2.0.1", &InstallKind::Portable, b"MZ raw").is_err());

        let mac = InstallKind::MacApp {
            bundle: PathBuf::from("/Applications/KwikPaste.app"),
        };
        assert!(finalize(dir.path(), "2.0.1", &mac, &[0x1f, 0x8b, 8, 0]).is_ok());
        assert!(finalize(dir.path(), "2.0.1", &mac, b"PK").is_err());
        assert!(finalize(dir.path(), "2.0.1", &InstallKind::Unmanaged, b"MZ").is_err());
    }

    #[test]
    fn stale_update_dirs_are_cleaned_up() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("KwikPaste-1.4.0-updater-AbC1")).unwrap();
        fs::create_dir_all(temp.path().join("KwikPaste-2.0.1-updater-XyZ2")).unwrap();
        fs::create_dir_all(temp.path().join("OtherApp-1.0-updater-a")).unwrap();
        fs::write(
            temp.path().join("KwikPaste-2.0.1-updater-file"),
            b"not a dir",
        )
        .unwrap();

        assert_eq!(remove_stale_dirs(temp.path(), SystemTime::now()), 0);
        let later = SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60);
        assert_eq!(remove_stale_dirs(temp.path(), later), 2);
        assert!(temp.path().join("OtherApp-1.0-updater-a").exists());
        assert!(temp.path().join("KwikPaste-2.0.1-updater-file").exists());
    }
}
