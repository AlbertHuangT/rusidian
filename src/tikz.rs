use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

pub fn compile(source: &str) -> Result<Vec<u8>, String> {
    compile_tex(source, 1, document(source))
}

fn compile_tex(source: &str, template_version: u8, tex: String) -> Result<Vec<u8>, String> {
    let cache = dirs::cache_dir()
        .ok_or("无法确定系统缓存目录")?
        .join("rusidian/tikz");
    fs::create_dir_all(&cache).map_err(|error| format!("无法创建 TikZ 缓存：{error}"))?;

    let mut hasher = DefaultHasher::new();
    template_version.hash(&mut hasher);
    source.hash(&mut hasher);
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

            let output = Command::new("tectonic")
                .args(["--untrusted", "--color", "never", "--outdir"])
                .arg(&job)
                .arg(&input)
                .output()
                .map_err(|error| format!("无法启动 Tectonic：{error}"))?;

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
        let mut command = Command::new("sips");
        command.args(["-s", "format", "png"]);
        let output = command
            .arg(&pdf)
            .arg("--out")
            .arg(&preview)
            .output()
            .map_err(|error| format!("无法启动 PDF 预览转换：{error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }

        let image = fs::read(preview).map_err(|error| format!("无法读取 TikZ 预览：{error}"))?;
        let _ = prune_cache(&cache, MAX_CACHE_BYTES);
        Ok(image)
    })();

    let _ = fs::remove_dir_all(&job);

    result
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

fn document(source: &str) -> String {
    format!(
        "\\documentclass[tikz,border=2pt]{{standalone}}\n\\usepackage{{tikz}}\n\\begin{{document}}\n{source}\n\\end{{document}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_tikz_environment_in_document() {
        let tex = document("\\begin{tikzpicture}\\draw (0,0)--(1,1);\\end{tikzpicture}");
        assert!(tex.contains("\\usepackage{tikz}"));
        assert!(tex.contains("\\begin{document}"));
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
    #[ignore = "requires the external Tectonic installation"]
    fn compiles_simple_tikz() {
        let png = compile("\\begin{tikzpicture}\\draw (0,0)--(1,1);\\end{tikzpicture}")
            .expect("TikZ should compile");
        assert!(png.starts_with(b"\x89PNG"));
    }
}
