use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const TEMPLATE_VERSION: u8 = 1;

pub fn compile(source: &str) -> Result<Vec<u8>, String> {
    let cache = dirs::cache_dir()
        .ok_or("无法确定系统缓存目录")?
        .join("rusidian/tikz");
    fs::create_dir_all(&cache).map_err(|error| format!("无法创建 TikZ 缓存：{error}"))?;

    let mut hasher = DefaultHasher::new();
    TEMPLATE_VERSION.hash(&mut hasher);
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
            fs::write(&input, document(source))
                .map_err(|error| format!("无法写入 TeX：{error}"))?;

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
        let output = Command::new("sips")
            .args(["-s", "format", "png"])
            .arg(&pdf)
            .arg("--out")
            .arg(&preview)
            .output()
            .map_err(|error| format!("无法启动 PDF 预览转换：{error}"))?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }

        fs::read(preview).map_err(|error| format!("无法读取 TikZ 预览：{error}"))
    })();

    let _ = fs::remove_dir_all(&job);

    // ponytail: add LRU eviction when UI integration provides real cache-size measurements.
    result
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
    #[ignore = "requires the external Tectonic installation"]
    fn compiles_simple_tikz() {
        let png = compile("\\begin{tikzpicture}\\draw (0,0)--(1,1);\\end{tikzpicture}")
            .expect("TikZ should compile");
        assert!(png.starts_with(b"\x89PNG"));
    }
}
