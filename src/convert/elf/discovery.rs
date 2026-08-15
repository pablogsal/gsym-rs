#[cfg(feature = "debuginfod")]
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use object::{Object, ObjectSection};

use super::super::ConversionWarning;
use super::ConversionOptions;
use super::image::{cross_check_elf_identity, malformed, parse_elf};
use crate::{ElfInputKind, Error, Result};

#[derive(Debug, Default)]
pub(super) struct Discovery {
    pub(super) artifact: Option<DiscoveredArtifact>,
    pub(super) warnings: Vec<ConversionWarning>,
}

#[derive(Debug)]
pub(super) struct DiscoveredArtifact {
    pub(super) path: PathBuf,
    pub(super) bytes: Vec<u8>,
}

pub(super) fn discover_dwp(image_path: &Path, debug_path: &Path) -> Option<DiscoveredArtifact> {
    let mut candidates = Vec::new();
    for path in [image_path, debug_path] {
        candidates.push(path.with_extension("dwp"));
        let mut appended = path.as_os_str().to_os_string();
        appended.push(".dwp");
        candidates.push(PathBuf::from(appended));
    }
    candidates.sort();
    candidates.dedup();
    candidates.into_iter().find_map(|candidate| {
        let Ok(bytes) = std::fs::read(&candidate) else {
            return None;
        };
        let Ok(file) = parse_elf(&bytes, ElfInputKind::Dwp) else {
            return None;
        };
        file.section_by_name(".debug_cu_index")?;
        Some(DiscoveredArtifact {
            path: candidate,
            bytes,
        })
    })
}

pub(super) fn validated_gnu_debugdata(
    image: &object::File<'_>,
    maximum_size: u64,
    warnings: &mut Vec<ConversionWarning>,
) -> Option<Vec<u8>> {
    let result = unpack_gnu_debugdata(image, maximum_size).and_then(|data| {
        if let Some(bytes) = &data {
            let debug = parse_elf(bytes, ElfInputKind::EmbeddedDebugData)?;
            cross_check_elf_identity(image, &debug, ElfInputKind::EmbeddedDebugData)?;
        }
        Ok(data)
    });
    match result {
        Ok(data) => data,
        Err(error) => {
            warnings.push(ConversionWarning::EmbeddedDebugData {
                reason: error.to_string().into_boxed_str(),
            });
            None
        }
    }
}

fn unpack_gnu_debugdata(image: &object::File<'_>, maximum_size: u64) -> Result<Option<Vec<u8>>> {
    let Some(section) = image.section_by_name(".gnu_debugdata") else {
        return Ok(None);
    };
    let compressed = section
        .data()
        .map_err(|error| malformed(".gnu_debugdata", error))?;
    let mut input = std::io::Cursor::new(compressed);
    let mut output = LimitedOutput::new(maximum_size);
    lzma_rs::xz_decompress(&mut input, &mut output)
        .map_err(|error| Error::malformed(".gnu_debugdata XZ stream", error.to_string()))?;
    Ok(Some(output.into_inner()))
}

struct LimitedOutput {
    bytes: Vec<u8>,
    limit: u64,
}

impl LimitedOutput {
    const fn new(limit: u64) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl std::io::Write for LimitedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let new_len = u64::try_from(self.bytes.len())
            .unwrap_or(u64::MAX)
            .saturating_add(bytes.len() as u64);
        if new_len > self.limit {
            return Err(std::io::Error::other(format!(
                "decompressed data exceeds the {}-byte limit",
                self.limit
            )));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn discover_supplementary(
    debug_path: &Path,
    debug_bytes: &[u8],
    options: &ConversionOptions,
) -> Result<Discovery> {
    let debug = parse_elf(debug_bytes, ElfInputKind::Debug)?;
    let Some((name, expected_id)) = debug
        .gnu_debugaltlink()
        .map_err(|error| malformed(".gnu_debugaltlink", error))?
    else {
        return Ok(Discovery::default());
    };
    let name = String::from_utf8_lossy(name);
    let mut candidates = Vec::new();
    if let Some(parent) = debug_path.parent() {
        candidates.push(parent.join(name.as_ref()));
    }
    if expected_id.len() >= 2 {
        let directory = hex::encode(expected_id.get(..1).unwrap_or_default());
        let filename = format!(
            "{}.debug",
            hex::encode(expected_id.get(1..).unwrap_or_default())
        );
        for root in &options.debug_directories {
            candidates.push(root.join(".build-id").join(&directory).join(&filename));
        }
    }
    for candidate in candidates {
        let Ok(bytes) = std::fs::read(&candidate) else {
            continue;
        };
        let Ok(file) = parse_elf(&bytes, ElfInputKind::Supplementary) else {
            continue;
        };
        let id = file
            .build_id()
            .map_err(|error| malformed("supplementary build ID", error))?;
        if id == Some(expected_id) {
            return Ok(Discovery {
                artifact: Some(DiscoveredArtifact {
                    path: candidate,
                    bytes,
                }),
                warnings: Vec::new(),
            });
        }
    }
    Ok(discover_with_debuginfod(
        expected_id,
        None,
        options,
        "supplementary debug file",
    ))
}

pub(super) fn discover_separate_debug(
    image_path: &Path,
    image_bytes: &[u8],
    options: &ConversionOptions,
) -> Result<Discovery> {
    let image = parse_elf(image_bytes, ElfInputKind::Image)?;
    if let Some((name, expected_crc)) = debug_link(&image)? {
        let mut candidates = Vec::new();
        if let Some(parent) = image_path.parent() {
            candidates.push(parent.join(&name));
            candidates.push(parent.join(".debug").join(&name));
        }
        for root in &options.debug_directories {
            if image_path.is_absolute() {
                let relative = image_path
                    .strip_prefix(Path::new("/"))
                    .unwrap_or(image_path);
                if let Some(parent) = relative.parent() {
                    candidates.push(root.join(parent).join(&name));
                }
            }
        }
        for candidate in candidates {
            let Ok(bytes) = std::fs::read(&candidate) else {
                continue;
            };
            if crc32fast::hash(&bytes) == expected_crc && valid_debug_companion(&image, &bytes) {
                return Ok(Discovery {
                    artifact: Some(DiscoveredArtifact {
                        path: candidate,
                        bytes,
                    }),
                    warnings: Vec::new(),
                });
            }
        }
    }

    if let Some(build_id) = image
        .build_id()
        .map_err(|error| malformed("ELF build ID", error))?
        && build_id.len() >= 2
    {
        let directory = hex::encode(build_id.get(..1).unwrap_or_default());
        let filename = format!(
            "{}.debug",
            hex::encode(build_id.get(1..).unwrap_or_default())
        );
        for root in &options.debug_directories {
            let candidate = root.join(".build-id").join(&directory).join(&filename);
            let Ok(bytes) = std::fs::read(&candidate) else {
                continue;
            };
            if valid_debug_companion(&image, &bytes) {
                return Ok(Discovery {
                    artifact: Some(DiscoveredArtifact {
                        path: candidate,
                        bytes,
                    }),
                    warnings: Vec::new(),
                });
            }
        }
        return Ok(discover_with_debuginfod(
            build_id,
            Some(&image),
            options,
            "debug file",
        ));
    }
    Ok(Discovery::default())
}

fn discover_with_debuginfod(
    build_id: &[u8],
    image: Option<&object::File<'_>>,
    options: &ConversionOptions,
    kind: &'static str,
) -> Discovery {
    if build_id.is_empty() {
        return Discovery::default();
    }
    let build_id_hex = hex::encode(build_id);
    let cache_path = options
        .debuginfod_cache
        .join(&build_id_hex)
        .join("debuginfo");
    if let Ok(bytes) = std::fs::read(&cache_path)
        && valid_debuginfod_response(image, build_id, &bytes)
    {
        return Discovery {
            artifact: Some(DiscoveredArtifact {
                path: cache_path,
                bytes,
            }),
            warnings: Vec::new(),
        };
    }
    fetch_debuginfod(build_id, &build_id_hex, image, options, kind, &cache_path)
}

#[cfg(feature = "debuginfod")]
fn fetch_debuginfod(
    build_id: &[u8],
    build_id_hex: &str,
    image: Option<&object::File<'_>>,
    options: &ConversionOptions,
    kind: &'static str,
    cache_path: &Path,
) -> Discovery {
    let mut warnings = Vec::new();
    let http_client = match debuginfod_client::reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            warnings.push(ConversionWarning::Debuginfod {
                endpoint: None,
                reason: format!("failed to create HTTP client: {error}").into_boxed_str(),
            });
            return Discovery {
                artifact: None,
                warnings,
            };
        }
    };
    let requested_build_id = debuginfod_client::BuildId::raw(build_id);
    for server in &options.debuginfod_urls {
        let client = match debuginfod_client::Client::builder()
            .http_client(http_client.clone())
            .build([server.as_str()])
        {
            Ok(Some(client)) => client,
            Ok(None) => continue,
            Err(error) => {
                warnings.push(ConversionWarning::Debuginfod {
                    endpoint: Some(server.clone().into_boxed_str()),
                    reason: format!("invalid server: {error}").into_boxed_str(),
                });
                continue;
            }
        };
        let response = match client.fetch_debug_info(&requested_build_id) {
            Ok(Some(response)) => response,
            Ok(None) => continue,
            Err(error) => {
                warnings.push(ConversionWarning::Debuginfod {
                    endpoint: Some(server.clone().into_boxed_str()),
                    reason: format!("request for {build_id_hex} failed: {error}").into_boxed_str(),
                });
                continue;
            }
        };
        let response_url = response.server_url.to_owned();
        let limit = options.debuginfod_max_download_size;
        let mut reader = response.data.take(limit.saturating_add(1));
        let mut bytes = Vec::new();
        if let Err(error) = reader.read_to_end(&mut bytes) {
            warnings.push(ConversionWarning::Debuginfod {
                endpoint: Some(response_url.into_boxed_str()),
                reason: format!("response was rejected: {error}").into_boxed_str(),
            });
            continue;
        }
        if bytes.len() as u64 > limit {
            warnings.push(ConversionWarning::Debuginfod {
                endpoint: Some(response_url.into_boxed_str()),
                reason: format!("response was rejected: response exceeds the {limit}-byte limit")
                    .into_boxed_str(),
            });
            continue;
        }
        if !valid_debuginfod_response(image, build_id, &bytes) {
            warnings.push(ConversionWarning::Debuginfod {
                endpoint: Some(response_url.into_boxed_str()),
                reason: format!(
                    "response does not match the requested {kind} build ID or architecture"
                )
                .into_boxed_str(),
            });
            continue;
        }
        match persist_debuginfod(cache_path, &bytes) {
            Ok(()) => {
                return Discovery {
                    artifact: Some(DiscoveredArtifact {
                        path: cache_path.to_path_buf(),
                        bytes,
                    }),
                    warnings,
                };
            }
            Err(error) => warnings.push(ConversionWarning::Debuginfod {
                endpoint: Some(cache_path.display().to_string().into_boxed_str()),
                reason: format!("failed to cache response: {error}").into_boxed_str(),
            }),
        }
    }
    Discovery {
        artifact: None,
        warnings,
    }
}

#[cfg(not(feature = "debuginfod"))]
fn fetch_debuginfod(
    _build_id: &[u8],
    _build_id_hex: &str,
    _image: Option<&object::File<'_>>,
    options: &ConversionOptions,
    _kind: &'static str,
    _cache_path: &Path,
) -> Discovery {
    let warnings = (!options.debuginfod_urls.is_empty())
        .then(|| ConversionWarning::Debuginfod {
            endpoint: None,
            reason: "URLs were configured, but the `debuginfod` feature is disabled".into(),
        })
        .into_iter()
        .collect();
    Discovery {
        artifact: None,
        warnings,
    }
}

fn valid_debuginfod_response(
    image: Option<&object::File<'_>>,
    expected_build_id: &[u8],
    bytes: &[u8],
) -> bool {
    let Ok(debug) = parse_elf(bytes, ElfInputKind::Debug) else {
        return false;
    };
    let Ok(actual_build_id) = debug.build_id() else {
        return false;
    };
    if actual_build_id != Some(expected_build_id) {
        return false;
    }
    image.is_none_or(|image| cross_check_elf_identity(image, &debug, ElfInputKind::Debug).is_ok())
}

#[cfg(feature = "debuginfod")]
fn persist_debuginfod(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("debuginfod cache path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let temporary = parent.join(format!(".debuginfo.{}.{nonce}.part", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    let linked = std::fs::hard_link(&temporary, path);
    drop(std::fs::remove_file(&temporary));
    linked
}

fn valid_debug_companion(image: &object::File<'_>, bytes: &[u8]) -> bool {
    parse_elf(bytes, ElfInputKind::Debug)
        .and_then(|debug| cross_check_elf_identity(image, &debug, ElfInputKind::Debug))
        .is_ok()
}

/// Reads `.gnu_debuglink`, rejecting the empty filename `object` tolerates.
///
/// `object` decodes the NUL-terminated name, the four-byte alignment before the
/// checksum, and the checksum itself in the image's own byte order.
fn debug_link(file: &object::File<'_>) -> Result<Option<(PathBuf, u32)>> {
    let Some((name, crc)) = file
        .gnu_debuglink()
        .map_err(|error| malformed(".gnu_debuglink", error))?
    else {
        return Ok(None);
    };
    if name.is_empty() {
        return Err(Error::malformed(".gnu_debuglink", "filename is empty"));
    }
    let name = String::from_utf8_lossy(name);
    Ok(Some((PathBuf::from(name.as_ref()), crc)))
}
