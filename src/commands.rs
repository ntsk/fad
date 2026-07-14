use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use zip::ZipArchive;

use crate::api::{Client, Release};
use crate::apps;
use crate::auth;
use crate::config;
use crate::config::Config;

fn load_or_select_config() -> Result<Config> {
    match config::load_optional()? {
        Some(config) => Ok(config),
        None => {
            let token = auth::get_access_token()?;
            apps::select_and_save(&token)?;
            config::load()
        }
    }
}

pub fn projects() -> Result<()> {
    let token = auth::get_access_token()?;
    apps::print_projects(&token)
}

pub fn use_target(project_id: Option<&str>) -> Result<()> {
    let token = auth::get_access_token()?;
    match project_id {
        None => apps::select_and_save(&token),
        Some(project_id) => apps::select_app_in_project(&token, project_id),
    }
}

pub fn list() -> Result<()> {
    let config = load_or_select_config()?;
    let client = Client::new(&config)?;
    let releases = client.list_releases()?;
    if releases.is_empty() {
        println!("No releases found");
        return Ok(());
    }
    let rows: Vec<(String, String, &str, String, String)> = releases
        .iter()
        .map(|release| {
            (
                sanitize_display(release.id()),
                sanitize_display(&release.version()),
                release.binary_type(),
                format_time(&release.create_time),
                summarize(release.notes(), 50),
            )
        })
        .collect();
    let id_width = rows
        .iter()
        .map(|r| r.0.len())
        .max()
        .unwrap_or(0)
        .max("ID".len());
    let version_width = rows
        .iter()
        .map(|r| r.1.len())
        .max()
        .unwrap_or(0)
        .max("VERSION".len());
    println!(
        "{:<id_width$}  {:<version_width$}  {:<4}  {:<16}  NOTES",
        "ID", "VERSION", "TYPE", "CREATED"
    );
    for (id, version, binary_type, created, notes) in &rows {
        println!(
            "{id:<id_width$}  {version:<version_width$}  {binary_type:<4}  {created:<16}  {notes}"
        );
    }
    println!("\nRun `fad install <ID>` to install or `fad download <ID>` to save a release");
    Ok(())
}

pub fn install(id: &str) -> Result<()> {
    let release_id = normalize_release_id(id)?;
    let config = load_or_select_config()?;
    let client = Client::new(&config)?;
    let release = client.get_release(release_id)?;
    require_tool("adb")?;
    if release.binary_type() == "AAB" {
        require_tool("bundletool")?;
    }
    println!(
        "Installing release {} (version {})",
        release.id(),
        release.version()
    );

    let temp_dir = tempfile::tempdir().context("failed to create a temporary directory")?;
    let download_path = temp_dir.path().join("release.bin");
    println!("Downloading the app binary...");
    client.download(&release, &download_path)?;

    let apk_path = match detect_binary_kind(&download_path)? {
        BinaryKind::Apk => {
            let apk_path = temp_dir.path().join("app.apk");
            std::fs::rename(&download_path, &apk_path)
                .context("failed to move the downloaded file")?;
            apk_path
        }
        BinaryKind::Aab => {
            let aab_path = temp_dir.path().join("app.aab");
            std::fs::rename(&download_path, &aab_path)
                .context("failed to move the downloaded file")?;
            build_universal_apk(temp_dir.path(), &aab_path)?
        }
    };
    adb_install(&apk_path)?;
    println!("Install complete");
    Ok(())
}

pub fn download(id: &str, output: Option<PathBuf>) -> Result<()> {
    let release_id = normalize_release_id(id)?;
    let config = load_or_select_config()?;
    let client = Client::new(&config)?;
    let release = client.get_release(release_id)?;
    println!(
        "Downloading release {} (version {})",
        release.id(),
        release.version()
    );

    let dir = output.unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create directory {}", dir.display()))?;
    let temp = tempfile::Builder::new()
        .prefix(".fad-download-")
        .tempfile_in(&dir)
        .with_context(|| format!("failed to create a temporary file in {}", dir.display()))?;
    client.download(&release, temp.path())?;

    let extension = match detect_binary_kind(temp.path()) {
        Ok(BinaryKind::Apk) => "apk",
        Ok(BinaryKind::Aab) => "aab",
        Err(_) => match release.binary_type() {
            "APK" => "apk",
            "AAB" => "aab",
            _ => "bin",
        },
    };
    let dest = dir.join(download_file_name(&release, extension));
    temp.persist(&dest)
        .with_context(|| format!("failed to save {}", dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644))?;
    }
    println!("Saved to {}", dest.display());
    Ok(())
}

fn normalize_release_id(id: &str) -> Result<&str> {
    let release_id = id.rsplit('/').next().unwrap_or(id).trim();
    if release_id.is_empty()
        || !release_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        bail!("invalid release ID: \"{id}\" (run `fad releases` to see available IDs)");
    }
    Ok(release_id)
}

fn download_file_name(release: &Release, extension: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !release.display_version.is_empty() {
        parts.push(sanitize_file_part(&release.display_version));
    }
    if !release.build_version.is_empty() {
        parts.push(sanitize_file_part(&release.build_version));
    }
    parts.push(sanitize_file_part(release.id()));
    format!("{}.{extension}", parts.join("-"))
}

fn sanitize_file_part(part: &str) -> String {
    part.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
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

fn require_tool(name: &str) -> Result<()> {
    let found = std::env::var_os("PATH")
        .map(|paths| find_in_paths(&paths, name))
        .unwrap_or(false);
    if !found {
        bail!("{name} not found on PATH; make sure it is installed");
    }
    Ok(())
}

fn find_in_paths(paths: &OsStr, name: &str) -> bool {
    std::env::split_paths(paths).any(|dir| is_executable(&dir.join(name)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file() || path.with_extension("exe").is_file()
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

fn sanitize_display(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn summarize(notes: &str, max_chars: usize) -> String {
    let first_line = sanitize_display(notes.lines().next().unwrap_or(""));
    let first_line = first_line.trim();
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
        assert!(matches!(
            detect_binary_kind(&path).unwrap(),
            BinaryKind::Aab
        ));
    }

    #[test]
    fn detects_apk_by_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.bin");
        write_zip(&path, "AndroidManifest.xml");
        assert!(matches!(
            detect_binary_kind(&path).unwrap(),
            BinaryKind::Apk
        ));
    }

    #[test]
    fn rejects_unknown_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.bin");
        write_zip(&path, "something-else.txt");
        assert!(detect_binary_kind(&path).is_err());
    }

    #[test]
    fn finds_executables_in_path_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("sometool");
        std::fs::write(&tool, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let paths = std::env::join_paths([dir.path()]).unwrap();
        assert!(find_in_paths(&paths, "sometool"));
        assert!(!find_in_paths(&paths, "othertool"));
    }

    #[cfg(unix)]
    #[test]
    fn ignores_non_executable_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("sometool");
        std::fs::write(&tool, "").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        let paths = std::env::join_paths([dir.path()]).unwrap();
        assert!(!find_in_paths(&paths, "sometool"));
    }

    #[test]
    fn normalizes_release_id_from_bare_id_and_resource_name() {
        assert_eq!(normalize_release_id("abc123").unwrap(), "abc123");
        assert_eq!(
            normalize_release_id("projects/1/apps/1:1:android:a/releases/r1").unwrap(),
            "r1"
        );
    }

    #[test]
    fn rejects_invalid_release_ids() {
        assert!(normalize_release_id("").is_err());
        assert!(normalize_release_id("releases/").is_err());
        assert!(normalize_release_id("id with spaces").is_err());
        assert!(normalize_release_id("id?query").is_err());
    }

    #[test]
    fn builds_download_file_name_from_release() {
        let release: Release = serde_json::from_value(serde_json::json!({
            "name": "projects/1/apps/a/releases/r1",
            "displayVersion": "9.0.0",
            "buildVersion": "10090000"
        }))
        .unwrap();
        assert_eq!(download_file_name(&release, "apk"), "9.0.0-10090000-r1.apk");
    }

    #[test]
    fn download_file_name_falls_back_to_release_id() {
        let release: Release = serde_json::from_value(serde_json::json!({
            "name": "projects/1/apps/a/releases/r1"
        }))
        .unwrap();
        assert_eq!(download_file_name(&release, "aab"), "r1.aab");
    }

    #[test]
    fn sanitizes_unsafe_characters_in_file_name() {
        let release: Release = serde_json::from_value(serde_json::json!({
            "name": "projects/1/apps/a/releases/r1",
            "displayVersion": "1.0 beta/2"
        }))
        .unwrap();
        assert_eq!(download_file_name(&release, "apk"), "1.0_beta_2-r1.apk");
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

    #[test]
    fn sanitizes_control_characters_for_display() {
        assert_eq!(sanitize_display("a\x1b[0mb\tc"), "a [0mb c");
        assert_eq!(sanitize_display("plain text"), "plain text");
    }

    #[test]
    fn summarize_neutralizes_ansi_escapes_in_notes() {
        assert_eq!(summarize("boom\x1b[31mred", 50), "boom [31mred");
    }
}
