use crate::{
    about::{REPOSITORY, VERSION},
    preview::settings_path,
};
use eframe::egui;
use semver::Version;
use serde_json::{Value, json};
use std::{
    fs, io,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_JSON: u64 = 256 * 1024;
const CHECK_INTERVAL: u64 = 24 * 60 * 60;
const EXECUTABLE: &str = "SourceSoundscapeStudio.exe";

fn repository(url: &str) -> Option<&str> {
    let repo = url.strip_prefix("https://github.com/")?;
    let (owner, name) = repo.split_once('/')?;
    [owner, name]
        .into_iter()
        .all(|part| {
            !matches!(part, "" | "." | "..")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
        .then_some(repo)
}

fn stable(tag: &str) -> Option<Version> {
    let version = Version::parse(tag.strip_prefix('v')?).ok()?;
    (version.pre.is_empty() && version.build.is_empty()).then_some(version)
}

fn newer(tag: &str, installed: &str) -> bool {
    stable(tag)
        .zip(Version::parse(installed).ok())
        .is_some_and(|(release, local)| release.cmp_precedence(&local).is_gt())
}

fn release_tag(data: &Value) -> Option<&str> {
    let tag = data["tag_name"].as_str()?;
    (data["draft"] == false && data["prerelease"] == false && stable(tag).is_some()).then_some(tag)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn agent(https_only: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(https_only)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(8)))
        .build()
        .into()
}

enum Response {
    Unchanged,
    Latest { data: Value, etag: Option<String> },
}

fn fetch(agent: &ureq::Agent, url: &str, etag: Option<&str>) -> Result<Response, String> {
    let mut request = agent
        .get(url)
        .header("User-Agent", format!("SourceSoundscapeStudio/{VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    let mut response = request
        .call()
        .map_err(|error| format!("Could not reach GitHub: {error}"))?;
    match response.status().as_u16() {
        304 => Ok(Response::Unchanged),
        404 => Ok(Response::Latest {
            data: Value::Null,
            etag: None,
        }),
        200 => {
            let etag = response
                .headers()
                .get("etag")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = response
                .body_mut()
                .with_config()
                .limit(MAX_JSON)
                .read_to_string()
                .map_err(|error| format!("Could not read GitHub's response: {error}"))?;
            let data = serde_json::from_str(&body)
                .map_err(|error| format!("Invalid GitHub response: {error}"))?;
            if release_tag(&data).is_none() {
                return Err("GitHub did not return a stable release tag.".into());
            }
            Ok(Response::Latest { data, etag })
        }
        status => Err(format!("GitHub returned HTTP {status}. Try again later.")),
    }
}

pub(super) struct Updates {
    startup: bool,
    last_check: u64,
    ignored: Option<String>,
    etag: Option<String>,
    latest: Value,
    pending: Option<(Receiver<Result<Response, String>>, bool)>,
    notified: Option<String>,
    initialized: bool,
    show_release: bool,
    show_settings: bool,
    status: String,
}

impl Default for Updates {
    fn default() -> Self {
        let prefs: Value = settings_path("updates.json")
            .and_then(|path| fs::File::open(path).ok())
            .and_then(|file| {
                use io::Read;
                serde_json::from_reader(file.take(MAX_JSON * 2)).ok()
            })
            .unwrap_or_default();
        let same_repo = prefs["repository"].as_str() == Some(REPOSITORY);
        Self {
            startup: prefs["startup"].as_bool().unwrap_or(true),
            last_check: if same_repo {
                prefs["last_check"].as_u64().unwrap_or(0)
            } else {
                0
            },
            ignored: prefs["ignored"]
                .as_str()
                .filter(|tag| same_repo && stable(tag).is_some())
                .map(str::to_owned),
            etag: prefs["etag"]
                .as_str()
                .filter(|etag| {
                    same_repo && etag.len() <= 256 && etag.bytes().all(|b| b.is_ascii_graphic())
                })
                .map(str::to_owned),
            latest: if same_repo {
                prefs["latest"].clone()
            } else {
                Value::Null
            },
            pending: None,
            notified: None,
            initialized: false,
            show_release: false,
            show_settings: false,
            status: String::new(),
        }
    }
}

impl Updates {
    fn persist(&mut self) {
        let result = (|| -> io::Result<()> {
            let path = settings_path("updates.json")
                .ok_or_else(|| io::Error::other("No settings directory"))?;
            fs::create_dir_all(path.parent().unwrap())?;
            let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
            let bytes = serde_json::to_vec(&json!({
                "repository": REPOSITORY,
                "startup": self.startup,
                "last_check": self.last_check,
                "ignored": self.ignored,
                "etag": self.etag,
                "latest": self.latest,
            }))?;
            fs::write(&tmp, bytes)?;
            fs::rename(tmp, path)
        })();
        if let Err(error) = result {
            self.status = format!("Could not save update preferences: {error}");
        }
    }

    pub(super) fn open_settings(&mut self) {
        self.show_settings = true;
    }

    pub(super) fn check_now(&mut self, ctx: &egui::Context) {
        self.show_settings = true;
        self.check(ctx, true);
    }

    fn check(&mut self, ctx: &egui::Context, manual: bool) {
        let Some(repo) = repository(REPOSITORY) else {
            self.status = "The official GitHub repository has not been configured.".into();
            return;
        };
        if let Some((_, was_manual)) = &mut self.pending {
            *was_manual |= manual;
            return;
        }
        let location = format!("https://api.github.com/repos/{repo}/releases/latest");
        let etag = self
            .etag
            .clone()
            .filter(|_| release_tag(&self.latest).is_some());
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("release-check".into())
            .spawn(move || {
                let _ = tx.send(fetch(&agent(true), &location, etag.as_deref()));
                ctx.request_repaint();
            });
        if let Err(error) = spawned {
            self.status = format!("Could not start update check: {error}");
            return;
        }
        self.last_check = now();
        self.status = "Checking GitHub Releases…".into();
        self.pending = Some((rx, manual));
        self.persist();
    }

    fn consider(&mut self, manual: bool) {
        if let Some(tag) = release_tag(&self.latest).filter(|tag| newer(tag, VERSION)) {
            if self.ignored.as_deref() == Some(tag) {
                self.status = format!("{tag} is skipped.");
            } else {
                self.status = format!("{tag} is available.");
                if manual || self.notified.as_deref() != Some(tag) {
                    self.notified = Some(tag.to_owned());
                    self.show_release = true;
                }
            }
        } else {
            self.status = if self.latest.is_null() {
                "No public stable release is available.".into()
            } else {
                "You have the latest version.".into()
            };
        }
    }

    pub(super) fn show(&mut self, ctx: &egui::Context) {
        if !self.initialized {
            self.initialized = true;
            if self.startup && repository(REPOSITORY).is_some() {
                if self.last_check > now()
                    || now().saturating_sub(self.last_check) >= CHECK_INTERVAL
                {
                    self.check(ctx, false);
                } else {
                    self.consider(false);
                }
            }
        }
        if let Some((rx, manual)) = &self.pending {
            let result = match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some(Err("The update check stopped.".into())),
                Err(TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                let manual = *manual;
                self.pending = None;
                match result {
                    Ok(Response::Latest { data, etag }) => {
                        self.latest = data;
                        self.etag = etag;
                        self.consider(manual);
                        self.persist();
                    }
                    Ok(Response::Unchanged) => self.consider(manual),
                    Err(error) => self.status = error,
                }
            }
        }
        if self.show_settings {
            let mut open = true;
            egui::Window::new("Settings")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.heading("Updates");
                    if ui
                        .checkbox(&mut self.startup, "Check for updates on startup")
                        .changed()
                    {
                        self.persist();
                    }
                    ui.label("Update channel · Stable");
                    let elapsed = now().saturating_sub(self.last_check);
                    let last = match (self.last_check, elapsed) {
                        (0, _) => "Never".into(),
                        (_, 0..=59) => "Just now".into(),
                        (_, 60..=3599) => format!("{} min ago", elapsed / 60),
                        (_, 3600..=86399) => format!("{} h ago", elapsed / 3600),
                        _ => format!("{} days ago", elapsed / CHECK_INTERVAL),
                    };
                    ui.label(format!("Last update check · {last}"));
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "Ignored version · {}",
                            self.ignored.as_deref().unwrap_or("None")
                        ));
                        if self.ignored.is_some() && ui.small_button("Clear").clicked() {
                            self.ignored = None;
                            self.persist();
                        }
                    });
                    if ui
                        .add_enabled(self.pending.is_none(), egui::Button::new("Check now"))
                        .clicked()
                    {
                        self.check(ctx, true);
                    }
                    if repository(REPOSITORY).is_none() && self.status.is_empty() {
                        ui.weak("The official GitHub repository has not been configured.");
                    }
                    ui.weak(&self.status);
                });
            self.show_settings = open;
        }
        if self.show_release
            && let Some(tag) = release_tag(&self.latest)
        {
            let mut open = true;
            let mut skip = false;
            let mut later = false;
            let release = format!("{REPOSITORY}/releases/tag/{tag}");
            egui::Window::new("A new version is available")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(format!("Installed {VERSION} · New {tag}"));
                    if let Some(date) = self.latest["published_at"].as_str() {
                        ui.label(format!(
                            "Published {}",
                            date.split('T').next().unwrap_or(date)
                        ));
                    }
                    if let Some(notes) = self.latest["body"].as_str() {
                        egui::ScrollArea::vertical()
                            .max_height(200.)
                            .show(ui, |ui| {
                                ui.label(notes);
                            });
                    }
                    ui.horizontal_wrapped(|ui| {
                        let archive = format!("SourceSoundscapeStudio-{tag}-windows-x64.zip");
                        if cfg!(windows)
                            && let Some(assets) = self.latest["assets"].as_array()
                        {
                            let name = [archive.as_str(), EXECUTABLE].into_iter().find(|name| {
                                assets
                                    .iter()
                                    .any(|asset| asset["name"].as_str() == Some(*name))
                            });
                            if let Some(name) = name {
                                ui.hyperlink_to(
                                    "Download update",
                                    format!("{REPOSITORY}/releases/download/{tag}/{name}"),
                                );
                            }
                        }
                        ui.hyperlink_to("Open GitHub release", release);
                        later = ui.button("Remind me later").clicked();
                        skip = ui.button("Skip this version").clicked();
                    });
                });
            self.show_release = open && !skip && !later;
            if skip {
                self.ignored = Some(tag.to_owned());
                self.persist();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };

    #[test]
    fn compares_release_tags_by_semver_and_limits_links_to_the_official_repository() {
        assert!(newer("v0.10.0", "0.9.5"));
        assert!(newer("v1.0.0", "1.0.0-rc.1"));
        for tag in [
            "v0.9.5",
            "v0.9.4",
            "v1.0.0-beta",
            "v01.0.0",
            "1.0.0",
            "v1.0.0+build",
        ] {
            assert!(!newer(tag, "0.9.5"), "{tag}");
        }
        assert_eq!(
            repository("https://github.com/owner/repo"),
            Some("owner/repo")
        );
        for url in [
            "https://github.com.example/owner/repo",
            "https://github.com/owner/repo/other",
            "https://github.com/owner/..",
            "https://github.com/owner/repo?redirect",
        ] {
            assert_eq!(repository(url), None);
        }
        assert!(
            release_tag(&json!({"tag_name":"v1.0.0","draft":true,"prerelease":false})).is_none()
        );
    }

    #[test]
    fn sends_conditional_request_and_distinguishes_http_and_network_errors() {
        let release = r#"{"tag_name":"v1.2.3","draft":false,"prerelease":false,"body":"Notes"}"#;
        let success = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"next\"\r\nConnection: close\r\n\r\n{release}",
            release.len()
        );
        for (reply, expected) in [
            (success.as_str(), None),
            (
                "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n",
                None,
            ),
            (
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                Some("HTTP 503"),
            ),
            ("", Some("Could not reach GitHub")),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/releases/latest", listener.local_addr().unwrap());
            thread::scope(|scope| {
                let server = scope.spawn(|| {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut conditional = false;
                    for line in BufReader::new(&mut stream).lines().map_while(Result::ok) {
                        conditional |= line.eq_ignore_ascii_case("if-none-match: \"version\"");
                        if line.is_empty() {
                            break;
                        }
                    }
                    stream.write_all(reply.as_bytes()).unwrap();
                    assert!(conditional);
                });
                let result = fetch(&agent(false), &url, Some("\"version\""));
                server.join().unwrap();
                match (result, expected) {
                    (Ok(Response::Latest { data, etag }), None) => {
                        assert_eq!(release_tag(&data), Some("v1.2.3"));
                        assert_eq!(etag.as_deref(), Some("\"next\""));
                    }
                    (Ok(Response::Unchanged), None) => {}
                    (Err(error), Some(expected)) => assert!(error.contains(expected)),
                    _ => panic!("Unexpected GitHub response"),
                }
            });
        }
    }
}
