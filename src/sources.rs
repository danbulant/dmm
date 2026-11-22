use std::{fmt, fs::File, io::Read, str::FromStr, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::Client;
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
            .build()
            .context("building download HTTP client")?;
        Ok(Self {
            http,
            downloads,
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
            "search-v3",
            &(source, game, query),
            Duration::from_secs(300),
            true,
            || match source {
                Source::GameBanana => gamebanana::search(&self.http, &self.cache, game, query),
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
            "details-v4",
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
        let response = self
            .downloads
            .get(url.clone())
            .send()
            .map_err(|_| anyhow!("requesting download failed"))?;
        if !response.status().is_success() {
            bail!("download server returned HTTP {}", response.status());
        }
        const MAX_DOWNLOAD_BYTES: u64 = 16 * 1024 * 1024 * 1024;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_DOWNLOAD_BYTES)
        {
            bail!("download exceeds the 16 GiB safety limit");
        }
        let copied = std::io::copy(&mut response.take(MAX_DOWNLOAD_BYTES + 1), dest)
            .map_err(|_| anyhow!("streaming download failed"))?;
        if copied > MAX_DOWNLOAD_BYTES {
            bail!("download exceeds the 16 GiB safety limit");
        }
        Ok(())
    }
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
    use super::{Category, ProviderFilters, TimeWindow, epoch_iso_timestamp};
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
    fn published_time_window_decodes_and_defaults() {
        let filters: ProviderFilters =
            serde_json::from_str(r#"{"published_within":"month"}"#).unwrap();
        assert_eq!(filters.published_within, Some(TimeWindow::Month));
    }
}
