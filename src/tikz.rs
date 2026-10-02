use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// Compile a TikZ environment with the user's TeX preamble into a PNG preview.
pub fn compile(source: &str, preamble: &str) -> Result<Vec<u8>, String> {
    compile_tex(document(source, preamble))
}

fn compile_tex(tex: String) -> Result<Vec<u8>, String> {
    let cache = dirs::cache_dir()
        .ok_or("无法确定系统缓存目录")?
        .join("rusidian/tikz");
    fs::create_dir_all(&cache).map_err(|error| format!("无法创建 TikZ 缓存：{error}"))?;

    // The whole document is the key, so preamble changes produce new PDFs.
    let mut hasher = DefaultHasher::new();
    tex.hash(&mut hasher);
    let key = format!("{:016x}", hasher.finish());
    let pdf = cache.join(format!("{key}.pdf"));
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("系统时间错误：{error}"))?
        .as_nanos();
    let job = cache.join(format!(".{key}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&job).map_err(|error| format!("无法创建 TikZ 临时目录：{error}"))?;

    let result = (|| {
        if !pdf.exists() {
            let input = job.join("input.tex");
            fs::write(&input, &tex).map_err(|error| format!("无法写入 TeX：{error}"))?;

            let output = Command::new(crate::paths::executable("tectonic"))
                .args(["--untrusted", "--color", "never", "--outdir"])
                .arg(&job)
                .arg(&input)
                .output()
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        "找不到 Tectonic。TikZ 需要 Tectonic，例如 brew install tectonic".to_owned()
                    } else {
                        format!("无法启动 Tectonic：{error}")
                    }
                })?;

            if !output.status.success() {
                let message = if output.stderr.is_empty() {
                    &output.stdout
                } else {
                    &output.stderr
                };
                return Err(String::from_utf8_lossy(message).trim().to_owned());
            }

            fs::rename(job.join("input.pdf"), &pdf)
                .map_err(|error| format!("无法保存 TikZ PDF：{error}"))?;
        }

        let preview = job.join("preview.png");
        rasterize(&pdf, &preview)?;
        let image = fs::read(preview).map_err(|error| format!("无法读取 TikZ 预览：{error}"))?;
        let _ = prune_cache(&cache, MAX_CACHE_BYTES);
        Ok(image)
    })();

    let _ = fs::remove_dir_all(&job);

    result
}

/// Pixels per PDF point in the preview; the reading view draws it at `1 / PREVIEW_SCALE`.
pub const PREVIEW_SCALE: f32 = if cfg!(target_os = "macos") { 1.0 } else { 2.0 };

#[cfg(target_os = "macos")]
fn rasterize(pdf: &std::path::Path, png: &std::path::Path) -> Result<(), String> {
    let output = Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(pdf)
        .arg("--out")
        .arg(png)
        .output()
        .map_err(|error| format!("无法启动 PDF 预览转换：{error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn rasterize(pdf: &std::path::Path, png: &std::path::Path) -> Result<(), String> {
    // Poppler appends ".png" to the output root when writing a single page.
    let root = png.with_extension("");
    let output = Command::new(crate::paths::executable("pdftoppm"))
        .args(["-png", "-singlefile", "-r"])
        .arg((72.0 * PREVIEW_SCALE).to_string())
        .arg(pdf)
        .arg(&root)
        .output()
        .map_err(|error| format!("无法启动 PDF 预览转换（需要 Poppler 的 pdftoppm）：{error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(())
}

/// Width and height of a PNG from its IHDR chunk.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") || &png[12..16] != b"IHDR" {
        return None;
    }
    let read = |offset: usize| u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap());
    Some((read(16), read(20)))
}

fn prune_cache(cache: &std::path::Path, max_bytes: u64) -> Result<(), String> {
    let mut files = fs::read_dir(cache)
        .map_err(|error| format!("无法读取 TikZ 缓存：{error}"))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|value| value.to_str()) == Some("pdf"))
                .then(|| entry.metadata().ok().map(|metadata| (path, metadata)))?
        })
        .collect::<Vec<_>>();
    let mut bytes = files
        .iter()
        .map(|(_, metadata)| metadata.len())
        .sum::<u64>();
    files.sort_by_key(|(_, metadata)| metadata.modified().unwrap_or(UNIX_EPOCH));

    for (path, metadata) in files {
        if bytes <= max_bytes {
            break;
        }
        if fs::remove_file(path).is_ok() {
            bytes = bytes.saturating_sub(metadata.len());
        }
    }
    Ok(())
}

fn document(source: &str, preamble: &str) -> String {
    format!(
        "\\documentclass[tikz,border=2pt]{{standalone}}\n\\usepackage{{tikz}}\n{preamble}\n\\begin{{document}}\n{source}\n\\end{{document}}\n"
    )
}

/// The global TeX preamble file, or the one for `vault`. Both live in the system config
/// directory, never inside a vault.
pub fn preamble_path(vault: Option<&Path>) -> Option<PathBuf> {
    let folder = dirs::config_dir()?.join("rusidian/tex");
    Some(match vault {
        None => folder.join("preamble.tex"),
        Some(root) => {
            let name = root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            folder.join(format!("vaults/{name}-{:016x}.tex", stable_hash(root)))
        }
    })
}

/// Whether `path` is one of Rusidian's preamble files.
pub fn is_preamble(path: &Path) -> bool {
    dirs::config_dir().is_some_and(|config| path.starts_with(config.join("rusidian/tex")))
}

/// The global preamble followed by the vault's, as written by the user.
pub fn preamble(vault: Option<&Path>) -> String {
    [
        preamble_path(None),
        vault.and_then(|root| preamble_path(Some(root))),
    ]
    .into_iter()
    .flatten()
    .filter_map(|path| fs::read_to_string(path).ok())
    .collect::<Vec<_>>()
    .join("\n")
}

/// Create a preamble file with a short explanation if it does not exist yet.
pub fn ensure_preamble(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let parent = path.parent().ok_or("无法确定设置目录")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建设置目录：{error}"))?;
    fs::write(
        path,
        "% Rusidian 在编译 TikZ 时把这里的内容放在 \\begin{document} 之前。\n% 例如：\\usepackage{tikz-cd} 或 \\usetikzlibrary{arrows.meta}\n",
    )
    .map_err(|error| format!("无法创建前导内容文件：{error}"))
}

/// FNV-1a, stable across Rust versions so per-vault files keep their names.
fn stable_hash(path: &Path) -> u64 {
    path.to_string_lossy()
        .bytes()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_tikz_environment_in_document() {
        let tex = document(
            "\\begin{tikzpicture}\\draw (0,0)--(1,1);\\end{tikzpicture}",
            "\\usepackage{tikz-cd}",
        );
        assert!(tex.contains("\\usepackage{tikz}"));
        let preamble = tex.find("\\usepackage{tikz-cd}").unwrap();
        assert!(preamble < tex.find("\\begin{document}").unwrap());
        let global = preamble_path(None).unwrap();
        let vault = preamble_path(Some(Path::new("/notes/My Vault"))).unwrap();
        assert!(is_preamble(&global) && is_preamble(&vault));
        assert!(vault.to_string_lossy().contains("My Vault-"));
        assert_eq!(stable_hash(Path::new("/a")), stable_hash(Path::new("/a")));
        assert!(!is_preamble(Path::new("/notes/My Vault/note.md")));
    }

    #[test]
    fn bounds_pdf_cache_size() {
        let cache =
            std::env::temp_dir().join(format!("rusidian-cache-test-{}", std::process::id()));
        fs::create_dir_all(&cache).unwrap();
        for name in ["a.pdf", "b.pdf", "c.pdf"] {
            fs::write(cache.join(name), [0_u8; 2]).unwrap();
        }

        prune_cache(&cache, 4).unwrap();
        let bytes = fs::read_dir(&cache)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.metadata().unwrap().len())
            .sum::<u64>();
        assert!(bytes <= 4);
        fs::remove_dir_all(cache).unwrap();
    }

    #[test]
    fn reads_png_dimensions() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&300_u32.to_be_bytes());
        png.extend_from_slice(&120_u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((300, 120)));
        assert_eq!(png_size(b"not a png"), None);
    }

    #[test]
    #[ignore = "requires the external Tectonic installation"]
    fn compiles_simple_tikz() {
        let png = compile(
            "\\begin{tikzpicture}\\draw (0,0)--(1,1);\\end{tikzpicture}",
            "",
        )
        .expect("TikZ should compile");
        assert!(png.starts_with(b"\x89PNG"));
    }
}
