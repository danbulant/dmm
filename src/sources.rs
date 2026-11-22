use std::{fmt, fs::File, future::Future, io::Write, pin::pin, str::FromStr, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Client as AsyncClient, blocking::Client};
use serde::{Deserialize, Serialize};
use url::Url;

mod cache;
mod gamebanana;
mod nexus;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    GameBanana,
    Nexus,
    Local,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::GameBanana => "gamebanana",
            Self::Nexus => "nexus",
            Self::Local => "local",
        })
    }
}

impl FromStr for Source {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "gamebanana" | "game_banana" | "game-banana" => Ok(Self::GameBanana),
            "nexus" | "nexusmods" | "nexus-mods" => Ok(Self::Nexus),
            "local" => Ok(Self::Local),
            _ => bail!("unknown source: {value}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct RemoteMod {
    pub source: Source,
    pub game: String,
    pub id: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Relevance,
    Popularity,
    Newest,
    Updated,
}

impl FromStr for Sort {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "relevance" | "best_match" => Ok(Self::Relevance),
            "popularity" => Ok(Self::Popularity),
            "newest" | "date" => Ok(Self::Newest),
            "updated" | "udate" => Ok(Self::Updated),
            _ => bail!("unknown sort order: {value}"),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    pub page: u32,
    pub category: Option<String>,
    pub sort: Sort,
    #[serde(default)]
    pub maturity: Maturity,
    #[serde(default)]
    pub filters: ProviderFilters,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Maturity {
    Safe,
    #[default]
    All,
    Adult,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeWindow {
    Week,
    Month,
    Year,
}

impl TimeWindow {
    pub const ALL: [Self; 3] = [Self::Week, Self::Month, Self::Year];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Week => "Last 7 days",
            Self::Month => "Last 30 days",
            Self::Year => "Last 365 days",
        }
    }

    pub(super) const fn seconds(self) -> u64 {
        match self {
            Self::Week => 7 * 24 * 60 * 60,
            Self::Month => 30 * 24 * 60 * 60,
            Self::Year => 365 * 24 * 60 * 60,
        }
    }

    pub(super) fn cutoff_epoch_seconds(self, now: u64) -> u64 {
        now.saturating_sub(self.seconds())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderFilters {
    pub author: Option<String>,
    pub language: Option<String>,
    pub tag: Option<String>,
    pub release_type: Option<String>,
    #[serde(default)]
    pub content_ratings: Vec<String>,
    #[serde(default)]
    pub published_within: Option<TimeWindow>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FilterOption {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FilterCatalog {
    pub categories: Vec<Category>,
    pub release_types: Vec<FilterOption>,
    pub content_ratings: Vec<FilterOption>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Category {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub id: RemoteMod,
    pub name: String,
    pub author: Option<String>,
    pub category: Option<String>,
    pub summary: Option<String>,
    pub image: Option<String>,
    pub version: Option<String>,
    pub updated_at: Option<String>,
    #[serde(default)]
    pub published_at: Option<String>,
    pub url: Option<String>,
    pub downloads: Option<u64>,
    #[serde(default)]
    pub endorsements: Option<u64>,
    #[serde(default)]
    pub likes: Option<u64>,
    #[serde(default)]
    pub adult: Option<bool>,
}
pub type ModSummary = SearchResult;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchPage {
    pub items: Vec<SearchResult>,
    pub page: u32,
    pub total: Option<u64>,
    pub total_pages: Option<u64>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModFile {
    pub id: u64,
    pub name: String,
    pub version: Option<String>,
    pub updated_at: Option<String>,
    pub size: Option<u64>,
    pub downloads: Option<u64>,
    pub primary: bool,
    pub category: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModImage {
    pub url: String,
    pub thumbnail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    pub name: String,
    pub url: Option<String>,
    pub notes: Option<String>,
    pub thumbnail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModDetails {
    pub id: RemoteMod,
    pub name: String,
    pub author: Option<String>,
    pub category: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub images: Vec<ModImage>,
    #[serde(default)]
    pub adult: Option<bool>,
    pub requirements: Vec<Requirement>,
    pub files: Vec<ModFile>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadAction {
    Direct(Url),
    OpenBrowser(Url),
}

pub struct SourceClient {
    http: Client,
    downloads: Client,
    download_stream: AsyncClient,
    download_runtime: tokio::runtime::Runtime,
    nexus_key: Option<String>,
    cache: cache::Cache,
}

impl SourceClient {
    pub fn new(nexus_key: Option<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("dmm/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building source HTTP client")?;
        let downloads = Client::builder()
            .user_agent(concat!("dmm/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .context("building download-link HTTP client")?;
        let download_stream = AsyncClient::builder()
            .user_agent(concat!("dmm/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .context("building streaming download HTTP client")?;
        let download_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building download runtime")?;
        Ok(Self {
            http,
            downloads,
            download_stream,
            download_runtime,
            nexus_key,
            cache: cache::Cache::default(),
        })
    }

    pub fn with_cache(mut self, path: &std::path::Path) -> Self {
        self.cache = cache::persistent(path);
        self
    }

    pub fn search(&self, source: Source, game: &str, query: &SearchQuery) -> Result<SearchPage> {
        self.cache.get(
            "search-v4",
            &(source, game, query),
            Duration::from_secs(300),
            true,
            || match source {
                Source::GameBanana => gamebanana::search(&self.http, game, query),
                Source::Nexus => {
                    if let Some(category) = query.category.as_deref()
                        && category.parse::<u64>().is_ok()
                    {
                        let mut query = query.clone();
                        query.category = self
                            .categories(source, game)?
                            .into_iter()
                            .find(|item| item.id == category)
                            .map(|item| item.name);
                        anyhow::ensure!(
                            query.category.is_some(),
                            "Unknown Nexus category ID {category}"
                        );
                        nexus::search(&self.http, game, &query)
                    } else {
                        nexus::search(&self.http, game, query)
                    }
                }
                Source::Local => bail!("local sources cannot be searched remotely"),
            },
        )
    }

    pub fn details(&self, id: &RemoteMod) -> Result<ModDetails> {
        let public = id.source != Source::Nexus || self.nexus_key.is_none();
        self.cache.get(
            "details-v5",
            id,
            Duration::from_secs(120),
            public,
            || match id.source {
                Source::GameBanana => gamebanana::details(&self.http, id),
                Source::Nexus => nexus::details(&self.http, self.nexus_key.as_deref(), id),
                Source::Local => bail!("local mods do not have remote details"),
            },
        )
    }

    pub fn filter_catalog(&self, source: Source, game: &str) -> Result<FilterCatalog> {
        self.cache.get(
            "filters-v2",
            &(source, game),
            Duration::from_secs(3600),
            true,
            || match source {
                Source::GameBanana => gamebanana::filter_catalog(&self.http, game),
                Source::Nexus => Ok(FilterCatalog {
                    categories: self.categories(source, game)?,
                    ..FilterCatalog::default()
                }),
                Source::Local => bail!("local sources do not have remote filters"),
            },
        )
    }

    pub fn categories(&self, source: Source, game: &str) -> Result<Vec<Category>> {
        self.cache.get(
            "categories-v2",
            &(source, game),
            Duration::from_secs(3600),
            true,
            || match source {
                Source::GameBanana => gamebanana::categories(&self.http, game),
                Source::Nexus => nexus::categories(&self.http, self.nexus_key.as_deref(), game),
                Source::Local => bail!("local sources do not have remote categories"),
            },
        )
    }

    pub fn download(&self, id: &RemoteMod, file: &ModFile) -> Result<DownloadAction> {
        match id.source {
            Source::GameBanana => gamebanana::download(&self.downloads, id, file),
            Source::Nexus => nexus::download(&self.http, self.nexus_key.as_deref(), id, file),
            Source::Local => bail!("local files are not downloaded from a remote source"),
        }
    }
    pub fn redeem(&self, uri: &str) -> Result<(RemoteMod, u64, Url)> {
        nexus::redeem(&self.http, self.nexus_key.as_deref(), uri)
    }

    pub fn fetch(&self, url: &Url, dest: &mut File) -> Result<()> {
        self.fetch_observed(url, dest, |_, _| Ok(()))
    }

    pub fn fetch_observed(
        &self,
        url: &Url,
        dest: &mut File,
        mut observe: impl FnMut(u64, Option<u64>) -> Result<()>,
    ) -> Result<()> {
        const MAX_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024 * 1024;
        self.download_runtime.block_on(async {
            let response = wait_for_io(self.download_stream.get(url.clone()).send(), || {
                observe(0, None)
            })
            .await?
            .map_err(|_| anyhow!("requesting download failed"))?;
            if !response.status().is_success() {
                bail!("download server returned HTTP {}", response.status());
            }
            let total = response.content_length();
            if total.is_some_and(|size| size > MAX_DOWNLOAD_BYTES) {
                bail!("download exceeds the 16 GiB safety limit");
            }
            let mut response = response;
            let mut copied = 0_u64;
            observe(copied, total)?;
            loop {
                let chunk = wait_for_io(response.chunk(), || observe(copied, total))
                    .await?
                    .map_err(|_| anyhow!("streaming download failed"))?;
                let Some(chunk) = chunk else { break };
                copied += chunk.len() as u64;
                if copied > MAX_DOWNLOAD_BYTES {
                    bail!("download exceeds the 16 GiB safety limit");
                }
                dest.write_all(&chunk)
                    .map_err(|_| anyhow!("writing downloaded archive failed"))?;
                observe(copied, total)?;
            }
            Ok(())
        })
    }
}

async fn wait_for_io<T>(
    future: impl Future<Output = T>,
    mut on_wait: impl FnMut() -> Result<()>,
) -> Result<T> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut future = pin!(future);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            bail!("download stalled");
        }
        match tokio::time::timeout(remaining.min(Duration::from_millis(100)), &mut future).await {
            Ok(result) => return Ok(result),
            Err(_) => on_wait()?,
        }
    }
}

pub fn nexus_identity(uri: &str) -> Result<(RemoteMod, u64)> {
    nexus::identity(uri)
}

pub(super) fn json(
    client: &Client,
    url: Url,
    nexus_key: Option<&str>,
) -> Result<serde_json::Value> {
    let mut request = client.get(url);
    if let Some(key) = nexus_key {
        request = request.header("apikey", key);
    }
    let response = request
        .send()
        .map_err(|_| anyhow!("requesting source API failed"))?;
    if !response.status().is_success() {
        bail!("source API returned HTTP {}", response.status());
    }
    response
        .json()
        .map_err(|_| anyhow!("decoding source API response failed"))
}

pub(super) fn obj<'a>(
    value: &'a serde_json::Value,
) -> Result<&'a serde_json::Map<String, serde_json::Value>> {
    value
        .as_object()
        .ok_or_else(|| anyhow!("unexpected source API response shape"))
}

pub(super) fn string(value: &serde_json::Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

pub(super) fn numeric(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
pub(super) fn epoch_iso_timestamp(seconds: u64) -> Option<String> {
    let seconds = i64::try_from(seconds).ok()?;
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3_600,
        (day_seconds % 3_600) / 60,
        day_seconds % 60,
    ))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Seek, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::Duration,
    };

    use super::{
        Category, ModDetails, ProviderFilters, SourceClient, TimeWindow, Url, epoch_iso_timestamp,
    };
    #[test]
    fn formats_unix_seconds_as_utc_iso_timestamps() {
        assert_eq!(
            epoch_iso_timestamp(0).as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert_eq!(
            epoch_iso_timestamp(1_700_000_000).as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
    }
    #[test]
    fn time_windows_use_fixed_rolling_durations() {
        let now = 1_800_000_000;
        assert_eq!(TimeWindow::Week.cutoff_epoch_seconds(now), now - 604_800);
        assert_eq!(TimeWindow::Month.cutoff_epoch_seconds(now), now - 2_592_000);
        assert_eq!(TimeWindow::Year.cutoff_epoch_seconds(now), now - 31_536_000);
        assert_eq!(TimeWindow::Week.cutoff_epoch_seconds(1), 0);
    }

    #[test]
    fn new_filter_and_category_fields_decode_with_defaults() {
        let filters: ProviderFilters = serde_json::from_str(
            r#"{"author":null,"language":null,"tag":null,"release_type":null}"#,
        )
        .unwrap();
        assert!(filters.content_ratings.is_empty());
        assert_eq!(filters.published_within, None);

        let category: Category =
            serde_json::from_str(r#"{"id":"12","name":"Character Skins"}"#).unwrap();
        assert_eq!(category.parent, None);
        assert_eq!(category.icon, None);
    }

    #[test]
    fn mod_details_decode_without_tags() {
        let details: ModDetails = serde_json::from_str(
            r#"{"id":{"source":"nexus","game":"skyrim","id":12},"name":"Example","images":[],"requirements":[],"files":[]}"#,
        )
        .unwrap();
        assert!(details.tags.is_empty());
    }
    #[test]
    fn published_time_window_decodes_and_defaults() {
        let filters: ProviderFilters =
            serde_json::from_str(r#"{"published_within":"month"}"#).unwrap();
        assert_eq!(filters.published_within, Some(TimeWindow::Month));
    }
    #[test]
    fn observed_fetch_reports_received_and_total_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789",
                )
                .unwrap();
        });
        let client = SourceClient::new(None).unwrap();
        let url = Url::parse(&format!("http://{address}/archive")).unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let mut observations = Vec::new();
        client
            .fetch_observed(&url, &mut output, |received, total| {
                observations.push((received, total));
                Ok(())
            })
            .unwrap();
        server.join().unwrap();
        assert!(observations.contains(&(0, Some(10))));
        assert_eq!(observations.last(), Some(&(10, Some(10))));
        let mut bytes = Vec::new();
        output.rewind().unwrap();
        output.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"0123456789");
    }

    #[test]
    fn observed_fetch_can_cancel_while_the_server_is_stalled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (done, wait) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n")
                .unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(3));
        });
        let client = SourceClient::new(None).unwrap();
        let url = Url::parse(&format!("http://{address}/archive")).unwrap();
        let mut output = tempfile::tempfile().unwrap();
        let mut received_headers = false;
        let error = client
            .fetch_observed(&url, &mut output, |_, total| {
                if total == Some(10) {
                    if received_headers {
                        return Err(anyhow::anyhow!("cancelled in test"));
                    }
                    received_headers = true;
                }
                Ok(())
            })
            .unwrap_err();
        let _ = done.send(());
        server.join().unwrap();
        assert_eq!(error.to_string(), "cancelled in test");
        assert_eq!(output.metadata().unwrap().len(), 0);
    }
}
