use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use zip::ZipArchive;

use crate::api::{Client, Release, UploadResult};
use crate::apps;
use crate::auth;
use crate::config;
use crate::config::Config;

#[derive(Default)]
pub struct SigningOptions {
    pub ks: Option<PathBuf>,
    pub ks_pass: Option<String>,
    pub ks_key_alias: Option<String>,
    pub key_pass: Option<String>,
}

impl SigningOptions {
    fn to_bundletool_args(&self) -> Result<Vec<String>> {
        if self.ks.is_none()
            && (self.ks_pass.is_some() || self.ks_key_alias.is_some() || self.key_pass.is_some())
        {
            bail!("--ks is required when providing keystore signing options");
        }
        let mut args = Vec::new();
        if let Some(ks) = &self.ks {
            args.push(format!("--ks={}", ks.display()));
        }
        if let Some(pass) = &self.ks_pass {
            args.push(format!("--ks-pass={pass}"));
        }
        if let Some(alias) = &self.ks_key_alias {
            args.push(format!("--ks-key-alias={alias}"));
        }
        if let Some(pass) = &self.key_pass {
            args.push(format!("--key-pass={pass}"));
        }
        Ok(args)
    }
}

fn find_debug_keystore(bases: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    bases
        .into_iter()
        .filter(|base| base.is_dir())
        .map(|base| base.join(".android").join("debug.keystore"))
        .find(|path| path.is_file())
}

fn debug_keystore() -> Option<PathBuf> {
    let bases = [
        std::env::var_os("ANDROID_SDK_HOME").map(PathBuf::from),
        dirs::home_dir(),
        std::env::var_os("HOME").map(PathBuf::from),
    ];
    find_debug_keystore(bases.into_iter().flatten())
}

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

fn resolve_config(app: Option<&str>) -> Result<Config> {
    match app {
        Some(app_id) => {
            let config = Config {
                app_id: app_id.to_string(),
                oauth: config::OauthConfig::default(),
            };
            config.project_number()?;
            Ok(config)
        }
        None => load_or_select_config(),
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

pub fn list(app: Option<&str>) -> Result<()> {
    let config = resolve_config(app)?;
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

pub fn upload(file: &Path, notes: Option<&str>, app: Option<&str>) -> Result<()> {
    if !file.is_file() {
        bail!("file not found: {}", file.display());
    }
    detect_binary_kind(file)
        .with_context(|| format!("{} is not a valid APK or AAB", file.display()))?;
    let config = resolve_config(app)?;
    let client = Client::new(&config)?;
    println!("Uploading {}...", file.display());
    let (release, result) = client.upload_release(file)?;
    println!("{}", upload_message(&release, result));
    if let Some(notes) = notes {
        client.set_release_notes(&release.name, notes)?;
        println!("Release notes set");
    }
    println!("Run `fad releases` to see it");
    Ok(())
}

fn upload_message(release: &Release, result: UploadResult) -> String {
    match result {
        UploadResult::Created => format!(
            "Release created: {} (version {})",
            release.id(),
            release.version()
        ),
        UploadResult::Updated => format!(
            "Release updated: {} (version {})",
            release.id(),
            release.version()
        ),
        UploadResult::Unmodified => format!(
            "This binary already exists as release {} (version {}); no new release was created",
            release.id(),
            release.version()
        ),
    }
}

pub fn install(id: &str, signing: &SigningOptions, app: Option<&str>) -> Result<()> {
    let signing_args = signing.to_bundletool_args()?;
    let release_id = normalize_release_id(id)?;
    let config = resolve_config(app)?;
    let client = Client::new(&config)?;
    let release = client.get_release(release_id)?;
    require_tool("adb")?;
    if release.binary_type() == "AAB" {
        require_tool("bundletool")?;
        // With no keystore, bundletool emits an unsigned (and thus uninstallable) APK
        // and still exits successfully, so the failure would only surface at `adb install`.
        // fad always installs the result, so require a signing key up front.
        if signing_args.is_empty() && debug_keystore().is_none() {
            bail!(
                "no keystore available to sign the AAB; installing it would fail.\nPass --ks (with --ks-pass / --ks-key-alias / --key-pass), or create a debug keystore at ~/.android/debug.keystore"
            );
        }
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
            require_tool("bundletool")?;
            let aab_path = temp_dir.path().join("app.aab");
            std::fs::rename(&download_path, &aab_path)
                .context("failed to move the downloaded file")?;
            build_universal_apk(temp_dir.path(), &aab_path, &signing_args)?
        }
    };
    adb_install(&apk_path)?;
    println!("Install complete");
    Ok(())
}

pub fn download(id: &str, output: Option<PathBuf>, app: Option<&str>) -> Result<()> {
    let release_id = normalize_release_id(id)?;
    let config = resolve_config(app)?;
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

fn build_universal_apk(
    work_dir: &Path,
    aab_path: &Path,
    signing_args: &[String],
) -> Result<PathBuf> {
    println!("Building a universal APK with bundletool...");
    let apks_path = work_dir.join("app.apks");
    let status = Command::new("bundletool")
        .arg("build-apks")
        .arg(format!("--bundle={}", aab_path.display()))
        .arg(format!("--output={}", apks_path.display()))
        .arg("--mode=universal")
        .args(signing_args)
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

    #[test]
    fn signing_args_are_empty_without_options() {
        let opts = SigningOptions::default();
        assert!(opts.to_bundletool_args().unwrap().is_empty());
    }

    #[test]
    fn signing_args_include_all_bundletool_flags() {
        let opts = SigningOptions {
            ks: Some(PathBuf::from("/keys/release.jks")),
            ks_pass: Some("pass:secret".to_string()),
            ks_key_alias: Some("release".to_string()),
            key_pass: Some("file:/keys/key.txt".to_string()),
        };
        assert_eq!(
            opts.to_bundletool_args().unwrap(),
            vec![
                "--ks=/keys/release.jks".to_string(),
                "--ks-pass=pass:secret".to_string(),
                "--ks-key-alias=release".to_string(),
                "--key-pass=file:/keys/key.txt".to_string(),
            ]
        );
    }

    #[test]
    fn signing_args_require_keystore_when_other_options_set() {
        let opts = SigningOptions {
            ks_key_alias: Some("release".to_string()),
            ..SigningOptions::default()
        };
        let err = opts.to_bundletool_args().unwrap_err();
        assert!(err.to_string().contains("--ks"));
    }

    #[test]
    fn finds_debug_keystore_in_first_matching_base() {
        let missing = tempfile::tempdir().unwrap();
        let present = tempfile::tempdir().unwrap();
        let ks = present.path().join(".android").join("debug.keystore");
        std::fs::create_dir_all(ks.parent().unwrap()).unwrap();
        std::fs::write(&ks, "keystore").unwrap();

        let found =
            find_debug_keystore([missing.path().to_path_buf(), present.path().to_path_buf()]);

        assert_eq!(found, Some(ks));
    }

    #[test]
    fn returns_none_when_no_base_has_debug_keystore() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_debug_keystore([dir.path().to_path_buf()]).is_none());
    }

    #[test]
    fn app_flag_overrides_config_target() {
        let config = resolve_config(Some("1:1234567890:android:abc")).unwrap();
        assert_eq!(config.app_id, "1:1234567890:android:abc");
        assert_eq!(config.project_number().unwrap(), "1234567890");
    }

    #[test]
    fn app_flag_rejects_invalid_app_id() {
        assert!(resolve_config(Some("not-an-app-id")).is_err());
    }

    #[test]
    fn upload_message_varies_by_result() {
        let release: Release = serde_json::from_value(serde_json::json!({
            "name": "projects/1/apps/a/releases/r7",
            "displayVersion": "1.0",
            "buildVersion": "1"
        }))
        .unwrap();

        assert_eq!(
            upload_message(&release, UploadResult::Created),
            "Release created: r7 (version 1.0 (1))"
        );
        assert_eq!(
            upload_message(&release, UploadResult::Updated),
            "Release updated: r7 (version 1.0 (1))"
        );
        assert_eq!(
            upload_message(&release, UploadResult::Unmodified),
            "This binary already exists as release r7 (version 1.0 (1)); no new release was created"
        );
    }
}
