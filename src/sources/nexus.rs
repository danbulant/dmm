use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::{Value, json};
use url::Url;

use super::{
    Category, DownloadAction, Maturity, ModDetails, ModFile, ModImage, RemoteMod, Requirement,
    SearchPage, SearchQuery, SearchResult, Sort, Source, TimeWindow, epoch_iso_timestamp, numeric,
    string,
};

const API: &str = "https://api.nexusmods.com/v1/";
const GRAPHQL: &str = "https://api.nexusmods.com/v2/graphql";
const WEB: &str = "https://www.nexusmods.com/";

fn api_url(path: &str) -> Result<Url> {
    Url::parse(API)?
        .join(path)
        .context("building Nexus API URL")
}

fn application_headers(request: RequestBuilder) -> RequestBuilder {
    request
        .header("Application-Name", "DMM")
        .header("Application-Version", env!("CARGO_PKG_VERSION"))
}

fn graphql(client: &Client, query: &str, variables: Value) -> Result<Value> {
    graphql_request(client.post(GRAPHQL), query, variables)
}

fn graphql_request(request: RequestBuilder, query: &str, variables: Value) -> Result<Value> {
    let response = application_headers(request)
        .json(&json!({"query": query, "variables": variables}))
        .send()
        .map_err(|_| anyhow!("requesting Nexus GraphQL API failed"))?;
    if !response.status().is_success() {
        bail!("Nexus GraphQL API returned HTTP {}", response.status());
    }
    let response: Value = response
        .json()
        .map_err(|_| anyhow!("decoding Nexus GraphQL response failed"))?;
    if response
        .get("errors")
        .is_some_and(|errors| !errors.as_array().is_some_and(Vec::is_empty))
    {
        bail!("Nexus GraphQL request failed");
    }
    response
        .get("data")
        .cloned()
        .ok_or_else(|| anyhow!("Nexus GraphQL response has no data"))
}

fn field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|key| value.get(*key))
}

fn text(value: &Value, keys: &[&str]) -> Option<String> {
    field(value, keys).and_then(string)
}

fn number(value: &Value, keys: &[&str]) -> Option<u64> {
    field(value, keys).and_then(numeric)
}

fn get_game_id(client: &Client, domain: &str) -> Result<i64> {
    let domain = path_segment(domain)?;
    let response = graphql(
        client,
        "query($domain: String!) { game(domainName: $domain) { id } }",
        json!({"domain": domain}),
    )?;
    let id = response
        .get("game")
        .and_then(|game| game.get("id"))
        .and_then(numeric)
        .ok_or_else(|| anyhow!("Nexus game domain was not found"))?;
    i64::try_from(id).context("Nexus game ID exceeds the supported range")
}
fn created_at_filter(window: TimeWindow, now: u64) -> Value {
    json!([{
        "value": window.cutoff_epoch_seconds(now).to_string(),
        "op": "GTE"
    }])
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sort_order(sort: Sort) -> Value {
    match sort {
        Sort::Relevance => json!([{"relevance": {"direction": "DESC"}}]),
        Sort::Popularity | Sort::Likes => json!([{"endorsements": {"direction": "DESC"}}]),
        Sort::Newest => json!([{"createdAt": {"direction": "DESC"}}]),
        Sort::Updated => json!([{"updatedAt": {"direction": "DESC"}}]),
    }
}

pub(super) fn search(client: &Client, game: &str, query: &SearchQuery) -> Result<SearchPage> {
    let mut filter = json!({
        "gameDomainName": [{"value": game, "op": "EQUALS"}]
    });
    if !query.text.is_empty() {
        filter["name"] = json!([{"value": query.text, "op": "WILDCARD"}]);
    }
    if let Some(category) = &query.category {
        filter["categoryName"] = json!([{"value": category, "op": "EQUALS"}]);
    }
    if let Some(window) = query.filters.published_within {
        filter["createdAt"] = created_at_filter(window, now_epoch_seconds());
    }
    for (name, value) in [
        ("author", query.filters.author.as_deref()),
        ("languageName", query.filters.language.as_deref()),
        ("tag", query.filters.tag.as_deref()),
    ] {
        if let Some(value) = value {
            filter[name] = json!([{"value": value, "op": "EQUALS"}]);
        }
    }
    let maturity = match query.maturity {
        Maturity::Safe => Some(false),
        Maturity::Adult => Some(true),
        Maturity::All => None,
    };
    if let Some(value) = maturity {
        filter["adultContent"] = json!([{"value": value, "op": "EQUALS"}]);
    }
    let sort = sort_order(query.sort);
    let offset = i64::from(query.page.max(1) - 1) * 20;
    let response = graphql(
        client,
        "query($filter: ModsFilter, $sort: [ModsSort!], $offset: Int, $count: Int) { mods(filter: $filter, sort: $sort, offset: $offset, count: $count) { nodes { modId name author category summary version createdAt updatedAt pictureUrl thumbnailUrl downloads endorsements adultContent } totalCount } }",
        json!({"filter": filter, "sort": sort, "offset": offset, "count": 20}),
    )?;
    let page = response
        .get("mods")
        .ok_or_else(|| anyhow!("Nexus GraphQL response has no mods page"))?;
    let nodes = page
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Nexus GraphQL response has invalid mod results"))?;
    let items = nodes
        .iter()
        .filter_map(|item| {
            let id = number(item, &["modId"])?;
            let adult = item.get("adultContent").and_then(Value::as_bool);
            if maturity.is_some() && adult != maturity {
                return None;
            }
            Some(SearchResult {
                id: RemoteMod {
                    source: Source::Nexus,
                    game: game.to_owned(),
                    id,
                },
                name: text(item, &["name"]).unwrap_or_else(|| format!("Mod {id}")),
                author: text(item, &["author"]),
                category: text(item, &["category"]),
                summary: text(item, &["summary"]),
                image: text(item, &["thumbnailUrl"]).or_else(|| text(item, &["pictureUrl"])),
                version: text(item, &["version"]),
                published_at: text(item, &["createdAt"]),
                updated_at: text(item, &["updatedAt"]),
                url: Some(web_mod_url(game, id)),
                downloads: number(item, &["downloads"]),
                endorsements: number(item, &["endorsements"]),
                likes: None,
                adult,
            })
        })
        .collect::<Vec<_>>();
    let total = number(page, &["totalCount"]);
    let has_more = total
        .map(|n| offset as u64 + (nodes.len() as u64) < n)
        .unwrap_or(nodes.len() == 20);
    Ok(SearchPage {
        items,
        page: query.page.max(1),
        total,
        total_pages: total.map(|n| n.div_ceil(20)),
        has_more,
    })
}

pub(super) fn details(client: &Client, key: Option<&str>, id: &RemoteMod) -> Result<ModDetails> {
    let game_id = get_game_id(client, &id.game)?;
    let response = graphql(
        client,
        "query($modId: ID!, $gameId: ID!) { mod(modId: $modId, gameId: $gameId) { modId name author category summary description version updatedAt createdAt pictureUrl thumbnailUrl thumbnailLargeUrl adultContent tags { name } modRequirements(skipDisabledRequirements: true) { nexusRequirements { nodes { modName notes url gameId modId externalRequirement } } } } modFiles(modId: $modId, gameId: $gameId) { fileId name version date sizeInBytes totalDownloads primary category description detectedFileExtension } }",
        json!({"modId": id.id.to_string(), "gameId": game_id.to_string()}),
    )?;
    let detail = response
        .get("mod")
        .ok_or_else(|| anyhow!("Nexus GraphQL response has no mod"))?;
    let tags = detail_tags(detail);
    let mut files = response
        .get("modFiles")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Nexus GraphQL response has invalid mod files"))?
        .iter()
        .filter_map(parse_file)
        .collect::<Vec<_>>();
    if let Some(key) = key {
        if let Some(v1_files) = v1_files(client, key, id)? {
            files = v1_files;
        }
    }
    let images = text(detail, &["pictureUrl"])
        .or_else(|| text(detail, &["thumbnailLargeUrl"]))
        .or_else(|| text(detail, &["thumbnailUrl"]))
        .into_iter()
        .map(|url| ModImage {
            url,
            thumbnail: text(detail, &["thumbnailUrl"]),
        })
        .collect();
    let requirements = mod_requirements(client, detail, &id.game, game_id as u64);
    let updated_at = text(detail, &["updatedAt"]);
    let version =
        text(detail, &["version"]).or_else(|| files.iter().find_map(|file| file.version.clone()));
    Ok(ModDetails {
        id: id.clone(),
        name: text(detail, &["name"]).unwrap_or_else(|| format!("Mod {}", id.id)),
        author: text(detail, &["author"]),
        tags,
        category: text(detail, &["category"]),
        summary: text(detail, &["summary"]),
        description: text(detail, &["description"]),
        version,
        updated_at,
        created_at: text(detail, &["createdAt"]),
        url: Some(web_mod_url(&id.game, id.id)),
        images,
        adult: detail.get("adultContent").and_then(Value::as_bool),
        requirements,
        files,
    })
}

fn detail_tags(detail: &Value) -> Vec<String> {
    detail
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tag| text(tag, &["name"]))
        .map(|tag| tag.trim().to_owned())
        .filter(|tag| !tag.is_empty())
        .collect()
}

fn mod_requirements(
    client: &Client,
    detail: &Value,
    domain: &str,
    game_id: u64,
) -> Vec<Requirement> {
    let mut query = String::from("{");
    let mut requested = Vec::new();
    let mut requirements = Vec::new();
    let nodes = detail
        .pointer("/modRequirements/nexusRequirements/nodes")
        .and_then(Value::as_array);
    for item in nodes.into_iter().flatten() {
        let Some(name) = text(item, &["modName"]) else {
            continue;
        };
        let mut requirement = Requirement {
            name,
            url: text(item, &["url"]).filter(|url| !url.trim().is_empty()),
            notes: text(item, &["notes"]).filter(|notes| !notes.trim().is_empty()),
            thumbnail: None,
        };
        if item.get("externalRequirement").and_then(Value::as_bool) != Some(true)
            && let (Some(game), Some(id)) = (number(item, &["gameId"]), number(item, &["modId"]))
            && game != 0
            && id != 0
        {
            if requirement.url.is_none() && game == game_id {
                requirement.url = Some(web_mod_url(domain, id));
            }
            if requested.len() < 12 {
                let alias = format!("r{}", requested.len());
                let _ = write!(
                    query,
                    " {alias}: mod(modId:\"{id}\", gameId:\"{game}\") {{ thumbnailUrl adultContent game {{ domainName }} }}"
                );
                requested.push((requirements.len(), alias, id));
            }
        }
        requirements.push(requirement);
    }
    if !requested.is_empty() {
        query.push('}');
        let request = client.post(GRAPHQL).timeout(Duration::from_secs(5));
        if let Ok(data) = graphql_request(request, &query, json!({})) {
            for (index, alias, id) in requested {
                let item = &data[alias];
                let requirement = &mut requirements[index];
                if item.get("adultContent").and_then(Value::as_bool) == Some(false) {
                    requirement.thumbnail = text(item, &["thumbnailUrl"]);
                }
                if requirement.url.is_none()
                    && let Some(game) = item.pointer("/game/domainName").and_then(Value::as_str)
                    && let Ok(game) = path_segment(game)
                {
                    requirement.url = Some(web_mod_url(game, id));
                }
            }
        }
    }
    requirements
}

fn parse_file(value: &Value) -> Option<ModFile> {
    let id = number(value, &["fileId", "file_id", "id"])?;
    let name =
        text(value, &["file_name", "filename", "name"]).unwrap_or_else(|| format!("File {id}"));
    let extension = text(value, &["detectedFileExtension"]);
    let name = if value.get("file_name").is_none() && value.get("filename").is_none() {
        extension
            .filter(|ext| !ext.is_empty())
            .map(|ext| format!("{name}.{ext}"))
            .unwrap_or(name)
    } else {
        name
    };
    Some(ModFile {
        id,
        name,
        version: text(value, &["version"]),
        updated_at: number(value, &["date", "uploaded_timestamp", "updated_timestamp"])
            .and_then(epoch_iso_timestamp)
            .or_else(|| text(value, &["uploaded_time", "updated_time"])),
        size: number(value, &["sizeInBytes", "size_in_bytes", "size"]),
        downloads: number(
            value,
            &[
                "totalDownloads",
                "mod_downloads",
                "downloads",
                "download_count",
                "downloadCount",
            ],
        ),
        primary: field(value, &["primary", "is_primary", "isPrimary"])
            .and_then(Value::as_bool)
            .or_else(|| {
                field(value, &["primary", "is_primary", "isPrimary"])
                    .and_then(numeric)
                    .map(|n| n != 0)
            })
            .unwrap_or(false),
        category: text(value, &["category_name", "category"]),
        description: text(value, &["description"]),
        url: None,
    })
}
fn v1_files(client: &Client, key: &str, id: &RemoteMod) -> Result<Option<Vec<ModFile>>> {
    let path = format!(
        "games/{}/mods/{}/files.json",
        path_segment(&id.game)?,
        id.id
    );
    let response = application_headers(client.get(api_url(&path)?))
        .header("apikey", key)
        .send()
        .map_err(|_| anyhow!("requesting Nexus file metadata failed"))?;
    if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
        return Ok(None);
    }
    if !response.status().is_success() {
        bail!("Nexus file API returned HTTP {}", response.status());
    }
    let value: Value = response
        .json()
        .map_err(|_| anyhow!("decoding Nexus file metadata failed"))?;
    let files = value
        .as_array()
        .or_else(|| value.get("files").and_then(Value::as_array))
        .ok_or_else(|| anyhow!("Nexus file response has no files"))?;
    Ok(Some(files.iter().filter_map(parse_file).collect()))
}

pub(super) fn download(
    client: &Client,
    key: Option<&str>,
    id: &RemoteMod,
    file: &ModFile,
) -> Result<DownloadAction> {
    let browser = browser_file_url(&id.game, id.id, file.id)?;
    let Some(key) = key else {
        return Ok(DownloadAction::OpenBrowser(browser));
    };
    let path = format!(
        "games/{}/mods/{}/files/{}/download_link.json",
        path_segment(&id.game)?,
        id.id,
        file.id
    );
    let url = api_url(&path)?;
    let response = application_headers(client.get(url))
        .header("apikey", key)
        .send()
        .map_err(|_| anyhow!("requesting Nexus download link failed"))?;
    if response.status().as_u16() == 401 || response.status().as_u16() == 403 {
        return Ok(DownloadAction::OpenBrowser(browser));
    }
    if !response.status().is_success() {
        bail!("Nexus download API returned HTTP {}", response.status());
    }
    let value: Value = response
        .json()
        .map_err(|_| anyhow!("decoding Nexus download response failed"))?;
    let first = value
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or(&value);
    let link = text(first, &["URI", "uri", "url"])
        .ok_or_else(|| anyhow!("Nexus did not return a download link"))?;
    let url = Url::parse(&link).context("invalid Nexus download URL")?;
    if url.scheme() != "https" {
        bail!("Nexus returned a non-HTTPS download URL");
    }
    Ok(DownloadAction::Direct(url))
}

pub(super) fn redeem(
    client: &Client,
    key: Option<&str>,
    raw_uri: &str,
) -> Result<(RemoteMod, u64, Url)> {
    let parsed = parse_nxm(raw_uri)?;
    let mut url = api_url(&format!(
        "games/{}/mods/{}/files/{}/download_link.json",
        path_segment(&parsed.game)?,
        parsed.mod_id,
        parsed.file_id
    ))?;
    {
        let mut params = url.query_pairs_mut();
        for (name, value) in [
            ("key", parsed.key.as_deref()),
            ("expires", parsed.expires.as_deref()),
            ("user_id", parsed.user_id.as_deref()),
        ] {
            if let Some(value) = value {
                params.append_pair(name, value);
            }
        }
    }
    let mut request = application_headers(client.get(url));
    if let Some(key) = key {
        request = request.header("apikey", key);
    }
    let response = request
        .send()
        .map_err(|_| anyhow!("exchanging Nexus nxm link failed"))?;
    if !response.status().is_success() {
        bail!("Nexus nxm exchange returned HTTP {}", response.status());
    }
    let value: Value = response
        .json()
        .map_err(|_| anyhow!("decoding Nexus nxm exchange response failed"))?;
    let first = value
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or(&value);
    let link = text(first, &["URI", "uri", "url"])
        .ok_or_else(|| anyhow!("Nexus did not return a download link"))?;
    let download = Url::parse(&link).context("invalid Nexus download URL")?;
    if download.scheme() != "https" {
        bail!("Nexus returned a non-HTTPS download URL");
    }
    Ok((
        RemoteMod {
            source: Source::Nexus,
            game: parsed.game,
            id: parsed.mod_id,
        },
        parsed.file_id,
        download,
    ))
}

struct Nxm {
    game: String,
    mod_id: u64,
    file_id: u64,
    key: Option<String>,
    expires: Option<String>,
    user_id: Option<String>,
}

fn parse_nxm(uri: &str) -> Result<Nxm> {
    let url = Url::parse(uri).context("invalid nxm URL")?;
    if url.scheme() != "nxm" {
        bail!("expected an nxm:// URL");
    }
    let game = url
        .host_str()
        .filter(|game| !game.is_empty())
        .ok_or_else(|| anyhow!("nxm URL is missing a game"))?
        .to_owned();
    let _ = path_segment(&game)?;
    let parts = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    if parts.len() != 4 || parts[0] != "mods" || parts[2] != "files" {
        bail!("nxm URL must identify a mod and file");
    }
    let mod_id = parts[1].parse().context("invalid mod ID in nxm URL")?;
    let file_id = parts[3].parse().context("invalid file ID in nxm URL")?;
    let query = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    Ok(Nxm {
        game,
        mod_id,
        file_id,
        key: query.get("key").map(ToString::to_string),
        expires: query.get("expires").map(ToString::to_string),
        user_id: query.get("user_id").map(ToString::to_string),
    })
}
pub(super) fn identity(uri: &str) -> Result<(RemoteMod, u64)> {
    let parsed = parse_nxm(uri)?;
    Ok((
        RemoteMod {
            source: Source::Nexus,
            game: parsed.game,
            id: parsed.mod_id,
        },
        parsed.file_id,
    ))
}

pub(super) fn categories(client: &Client, _key: Option<&str>, game: &str) -> Result<Vec<Category>> {
    let game_id = get_game_id(client, game)?;
    let response = graphql(
        client,
        "query($gameId: Int!) { categories(gameId: $gameId, global: true) { id name } }",
        json!({"gameId": game_id}),
    )?;
    let categories = response
        .get("categories")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Nexus GraphQL response has invalid categories"))?;
    Ok(categories
        .iter()
        .filter_map(|item| {
            let id = number(item, &["id"])?.to_string();
            Some(Category {
                id,
                name: text(item, &["name"])?,
                parent: None,
                icon: None,
            })
        })
        .collect())
}

fn path_segment(value: &str) -> Result<&str> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!("invalid Nexus game domain");
    }
    Ok(value)
}

fn web_mod_url(game: &str, id: u64) -> String {
    format!("{}{}/mods/{id}", WEB, path_segment(game).unwrap_or(""))
}

fn browser_file_url(game: &str, id: u64, file: u64) -> Result<Url> {
    let mut url = Url::parse(&format!("{}{}/mods/{id}", WEB, path_segment(game)?))?;
    url.query_pairs_mut()
        .append_pair("tab", "files")
        .append_pair("file_id", &file.to_string());
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::{created_at_filter, detail_tags, parse_file, parse_nxm, sort_order};
    use crate::sources::{Sort, TimeWindow};
    use serde_json::json;

    #[test]
    fn likes_sort_uses_nexus_endorsements_graphql_wire_order() {
        assert_eq!(
            sort_order(Sort::Likes),
            json!([{"endorsements": {"direction": "DESC"}}])
        );
    }

    #[test]
    fn nexus_detail_tags_use_tag_names_and_missing_tags_are_empty() {
        assert_eq!(
            detail_tags(&json!({"tags": [{"name": "Gameplay"}, {"name": "UI"}, {"name": ""}]})),
            ["Gameplay", "UI"]
        );
        assert!(detail_tags(&json!({})).is_empty());
    }

    #[test]
    fn published_window_uses_created_at_gte_cutoff() {
        let cutoff = 1_800_000_000 - 604_800;
        assert_eq!(
            created_at_filter(TimeWindow::Week, 1_800_000_000),
            json!([{"value": cutoff.to_string(), "op": "GTE"}])
        );
    }

    #[test]
    fn parses_nxm_link_and_preserves_escaped_credentials() {
        let parsed =
            parse_nxm("nxm://skyrim/mods/12/files/34?key=a%2Bb&expires=1700000000&user_id=8")
                .unwrap();
        assert_eq!(parsed.game, "skyrim");
        assert_eq!(parsed.mod_id, 12);
        assert_eq!(parsed.file_id, 34);
        assert_eq!(parsed.key.as_deref(), Some("a+b"));
        assert_eq!(parsed.expires.as_deref(), Some("1700000000"));
        assert_eq!(parsed.user_id.as_deref(), Some("8"));
    }

    #[test]
    fn rejects_other_schemes_and_malformed_paths() {
        assert!(parse_nxm("https://example.com/mods/12/files/34").is_err());
        assert!(parse_nxm("nxm://skyrim/mods/12").is_err());
        assert!(parse_nxm("nxm://skyrim/mods/not-a-number/files/34").is_err());
    }

    #[test]
    fn prefers_actual_file_name_over_display_name() {
        let file = parse_file(&json!({"fileId": 8, "file_name": "actual.zip", "name": "Display title", "sizeInBytes": "123", "downloads": "9", "primary": 1})).unwrap();
        assert_eq!(file.name, "actual.zip");
        assert_eq!(file.size, Some(123));
        assert_eq!(file.downloads, Some(9));
        assert!(file.primary);
    }
}
