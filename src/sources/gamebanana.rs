use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use std::thread;
use std::time::Duration;

use super::{
    Category, DownloadAction, FilterCatalog, FilterOption, Maturity, ModDetails, ModFile, ModImage,
    RemoteMod, Requirement, SearchPage, SearchQuery, SearchResult, Sort, Source, cache::Cache,
    epoch_iso_timestamp, json, numeric, obj, string,
};

const BASE: &str = "https://gamebanana.com/apiv11/";
const WEB: &str = "https://gamebanana.com/mods/";

fn endpoint(path: &str) -> Result<Url> {
    Url::parse(BASE)?
        .join(path)
        .context("building GameBanana API URL")
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(string)
}

fn optional_num(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(numeric)
}

fn as_array<'a>(value: &'a Value, label: &str) -> Result<&'a [Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow!("unexpected GameBanana {label} response shape"))
}

fn id_for_game(game: &str) -> Result<u64> {
    game.parse::<u64>()
        .context("GameBanana game must be its numeric game ID")
}

const PAGE_SIZE: usize = 15;
const SOURCE_PAGES_PER_REQUEST: usize = 8;
const SEARCH_TTL: Duration = Duration::from_secs(300);

#[derive(Default, Deserialize, Serialize)]
struct Cursor {
    source_page: u32,
    tail: Vec<SearchResult>,
    prior_matches: u64,
}

#[derive(Deserialize, Serialize)]
struct PackedPage {
    result: SearchPage,
    next: Option<Cursor>,
}

pub(super) fn search(
    client: &Client,
    cache: &Cache,
    game: &str,
    query: &SearchQuery,
) -> Result<SearchPage> {
    let tags = &query.filters.content_ratings;
    if query.maturity == Maturity::All && tags.len() <= 1 {
        return search_single(client, game, query);
    }
    let mut base = query.clone();
    base.page = 1;
    base.maturity = Maturity::All;
    base.filters.content_ratings = if tags.len() <= 1 {
        tags.clone()
    } else {
        cache.get("gb-tag-anchor", &(game, &base), SEARCH_TTL, true, || {
            let mut candidates = base.clone();
            let mut best = None;
            for tag in tags {
                candidates.filters.content_ratings = vec![tag.clone()];
                let page = search_single(client, game, &candidates)?;
                let count = page.total.unwrap_or(u64::MAX);
                if best.as_ref().is_none_or(|(smallest, _)| count < *smallest) {
                    best = Some((count, tag.clone()));
                }
            }
            Ok(best.map(|(_, tag)| tag).into_iter().collect::<Vec<_>>())
        })?
    };
    let mut cursor = Cursor {
        source_page: 1,
        ..Cursor::default()
    };
    for number in 1..=query.page.max(1) {
        let packed = cache.get(
            "gb-packed-v2",
            &(
                game,
                query.text.as_str(),
                &query.category,
                query.sort,
                query.maturity,
                &query.filters,
                number,
            ),
            SEARCH_TTL,
            true,
            || {
                pack_page(number, cursor, |source_page| {
                    base.page = source_page;
                    let page = search_single(client, game, &base)?;
                    let matches = classify_candidates(client, page.items, query.maturity, tags)?;
                    Ok((matches, page.has_more.then_some(source_page + 1)))
                })
            },
        )?;
        if number == query.page.max(1) || packed.next.is_none() {
            return Ok(packed.result);
        }
        cursor = packed.next.expect("checked packed continuation");
    }
    unreachable!("a normalized page is always positive")
}

fn pack_page(
    number: u32,
    mut cursor: Cursor,
    mut fetch: impl FnMut(u32) -> Result<(Vec<SearchResult>, Option<u32>)>,
) -> Result<PackedPage> {
    for _ in 0..SOURCE_PAGES_PER_REQUEST {
        if cursor.tail.len() > PAGE_SIZE || cursor.source_page == 0 {
            break;
        }
        let (matches, next) = fetch(cursor.source_page)?;
        cursor.tail.extend(matches);
        cursor.source_page = next.unwrap_or(0);
    }
    let items = if cursor.tail.len() <= PAGE_SIZE {
        std::mem::take(&mut cursor.tail)
    } else {
        let tail = cursor.tail.split_off(PAGE_SIZE);
        std::mem::replace(&mut cursor.tail, tail)
    };
    cursor.prior_matches += items.len() as u64;
    let has_more = !cursor.tail.is_empty() || cursor.source_page != 0;
    Ok(PackedPage {
        result: SearchPage {
            items,
            page: number,
            total: (!has_more).then_some(cursor.prior_matches),
            total_pages: (!has_more).then_some(u64::from(number)),
            has_more,
        },
        next: has_more.then_some(cursor),
    })
}

fn classify_candidates(
    client: &Client,
    mut items: Vec<SearchResult>,
    maturity: Maturity,
    tags: &[String],
) -> Result<Vec<SearchResult>> {
    let wanted = match maturity {
        Maturity::All => None,
        Maturity::Safe => Some(false),
        Maturity::Adult => Some(true),
    };
    if let Some(wanted) = wanted {
        items.retain(|item| item.adult.is_none_or(|adult| adult == wanted));
    }
    let mut accepted = Vec::with_capacity(items.len());
    for chunk in items.chunks_mut(5) {
        thread::scope(|scope| {
            let jobs: Vec<_> = chunk
                .iter_mut()
                .map(|item| {
                    scope.spawn(move || {
                        // A single tag was applied by the server, so only fetch its
                        // profile if the requested visibility still needs classification.
                        let matches = if tags.len() > 1 {
                            let profile = fetch_profile_ratings(client, item.id.id)?;
                            item.adult = content_rating(&profile);
                            has_all_ratings(&profile, tags)
                        } else {
                            if item.adult.is_none() && wanted.is_some() {
                                item.adult =
                                    content_rating(&fetch_profile_ratings(client, item.id.id)?);
                            }
                            true
                        };
                        Ok::<_, anyhow::Error>(
                            matches && wanted.is_none_or(|wanted| item.adult == Some(wanted)),
                        )
                    })
                })
                .collect();
            for job in jobs {
                accepted.push(job.join().expect("rating worker panicked")?);
            }
            Ok::<_, anyhow::Error>(())
        })?;
    }
    let mut accepted = accepted.into_iter();
    items.retain(|_| accepted.next() == Some(true));
    Ok(items)
}

fn search_single(client: &Client, game: &str, query: &SearchQuery) -> Result<SearchPage> {
    let game = id_for_game(game)?.to_string();
    let page = query.page.max(1);
    let text = query.text.trim();
    let listing = text.is_empty()
        || query.category.is_some()
        || query.filters.release_type.is_some()
        || !query.filters.content_ratings.is_empty();
    let mut url = endpoint(if listing {
        "Mod/Index"
    } else {
        "Util/Search/Results"
    })?;
    {
        let mut params = url.query_pairs_mut();
        params
            .append_pair("_nPage", &page.to_string())
            .append_pair("_nPerpage", "15");
        if listing {
            let order = match query.sort {
                Sort::Relevance | Sort::Newest => "Generic_Newest",
                Sort::Popularity => "Generic_MostDownloaded",
                Sort::Updated => "Generic_LatestUpdated",
            };
            params
                .append_pair("_aFilters[Generic_Game]", &game)
                .append_pair("_sSort", order);
            if !text.is_empty() {
                params.append_pair("_aFilters[Generic_Name]", &format!("contains,{text}"));
            }
            if let Some(category) = &query.category {
                let category = category
                    .parse::<u64>()
                    .context("GameBanana category must be its numeric ID")?;
                params.append_pair("_aFilters[Generic_Category]", &category.to_string());
            }
            if let Some(release) = &query.filters.release_type {
                params.append_pair("_aFilters[Generic_ReleaseType]", release);
            }
            if let Some(rating) = query.filters.content_ratings.first() {
                params.append_pair("_aFilters[Generic_ContentRatings]", rating);
            }
        } else {
            let order = match query.sort {
                Sort::Relevance => "best_match",
                Sort::Popularity => "popularity",
                Sort::Newest => "date",
                Sort::Updated => "udate",
            };
            params
                .append_pair("_sSearchString", text)
                .append_pair("_sModelName", "Mod")
                .append_pair("_sOrder", order)
                .append_pair("_idGameRow", &game);
        }
    }
    let value = json(client, url, None)?;
    let root = obj(&value)?;
    let data = root.get("data").unwrap_or(&value);
    let records = data
        .get("_aRecords")
        .or_else(|| data.get("records"))
        .or_else(|| data.get("items"))
        .ok_or_else(|| anyhow!("GameBanana search response has no records"))?;
    let records = as_array(records, "search")?;
    let items = records
        .iter()
        .filter_map(|record| {
            let id = optional_num(record, "_idRow").or_else(|| optional_num(record, "id"))?;
            Some(SearchResult {
                id: RemoteMod {
                    source: Source::GameBanana,
                    game: game.to_owned(),
                    id,
                },
                name: optional_string(record, "_sName")
                    .or_else(|| optional_string(record, "name"))
                    .unwrap_or_else(|| format!("Mod {id}")),
                author: author(record),
                category: category(record),
                summary: optional_string(record, "_sDescription")
                    .or_else(|| optional_string(record, "description")),
                image: preview_image(
                    record
                        .get("_aPreviewMedia")
                        .or_else(|| record.get("preview_media")),
                ),
                version: optional_string(record, "_sVersion")
                    .or_else(|| optional_string(record, "version")),
                published_at: optional_num(record, "_tsDateAdded")
                    .or_else(|| optional_num(record, "date_added"))
                    .and_then(epoch_iso_timestamp),
                updated_at: optional_num(record, "_tsDateModified")
                    .or_else(|| optional_num(record, "date_modified"))
                    .and_then(epoch_iso_timestamp),
                url: optional_string(record, "_sProfileUrl")
                    .or_else(|| optional_string(record, "url")),
                downloads: optional_num(record, "_nDownloadCount")
                    .or_else(|| optional_num(record, "download_count")),
                endorsements: None,
                likes: optional_num(record, "_nLikeCount")
                    .or_else(|| optional_num(record, "like_count")),
                adult: content_rating(record),
            })
        })
        .collect::<Vec<_>>();
    let pagination = data.get("pagination").unwrap_or(data);
    let metadata = data
        .get("_aMetadata")
        .or_else(|| root.get("_aMetadata"))
        .unwrap_or(pagination);
    let total = root
        .get("_nRecordCount")
        .or_else(|| root.get("record_count"))
        .or_else(|| metadata.get("_nRecordCount"))
        .or_else(|| pagination.get("total"))
        .and_then(numeric);
    let per_page = optional_num(metadata, "_nPerpage")
        .filter(|&per_page| per_page > 0)
        .unwrap_or(15);
    let has_more = metadata
        .get("_bIsComplete")
        .and_then(Value::as_bool)
        .map(|complete| !complete)
        .or_else(|| total.map(|count| u64::from(page).saturating_mul(per_page) < count))
        .unwrap_or(records.len() as u64 == per_page);
    Ok(SearchPage {
        items,
        page,
        total,
        total_pages: total.map(|count| count.div_ceil(per_page)),
        has_more,
    })
}

fn content_rating(value: &Value) -> Option<bool> {
    if let Some(adult) = value.get("_bIsNsfw").and_then(Value::as_bool) {
        return Some(adult);
    }
    if let Some(ratings) = value.get("_aContentRatings").and_then(Value::as_object) {
        if ratings.keys().any(|rating| {
            matches!(
                rating.as_str(),
                "cp" | "st"
                    | "sc"
                    | "bg"
                    | "au"
                    | "tu"
                    | "du"
                    | "sa"
                    | "lp"
                    | "pn"
                    | "nu"
                    | "iv"
                    | "ft"
            )
        }) {
            return Some(true);
        }
        return ratings.keys().all(|rating| rating == "ps").then_some(false);
    }
    if value
        .get("_aContentRatings")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
        || value.get("_bHasContentRatings").and_then(Value::as_bool) == Some(false)
    {
        return Some(false);
    }
    None
}

fn fetch_profile_ratings(client: &Client, id: u64) -> Result<Value> {
    let mut response: Value = client
        .get(endpoint(&format!("Mod/{id}/ProfilePage"))?)
        .timeout(Duration::from_secs(5))
        .send()?
        .error_for_status()?
        .json()?;
    if let Some(data) = response.get_mut("data") {
        return Ok(data.take());
    }
    Ok(response)
}
fn has_all_ratings(profile: &Value, selected: &[String]) -> bool {
    profile
        .get("_aContentRatings")
        .and_then(Value::as_object)
        .is_some_and(|ratings| selected.iter().all(|rating| ratings.contains_key(rating)))
}

pub(super) fn details(client: &Client, id: &RemoteMod) -> Result<ModDetails> {
    let response = json(
        client,
        endpoint(&format!("Mod/{}/ProfilePage", id.id))?,
        None,
    )?;
    let value = response.get("data").unwrap_or(&response);
    let expected_game = id_for_game(&id.game)?;
    let actual_game = value
        .get("_aGame")
        .and_then(|game| optional_num(game, "_idRow"))
        .ok_or_else(|| anyhow!("GameBanana mod response has no game ID"))?;
    if actual_game != expected_game {
        bail!("GameBanana mod does not belong to the selected game");
    }
    let name = optional_string(value, "_sName").unwrap_or_else(|| format!("Mod {}", id.id));
    let files: Vec<ModFile> = value
        .get("_aFiles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(parse_file)
        .collect();
    let images = value
        .get("_aPreviewMedia")
        .map(preview_images)
        .unwrap_or_default();
    let requirements = value
        .get("_aRequirements")
        .map(requirements)
        .unwrap_or_default();
    let summary = optional_string(value, "_sDescription");
    let created_at = optional_num(value, "_tsDateAdded").and_then(epoch_iso_timestamp);
    let updated_at = optional_num(value, "_tsDateModified").and_then(epoch_iso_timestamp);
    let version = files.iter().find_map(|file: &ModFile| file.version.clone());
    Ok(ModDetails {
        id: id.clone(),
        name,
        summary,
        description: optional_string(value, "_sText"),
        version,
        author: author(value),
        category: category(value),
        updated_at,
        created_at,
        url: optional_string(value, "_sProfileUrl").or_else(|| Some(format!("{WEB}{}", id.id))),
        images,
        adult: content_rating(value),
        requirements,
        files,
    })
}

fn author(value: &Value) -> Option<String> {
    optional_string(value.get("_aSubmitter")?, "_sName")
}

fn category(value: &Value) -> Option<String> {
    ["_aCategory", "_aSubCategory", "_aRootCategory"]
        .iter()
        .find_map(|key| optional_string(value.get(*key)?, "_sName"))
}

fn parse_file(value: &Value) -> Option<ModFile> {
    Some(ModFile {
        id: optional_num(value, "_idRow").or_else(|| optional_num(value, "id"))?,
        name: optional_string(value, "_sFile")
            .or_else(|| optional_string(value, "_sName"))
            .or_else(|| optional_string(value, "name"))
            .unwrap_or_else(|| "Download".to_owned()),
        version: optional_string(value, "_sVersion").or_else(|| optional_string(value, "version")),
        updated_at: optional_num(value, "_tsDateAdded").and_then(epoch_iso_timestamp),
        size: optional_num(value, "_nFilesize"),
        downloads: optional_num(value, "_nDownloadCount"),
        primary: value
            .get("_bIsPrimary")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        category: optional_string(value, "_sFileType"),
        description: optional_string(value, "_sDescription"),
        url: optional_string(value, "_sDownloadUrl"),
    })
}

fn image_url(item: &Value) -> Option<String> {
    if let Some(base) = item.get("_sBaseUrl").and_then(string) {
        if let Some(file) = item.get("_sFile").and_then(string) {
            if Url::parse(&file).is_ok() {
                return Some(file);
            }
            return Some(format!(
                "{}/{}",
                base.trim_end_matches('/'),
                file.trim_start_matches('/')
            ));
        }
        return Some(base);
    }
    item.get("_sUrl").and_then(string)
}

fn image_thumbnail(item: &Value) -> Option<String> {
    let base = item.get("_sBaseUrl").and_then(string);
    ["_sFile220", "_sFile530"]
        .iter()
        .find_map(|key| {
            let file = item.get(*key).and_then(string)?;
            if Url::parse(&file).is_ok() {
                Some(file)
            } else {
                base.as_deref().map(|base| {
                    format!(
                        "{}/{}",
                        base.trim_end_matches('/'),
                        file.trim_start_matches('/')
                    )
                })
            }
        })
        .or_else(|| item.get("_sThumbnailUrl").and_then(string))
}

fn image_items(value: &Value) -> Option<&[Value]> {
    value
        .as_array()
        .or_else(|| value.get("_aImages").and_then(Value::as_array))
        .map(Vec::as_slice)
}

fn preview_image(value: Option<&Value>) -> Option<String> {
    let value = value?;
    image_items(value)
        .and_then(|items| items.first())
        .and_then(|item| image_thumbnail(item).or_else(|| image_url(item)))
}

fn preview_images(value: &Value) -> Vec<ModImage> {
    image_items(value)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            image_url(item).map(|url| ModImage {
                url,
                thumbnail: image_thumbnail(item),
            })
        })
        .collect()
}

fn requirements(value: &Value) -> Vec<Requirement> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            Value::String(text) => Some(Requirement {
                name: text.clone(),
                url: None,
                notes: None,
                thumbnail: None,
            }),
            Value::Array(parts) => Some(Requirement {
                name: parts.first().and_then(string)?,
                url: parts.get(1).and_then(string).filter(|url| !url.is_empty()),
                notes: None,
                thumbnail: None,
            }),
            Value::Object(_) => item
                .get("_sName")
                .and_then(string)
                .or_else(|| item.get("_sDescription").and_then(string))
                .map(|name| Requirement {
                    name,
                    url: optional_string(item, "_sProfileUrl"),
                    notes: optional_string(item, "_sDescription"),
                    thumbnail: optional_string(item, "_sThumbnailUrl"),
                }),
            _ => None,
        })
        .collect()
}

pub(super) fn download(client: &Client, id: &RemoteMod, file: &ModFile) -> Result<DownloadAction> {
    let _ = (client, id);
    let url = file
        .url
        .clone()
        .unwrap_or_else(|| format!("https://gamebanana.com/dl/{}", file.id));
    let url = Url::parse(&url).context("invalid GameBanana download URL")?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("gamebanana.com" | "files.gamebanana.com")
        )
    {
        bail!("unsupported GameBanana download URL");
    }
    Ok(DownloadAction::Direct(url))
}

pub(super) fn filter_catalog(client: &Client, game: &str) -> Result<FilterCatalog> {
    let config = json(client, endpoint("Mod/ListFilterConfig")?, None)?;
    let filters = config
        .get("_aFilters")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("GameBanana did not return a filter catalog"))?;
    let choices = |alias: &str| {
        filters
            .iter()
            .find(|filter| filter.get("_sAlias").and_then(Value::as_str) == Some(alias))
            .and_then(|filter| filter.get("_aOptions").and_then(Value::as_array))
            .into_iter()
            .flatten()
            .filter_map(|value| {
                let id = optional_string(value, "_sAlias")?;
                (id != "any").then_some(FilterOption {
                    id,
                    name: optional_string(value, "_sTitle")?,
                })
            })
            .collect()
    };
    Ok(FilterCatalog {
        categories: categories(client, game)?,
        release_types: choices("Generic_ReleaseType"),
        content_ratings: choices("Generic_ContentRatings"),
    })
}

pub(super) fn categories(client: &Client, game: &str) -> Result<Vec<Category>> {
    let mut url = endpoint("Util/ModCategory/NestedStructure")?;
    url.query_pairs_mut()
        .append_pair("_idGameRow", &id_for_game(game)?.to_string());
    let value = json(client, url, None)?;
    let mut categories = Vec::new();
    for item in as_array(&value, "category")? {
        collect_categories(item, None, &mut categories);
    }
    Ok(categories)
}

fn collect_categories(item: &Value, parent: Option<&str>, output: &mut Vec<Category>) {
    let Some(id) = optional_num(item, "_idRow") else {
        return;
    };
    let Some(name) = item.get("_sName").and_then(Value::as_str) else {
        return;
    };
    let id = id.to_string();
    output.push(Category {
        id: id.clone(),
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
        icon: optional_string(item, "_sIconUrl"),
    });
    if let Some(children) = item.get("_aChildren").and_then(Value::as_array) {
        for child in children {
            collect_categories(child, Some(&id), output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Cursor, collect_categories, content_rating, has_all_ratings, pack_page, parse_file,
        preview_images, requirements,
    };
    use serde_json::json;

    #[test]
    fn filtered_page_packs_matches_across_source_pages_without_losing_lookahead() {
        use super::{RemoteMod, SearchResult, Source};
        let items = |range: std::ops::RangeInclusive<u64>| {
            range
                .map(|id| SearchResult {
                    id: RemoteMod {
                        id,
                        source: Source::GameBanana,
                        game: "19567".into(),
                    },
                    name: id.to_string(),
                    author: None,
                    category: None,
                    summary: None,
                    image: None,
                    version: None,
                    updated_at: None,
                    published_at: None,
                    url: None,
                    downloads: None,
                    endorsements: None,
                    likes: None,
                    adult: Some(true),
                })
                .collect()
        };
        let first = pack_page(
            1,
            Cursor {
                source_page: 1,
                ..Cursor::default()
            },
            |p| {
                Ok(if p == 1 {
                    (items(1..=7), Some(2))
                } else {
                    (items(8..=17), Some(3))
                })
            },
        )
        .unwrap();
        assert_eq!(first.result.items.len(), 15);
        assert!(first.result.has_more);
        let last = pack_page(2, first.next.unwrap(), |p| {
            assert_eq!(p, 3);
            Ok((items(18..=20), None))
        })
        .unwrap();
        assert_eq!(
            last.result
                .items
                .iter()
                .map(|item| item.id.id)
                .collect::<Vec<_>>(),
            [16, 17, 18, 19, 20]
        );
        assert_eq!(last.result.total_pages, Some(2));
        assert_eq!(last.result.total, Some(20));
        assert!(!last.result.has_more);
    }

    #[test]
    fn distinguishes_mature_unrated_and_unknown_submissions() {
        assert_eq!(
            content_rating(&json!({"_bHasContentRatings": false})),
            Some(false)
        );
        assert_eq!(
            content_rating(&json!({"_aContentRatings": {"ps": "Flashing lights"}})),
            Some(false)
        );
        assert_eq!(
            content_rating(&json!({"_aContentRatings": {"nu": "Full nudity"}})),
            Some(true)
        );
        assert_eq!(
            content_rating(&json!({"_aContentRatings": {"rp": "Pending"}})),
            None
        );
        assert_eq!(content_rating(&json!({"_bHasContentRatings": true})), None);
    }

    #[test]
    fn rating_multi_select_uses_all_selected_profile_codes() {
        let profile = json!({"_aContentRatings": {"iv": "Flashing lights", "nu": "Nudity"}});
        assert!(has_all_ratings(&profile, &["iv".into(), "nu".into()]));
        assert!(!has_all_ratings(&profile, &["iv".into(), "cp".into()]));
        assert!(!has_all_ratings(
            &json!({"_bHasContentRatings": true}),
            &["iv".into()]
        ));
    }

    #[test]
    fn category_catalog_keeps_short_names_parent_ids_and_optional_icons() {
        let mut categories = Vec::new();
        collect_categories(
            &json!({
                "_idRow": 1,
                "_sName": "Character Skins",
                "_sIconUrl": "https://img.example/root.png",
                "_aChildren": [
                    {"_idRow": 2, "_sName": "Nicole", "_sIconUrl": "https://img.example/nicole.png"},
                    {"_idRow": 3, "_sName": "Bangboo"}
                ]
            }),
            None,
            &mut categories,
        );
        assert_eq!(categories[0].name, "Character Skins");
        assert_eq!(categories[0].parent, None);
        assert_eq!(
            categories[0].icon.as_deref(),
            Some("https://img.example/root.png")
        );
        assert_eq!(categories[1].name, "Nicole");
        assert_eq!(categories[1].parent.as_deref(), Some("1"));
        assert_eq!(
            categories[1].icon.as_deref(),
            Some("https://img.example/nicole.png")
        );
        assert_eq!(categories[2].parent.as_deref(), Some("1"));
        assert_eq!(categories[2].icon, None);
    }

    #[test]
    fn decodes_gamebanana_file_metadata_and_previews() {
        let file = parse_file(&json!({
            "_idRow": 417,
            "_sFile": "mod.zip",
            "_sVersion": "2.1",
            "_nFilesize": 2048,
            "_nDownloadCount": 42,
            "_bIsPrimary": true,
            "_sFileType": "Main",
            "_sDownloadUrl": "https://gamebanana.com/dl/417"
        }))
        .unwrap();
        assert_eq!(file.id, 417);
        assert_eq!(file.name, "mod.zip");
        assert_eq!(file.version.as_deref(), Some("2.1"));
        assert_eq!(file.size, Some(2048));
        assert_eq!(file.downloads, Some(42));
        assert!(file.primary);
        let images = preview_images(&json!({"_aImages":[{
            "_sBaseUrl":"https://img.example/mods", "_sFile":"a.png",
            "_sFile220":"https://img.example/mods/a-small.png"
        }]}));
        assert_eq!(images[0].url, "https://img.example/mods/a.png");
        assert_eq!(
            images[0].thumbnail.as_deref(),
            Some("https://img.example/mods/a-small.png")
        );
    }

    #[test]
    fn profile_requirement_pairs_keep_their_external_links() {
        let parsed = requirements(&json!([
            ["ShareMii", "https://gamebanana.com/tools/22305"],
            ["Base game", ""],
            "No additional files"
        ]));
        assert_eq!(
            parsed
                .iter()
                .map(|item| (item.name.as_str(), item.url.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("ShareMii", Some("https://gamebanana.com/tools/22305")),
                ("Base game", None),
                ("No additional files", None),
            ]
        );
    }
}
