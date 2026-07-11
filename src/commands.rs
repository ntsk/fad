use anyhow::Result;

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
