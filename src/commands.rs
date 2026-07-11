use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use zip::ZipArchive;

use crate::api::Client;
use crate::config;

pub fn list() -> Result<()> {
    let config = config::load()?;
    let client = Client::new(&config)?;
    let releases = client.list_releases()?;
    if releases.is_empty() {
        println!("No releases found");
        return Ok(());
    }
    let rows: Vec<(String, String, String, String)> = releases
        .iter()
        .map(|release| {
            (
                release.id().to_string(),
                release.version(),
                format_time(&release.create_time),
                summarize(release.notes(), 50),
            )
        })
        .collect();
    let id_width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max("ID".len());
    let version_width = rows.iter().map(|r| r.1.len()).max().unwrap_or(0).max("VERSION".len());
    println!(
        "{:<id_width$}  {:<version_width$}  {:<16}  {}",
        "ID", "VERSION", "CREATED", "NOTES"
    );
    for (id, version, created, notes) in &rows {
        println!("{id:<id_width$}  {version:<version_width$}  {created:<16}  {notes}");
    }
    println!("\nRun `fad install <ID>` to download and install a release");
    Ok(())
}

pub fn install(id: &str) -> Result<()> {
    let config = config::load()?;
    let client = Client::new(&config)?;
    let release_id = id.rsplit('/').next().unwrap_or(id);
    let release = client.get_release(release_id)?;
    println!("Installing release {} (version {})", release.id(), release.version());

    let temp_dir = tempfile::tempdir().context("failed to create a temporary directory")?;
    let download_path = temp_dir.path().join("release.bin");
    println!("Downloading the app binary...");
    client.download(&release, &download_path)?;

    let apk_path = match detect_binary_kind(&download_path)? {
        BinaryKind::Apk => {
            let apk_path = temp_dir.path().join("app.apk");
            std::fs::rename(&download_path, &apk_path)?;
            apk_path
        }
        BinaryKind::Aab => {
            let aab_path = temp_dir.path().join("app.aab");
            std::fs::rename(&download_path, &aab_path)?;
            build_universal_apk(temp_dir.path(), &aab_path)?
        }
    };
    adb_install(&apk_path)?;
    println!("Install complete");
    Ok(())
}

enum BinaryKind {
    Apk,
    Aab,
}

fn detect_binary_kind(path: &Path) -> Result<BinaryKind> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut archive =
        ZipArchive::new(file).context("the downloaded binary is not a valid APK/AAB")?;
    if archive.by_name("BundleConfig.pb").is_ok() {
        return Ok(BinaryKind::Aab);
    }
    if archive.by_name("AndroidManifest.xml").is_ok() {
        return Ok(BinaryKind::Apk);
    }
    bail!("could not determine whether the downloaded binary is an APK or an AAB")
}

fn build_universal_apk(work_dir: &Path, aab_path: &Path) -> Result<PathBuf> {
    println!("Building a universal APK with bundletool...");
    let apks_path = work_dir.join("app.apks");
    let status = Command::new("bundletool")
        .arg("build-apks")
        .arg(format!("--bundle={}", aab_path.display()))
        .arg(format!("--output={}", apks_path.display()))
        .arg("--mode=universal")
        .status()
        .context("failed to run bundletool; make sure it is installed and on PATH")?;
    if !status.success() {
        bail!("bundletool build-apks failed");
    }
    let apk_path = work_dir.join("universal.apk");
    let file = File::open(&apks_path)
        .with_context(|| format!("failed to open {}", apks_path.display()))?;
    let mut archive = ZipArchive::new(file).context("failed to read the bundletool output")?;
    let mut entry = archive
        .by_name("universal.apk")
        .context("universal.apk not found in the bundletool output")?;
    let mut out = File::create(&apk_path)
        .with_context(|| format!("failed to create {}", apk_path.display()))?;
    io::copy(&mut entry, &mut out)?;
    Ok(apk_path)
}

fn adb_install(apk_path: &Path) -> Result<()> {
    println!("Installing with adb...");
    let status = Command::new("adb")
        .arg("install")
        .arg("-r")
        .arg(apk_path)
        .status()
        .context("failed to run adb; make sure it is installed and on PATH")?;
    if !status.success() {
        bail!("adb install failed");
    }
    Ok(())
}

fn format_time(create_time: &str) -> String {
    create_time.replace('T', " ").chars().take(16).collect()
}

fn summarize(notes: &str, max_chars: usize) -> String {
    let first_line = notes.lines().next().unwrap_or("").trim();
    let mut summary: String = first_line.chars().take(max_chars).collect();
    if first_line.chars().count() > max_chars {
        summary.push('…');
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn write_zip(path: &Path, entry_name: &str) {
        let file = File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file(entry_name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"data").unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn detects_aab_by_bundle_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.bin");
        write_zip(&path, "BundleConfig.pb");
        assert!(matches!(detect_binary_kind(&path).unwrap(), BinaryKind::Aab));
    }

    #[test]
    fn detects_apk_by_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.bin");
        write_zip(&path, "AndroidManifest.xml");
        assert!(matches!(detect_binary_kind(&path).unwrap(), BinaryKind::Apk));
    }

    #[test]
    fn rejects_unknown_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.bin");
        write_zip(&path, "something-else.txt");
        assert!(detect_binary_kind(&path).is_err());
    }

    #[test]
    fn formats_create_time_for_display() {
        assert_eq!(format_time("2026-07-10T03:12:45.678Z"), "2026-07-10 03:12");
    }

    #[test]
    fn summarizes_notes_to_first_line_with_limit() {
        assert_eq!(summarize("first line\nsecond", 50), "first line");
        assert_eq!(summarize("abcdef", 3), "abc…");
        assert_eq!(summarize("", 10), "");
    }
}
