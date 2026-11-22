use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::Client;
use serde_json::Value;
use url::Url;

use super::{
    Category, DownloadAction, FilterCatalog, FilterOption, Maturity, ModDetails, ModFile, ModImage,
    RemoteMod, Requirement, SearchPage, SearchQuery, SearchResult, Sort, Source,
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

pub(super) fn search(client: &Client, game: &str, query: &SearchQuery) -> Result<SearchPage> {
    anyhow::ensure!(
        query.maturity == Maturity::All,
        "GameBanana does not support SFW/NSFW maturity filters; use site content ratings"
    );
    anyhow::ensure!(
        query.filters.content_ratings.len() <= 1
            || !query
                .filters
                .content_ratings
                .iter()
                .any(|rating| rating == "-"),
        "GameBanana's Unrated filter cannot be combined with other content ratings"
    );
    search_single(client, game, query)
}

fn listing_sort(sort: Sort) -> &'static str {
    match sort {
        Sort::Relevance | Sort::Newest => "Generic_Newest",
        Sort::Popularity => "Generic_MostDownloaded",
        Sort::Likes => "Generic_MostLiked",
        Sort::Updated => "Generic_LatestUpdated",
    }
}

fn search_single(client: &Client, game: &str, query: &SearchQuery) -> Result<SearchPage> {
    let game = id_for_game(game)?.to_string();
    let page = query.page.max(1);
    let text = query.text.trim();
    let listing = text.is_empty()
        || query.category.is_some()
        || query.filters.release_type.is_some()
        || !query.filters.content_ratings.is_empty()
        || query.sort == Sort::Likes;
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
            params
                .append_pair("_aFilters[Generic_Game]", &game)
                .append_pair("_sSort", listing_sort(query.sort));
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
            if !query.filters.content_ratings.is_empty() {
                params.append_pair(
                    "_aFilters[Generic_ContentRatings]",
                    &query.filters.content_ratings.join(","),
                );
            }
        } else {
            let order = match query.sort {
                Sort::Relevance => "best_match",
                Sort::Popularity | Sort::Likes => "popularity",
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
    value.get("_bIsNsfw").and_then(Value::as_bool)
}

fn tags(value: &Value) -> Vec<String> {
    let mut tags: Vec<String> = value
        .get("_aTags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tag| {
            let value = optional_string(tag, "_sValue")?;
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            let title = optional_string(tag, "_sTitle")
                .map(|title| title.trim().to_owned())
                .filter(|title| !title.is_empty());
            Some(title.map_or_else(|| value.to_owned(), |title| format!("{title}: {value}")))
        })
        .collect();
    let parent = value
        .get("_aSuperCategory")
        .and_then(|value| optional_string(value, "_sName"));
    if parent.as_deref() == Some("Character Skins")
        && let Some(name) = value
            .get("_aCategory")
            .and_then(|value| optional_string(value, "_sName"))
    {
        let tag = format!("Character: {name}");
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags
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
        tags: tags(value),
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
    use serde_json::json;

    use crate::sources::ProviderFilters;

    use super::{
        Maturity, SearchQuery, Sort, collect_categories, content_rating, listing_sort, parse_file,
        preview_images, requirements, search, tags,
    };

    #[test]
    fn likes_sort_uses_gamebanana_list_filter_wire_alias() {
        assert_eq!(listing_sort(Sort::Likes), "Generic_MostLiked");
        assert_eq!(listing_sort(Sort::Popularity), "Generic_MostDownloaded");
    }

    #[test]
    fn rejects_unsupported_maturity_and_unrated_combinations() {
        let client = reqwest::blocking::Client::new();
        let query = SearchQuery {
            maturity: Maturity::Safe,
            ..SearchQuery::default()
        };
        assert!(search(&client, "19567", &query).is_err());
        let query = SearchQuery {
            maturity: Maturity::All,
            filters: ProviderFilters {
                content_ratings: vec!["-".into(), "nu".into()],
                ..ProviderFilters::default()
            },
            ..SearchQuery::default()
        };
        assert!(search(&client, "19567", &query).is_err());
    }

    #[test]
    fn content_rating_uses_only_the_explicit_nsfw_flag() {
        assert_eq!(content_rating(&json!({"_bIsNsfw": true})), Some(true));
        assert_eq!(content_rating(&json!({"_bIsNsfw": false})), Some(false));
        assert_eq!(
            content_rating(&json!({"_aContentRatings": {"nu": "Full Nudity"}})),
            None
        );
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
    fn gamebanana_tags_use_published_tag_values_and_ignore_missing_values() {
        assert_eq!(
            tags(&json!({"_aTags": [
                {"_sTitle": "Character", "_sValue": "Mario"},
                {"_sTitle": "Genre", "_sValue": "Platformer"},
                {"_sTitle": "Empty", "_sValue": ""},
                {"_sTitle": "Missing"}
            ]})),
            ["Character: Mario", "Genre: Platformer"]
        );
        assert!(tags(&json!({})).is_empty());
    }

    #[test]
    fn character_subcategory_is_saved_even_without_explicit_provider_tags() {
        let profile = json!({
            "_aTags": [],
            "_aCategory": {"_sName": "Corin Wickes"},
            "_aSuperCategory": {"_sName": "Character Skins"},
        });
        assert_eq!(tags(&profile), ["Character: Corin Wickes"]);
        assert!(
            tags(&json!({
                "_aCategory": {"_sName": "UI"},
                "_aSuperCategory": {"_sName": "Other"},
            }))
            .is_empty()
        );
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
