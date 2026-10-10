//! Ported from oh-my-pi packages/coding-agent/src/lsp/config.ts (MIT).
//! This file owns pinned downloads, checksum verification and safe extraction.
//! `LspManager` invokes it only for rust-analyzer, Ruff and Biome.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::search_path::installed_bin_dir;

#[derive(Clone, Debug)]
pub struct Installer {
    data_dir: PathBuf,
    http: reqwest::Client,
    download_base: Option<String>,
}

#[derive(Deserialize)]
struct Package {
    version: String,
    platforms: std::collections::BTreeMap<String, Asset>,
}
#[derive(Deserialize)]
struct Asset {
    url: String,
    sha256: String,
    archive: String,
    binary: String,
}

impl Installer {
    pub fn new(data_dir: PathBuf, download_base: Option<String>) -> Self {
        Self {
            data_dir,
            http: reqwest::Client::new(),
            download_base,
        }
    }

    pub async fn install(
        &self,
        name: &str,
        output: &tokio::sync::mpsc::Sender<String>,
    ) -> Result<PathBuf, String> {
        let packages: std::collections::BTreeMap<String, Package> =
            serde_json::from_str(include_str!("downloads.json"))
                .map_err(|error| error.to_string())?;
        let package = packages
            .get(name)
            .ok_or_else(|| format!("no pinned download for {name}"))?;
        let platform = platform_key().ok_or_else(|| {
            format!(
                "unsupported platform {}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        })?;
        let asset = package
            .platforms
            .get(platform)
            .ok_or_else(|| format!("no {platform} download for {name}"))?;
        let _ = output
            .send(format!("Installing {name} {}…", package.version))
            .await;
        self.install_asset(name, &package.version, asset).await
    }

    async fn install_asset(
        &self,
        name: &str,
        version: &str,
        asset: &Asset,
    ) -> Result<PathBuf, String> {
        let url = match &self.download_base {
            Some(base) => format!(
                "{}/{}",
                base.trim_end_matches('/'),
                asset.url.rsplit('/').next().unwrap_or("asset")
            ),
            None => asset.url.clone(),
        };
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("download returned HTTP {}", response.status()));
        }
        let bytes = response.bytes().await.map_err(|error| error.to_string())?;
        let digest = hex::encode(Sha256::digest(&bytes));
        if digest != asset.sha256 {
            return Err(format!(
                "sha256 mismatch for {name}: expected {}, got {digest}",
                asset.sha256
            ));
        }
        let root = self
            .data_dir
            .join("agent-tools")
            .join("lsp")
            .join(format!("{name}-{version}"));
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|error| error.to_string())?;
        let staged = root.join(&asset.binary);
        let binary = asset.binary.clone();
        let archive = asset.archive.clone();
        let bytes = bytes.to_vec();
        let staged_clone = staged.clone();
        tokio::task::spawn_blocking(move || {
            extract_asset(&bytes, &archive, &binary, &staged_clone)
        })
        .await
        .map_err(|error| error.to_string())??;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = tokio::fs::metadata(&staged)
                .await
                .map_err(|error| error.to_string())?
                .permissions();
            permissions.set_mode(0o755);
            tokio::fs::set_permissions(&staged, permissions)
                .await
                .map_err(|error| error.to_string())?;
        }
        let bin_dir = installed_bin_dir(&self.data_dir);
        tokio::fs::create_dir_all(&bin_dir)
            .await
            .map_err(|error| error.to_string())?;
        let target = bin_dir.join(&asset.binary);
        let temporary = bin_dir.join(format!(".{}-installing", asset.binary));
        let _ = tokio::fs::remove_file(&temporary).await;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&staged, &temporary).map_err(|error| error.to_string())?;
        }
        #[cfg(windows)]
        {
            tokio::fs::copy(&staged, &temporary)
                .await
                .map_err(|error| error.to_string())?;
        }
        if target.exists() {
            tokio::fs::remove_file(&target)
                .await
                .map_err(|error| error.to_string())?;
        }
        tokio::fs::rename(&temporary, &target)
            .await
            .map_err(|error| error.to_string())?;
        Ok(target)
    }
}

fn extract_asset(
    bytes: &[u8],
    archive: &str,
    binary: &str,
    destination: &Path,
) -> Result<(), String> {
    let mut output = Vec::new();
    match archive {
        "raw" => {
            output.extend_from_slice(bytes);
        }
        "gz" => {
            GzDecoder::new(bytes)
                .read_to_end(&mut output)
                .map_err(|error| error.to_string())?;
        }
        "tar.gz" => {
            let decoder = GzDecoder::new(bytes);
            let mut archive = tar::Archive::new(decoder);
            let mut found = false;
            for entry in archive.entries().map_err(|error| error.to_string())? {
                let mut entry = entry.map_err(|error| error.to_string())?;
                if entry
                    .path()
                    .map_err(|error| error.to_string())?
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name == binary)
                {
                    entry
                        .read_to_end(&mut output)
                        .map_err(|error| error.to_string())?;
                    found = true;
                    break;
                }
            }
            if !found {
                return Err(format!("{binary} missing from tarball"));
            }
        }
        "zip" => {
            let mut zip =
                zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| error.to_string())?;
            let mut found = false;
            for index in 0..zip.len() {
                let mut entry = zip.by_index(index).map_err(|error| error.to_string())?;
                if entry
                    .name()
                    .map_err(|error| error.to_string())?
                    .rsplit('/')
                    .next()
                    == Some(binary)
                {
                    entry
                        .read_to_end(&mut output)
                        .map_err(|error| error.to_string())?;
                    found = true;
                    break;
                }
            }
            if !found {
                return Err(format!("{binary} missing from zip"));
            }
        }
        _ => return Err(format!("unknown archive format {archive}")),
    }
    if output.is_empty() {
        return Err("downloaded binary is empty".into());
    }
    std::fs::write(destination, output).map_err(|error| error.to_string())
}

fn platform_key() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux-x64"),
        ("macos", "aarch64") => Some("darwin-arm64"),
        ("macos", "x86_64") => Some("darwin-x64"),
        ("windows", "x86_64") => Some("windows-x64"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {

    use axum::{Router, body::Body, routing::get};
    use flate2::{Compression, write::GzEncoder};
    use sha2::{Digest, Sha256};
    use tokio::net::TcpListener;

    use super::{Asset, Installer};

    fn fixture_tarball() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut compressed = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut archive = tar::Builder::new(&mut compressed);
            let bytes = b"#!/bin/sh\nexit 0\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            archive.append_data(&mut header, "ruff/ruff", &bytes[..])?;
            archive.finish()?;
        }
        Ok(compressed.finish()?)
    }

    #[tokio::test]
    async fn verified_tarball_is_installed_and_bad_checksum_is_refused()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = fixture_tarball()?;
        let digest = hex::encode(Sha256::digest(&bytes));
        let response_bytes = bytes.clone();
        let app = Router::new().route(
            "/fixture.tar.gz",
            get(move || {
                let response_bytes = response_bytes.clone();
                async move { Body::from(response_bytes) }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let _server = tokio::spawn(async move { axum::serve(listener, app).await });
        let temp = tempfile::tempdir()?;
        let installer =
            Installer::new(temp.path().to_path_buf(), Some(format!("http://{address}")));
        let asset = Asset {
            url: "https://example.invalid/fixture.tar.gz".into(),
            sha256: digest,
            archive: "tar.gz".into(),
            binary: "ruff".into(),
        };
        let installed = installer.install_asset("ruff", "test", &asset).await?;
        assert!(installed.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(&installed)?.permissions().mode() & 0o111,
                0
            );
        }
        let bad_asset = Asset {
            sha256: "00".repeat(32),
            ..asset
        };
        assert!(
            installer
                .install_asset("ruff", "bad", &bad_asset)
                .await
                .is_err()
        );
        Ok(())
    }
}
