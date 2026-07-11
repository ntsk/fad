use anyhow::{Context, Result};
use dialoguer::theme::ColorfulTheme;
use dialoguer::Select;
use serde::Deserialize;

use crate::api;
use crate::config;
use crate::config::Config;

const BASE_URL: &str = "https://firebase.googleapis.com/v1beta1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListProjectsResponse {
    #[serde(default)]
    results: Vec<Project>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    project_id: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListAndroidAppsResponse {
    #[serde(default)]
    apps: Vec<AndroidApp>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AndroidApp {
    app_id: String,
    #[serde(default)]
    package_name: Option<String>,
}

pub fn select_and_save(token: &str) -> Result<()> {
    let http = reqwest::blocking::Client::new();
    let mut projects = list_projects(&http, token)?;
    if projects.is_empty() {
        println!("No Firebase projects are accessible with this account");
        return Ok(());
    }
    projects.sort_by(|a, b| a.project_id.cmp(&b.project_id));

    let project_items: Vec<String> = projects.iter().map(project_label).collect();
    let Some(project_index) = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select a Firebase project")
        .items(&project_items)
        .default(0)
        .interact_opt()?
    else {
        return skipped();
    };
    let project = &projects[project_index];

    let mut apps = list_android_apps(&http, token, &project.project_id)?;
    if apps.is_empty() {
        println!("No Android apps found in project {}", project.project_id);
        return Ok(());
    }
    apps.sort_by(|a, b| a.package_name.cmp(&b.package_name));

    let current_app_id = config::load_optional()?.map(|c| c.app_id);
    let default_index = current_app_id
        .as_deref()
        .and_then(|current| apps.iter().position(|app| app.app_id == current))
        .unwrap_or(0);
    let app_items: Vec<String> = apps.iter().map(app_label).collect();
    let Some(app_index) = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select an Android app")
        .items(&app_items)
        .default(default_index)
        .interact_opt()?
    else {
        return skipped();
    };
    let app = &apps[app_index];

    let oauth = config::load_optional()?
        .map(|c| c.oauth)
        .unwrap_or_default();
    config::save(&Config {
        app_id: app.app_id.clone(),
        oauth,
    })?;
    println!(
        "Saved app_id {} to {}",
        app.app_id,
        config::config_path()?.display()
    );
    Ok(())
}

fn skipped() -> Result<()> {
    println!(
        "Selection skipped; set app_id in {} manually",
        config::config_path()?.display()
    );
    Ok(())
}

fn project_label(project: &Project) -> String {
    match project
        .display_name
        .as_deref()
        .filter(|name| !name.is_empty() && *name != project.project_id)
    {
        Some(name) => format!("{name} ({})", project.project_id),
        None => project.project_id.clone(),
    }
}

fn app_label(app: &AndroidApp) -> String {
    match app.package_name.as_deref().filter(|name| !name.is_empty()) {
        Some(package) => format!("{package} ({})", app.app_id),
        None => app.app_id.clone(),
    }
}

fn list_projects(http: &reqwest::blocking::Client, token: &str) -> Result<Vec<Project>> {
    let mut projects = Vec::new();
    let mut page_token = String::new();
    loop {
        let mut request = http
            .get(format!("{BASE_URL}/projects"))
            .query(&[("pageSize", "100")])
            .bearer_auth(token);
        if !page_token.is_empty() {
            request = request.query(&[("pageToken", page_token.as_str())]);
        }
        let resp = request
            .send()
            .context("failed to reach the Firebase Management API")?;
        let resp = api::check(resp)?;
        let list: ListProjectsResponse = resp.json().context("failed to parse the project list")?;
        projects.extend(list.results);
        match list.next_page_token {
            Some(next) if !next.is_empty() => page_token = next,
            _ => break,
        }
    }
    Ok(projects)
}

fn list_android_apps(
    http: &reqwest::blocking::Client,
    token: &str,
    project_id: &str,
) -> Result<Vec<AndroidApp>> {
    let mut apps = Vec::new();
    let mut page_token = String::new();
    loop {
        let mut request = http
            .get(format!("{BASE_URL}/projects/{project_id}/androidApps"))
            .query(&[("pageSize", "100")])
            .bearer_auth(token);
        if !page_token.is_empty() {
            request = request.query(&[("pageToken", page_token.as_str())]);
        }
        let resp = request
            .send()
            .context("failed to reach the Firebase Management API")?;
        let resp = api::check(resp)?;
        let list: ListAndroidAppsResponse = resp
            .json()
            .context("failed to parse the Android app list")?;
        apps.extend(list.apps);
        match list.next_page_token {
            Some(next) if !next.is_empty() => page_token = next,
            _ => break,
        }
    }
    Ok(apps)
}
