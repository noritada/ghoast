use anyhow::*;
use graphql_client::{GraphQLQuery, Response};
use std::io::Write;

use crate::common::*;

type DateTime = String;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/schema.graphql",
    query_path = "src/repositories_query.graphql",
    response_derives = "Debug"
)]
struct RepositoriesView;

fn query(
    config: &Config,
    repositories_cursor: Option<String>,
    iter_num: u8,
) -> std::result::Result<Response<repositories_view::ResponseData>, anyhow::Error> {
    let q = repositories_view::Variables {
        organization: config.org.clone(),
        repositories_cursor,
    };

    let extra_info = format!("with repositories_cursor={:?}", &q.repositories_cursor);
    print_progress(Progress::Downloading, &extra_info)?;
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://api.github.com/graphql")
        .bearer_auth(&config.github_access_token)
        .header(reqwest::header::USER_AGENT, "ghoast/0.1.0")
        .json(&q)
        .send()?;

    resp.error_for_status_ref()?;
    print_progress(Progress::Downloaded, &extra_info)?;

    let resp_text = resp.text()?;
    if let Some(cache_file_prefix) = &config.cache_file_prefix {
        let cache_file_path = format!("{}.{:02}", cache_file_prefix, iter_num);
        let mut cache_file = std::fs::File::create(cache_file_path)?;
        cache_file.write_all(resp_text.as_ref())?;
    }

    let json_root: Response<repositories_view::ResponseData> = serde_json::from_str(&resp_text)?;

    Ok(json_root)
}

fn extract(
    json_root: Response<repositories_view::ResponseData>,
) -> std::result::Result<
    (
        Vec<Option<repositories_view::RepositoriesViewOrganizationRepositoriesEdges>>,
        repositories_view::RepositoriesViewOrganizationRepositoriesPageInfo,
    ),
    anyhow::Error,
> {
    if let Some(errors) = json_root.errors {
        let messages = errors
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("\n    ");

        bail!("Resulted in output with errors:\n    {}", messages);
    }

    let organization = json_root
        .data
        .and_then(|d| d.organization)
        .ok_or_else(|| anyhow!("organization info not found"))?;

    let repositories = organization
        .repositories
        .edges
        .ok_or_else(|| anyhow!("repositories list not found"))?;

    let repositories_page_info = organization.repositories.page_info;

    Ok((repositories, repositories_page_info))
}

pub(crate) fn process(config: &Config) -> std::result::Result<(), anyhow::Error> {
    let mut repositories_list = Vec::new();
    let mut repositories_cursor = None;
    let mut num = 0;

    let local: chrono::prelude::DateTime<chrono::prelude::Local> = chrono::prelude::Local::now();
    let out_fname = local.format(&config.out_csv_file).to_string();

    loop {
        let json_root = query(config, repositories_cursor, num)?;
        let (repositories, repositories_page_info) = extract(json_root)?;
        repositories_list.push(repositories);

        if !repositories_page_info.has_next_page {
            break;
        }

        repositories_cursor = repositories_page_info.end_cursor;
        num += 1;
    }

    for repositories in &repositories_list {
        for repository in repositories.iter().flatten() {
            if repository.node.is_none() {
                continue;
            }
            let node = repository.node.as_ref().unwrap();

            if let Some(languages) = &node.languages {
                if languages.page_info.has_next_page {
                    bail!(
                        "'Languages' needs pagenation support!
    Repository: {}
    Repository ID: {}
    Language's End Cursor: {}",
                        node.name,
                        node.id,
                        languages.page_info.end_cursor.as_ref().unwrap()
                    )
                }
            }
        }
    }

    let mut writer = csv::Writer::from_path(out_fname)?;
    writer.write_record(&[
        "id",
        "database_id",
        "name",
        "created_at",
        "updated_at",
        "is_fork",
        "is_private",
        "primary_language",
        "languages",
        "description",
    ])?;

    for repositories in repositories_list {
        for repository in repositories.into_iter().flatten() {
            if repository.node.is_none() {
                continue;
            }
            let node = repository.node.unwrap();

            let primary_language = node.primary_language.map(|n| n.name);

            let languages = match node.languages {
                Some(
                    repositories_view::RepositoriesViewOrganizationRepositoriesEdgesNodeLanguages {
                        edges: Some(languages),
                        ..
                    },
                ) => languages
                    .into_iter()
                    .filter_map(|o| o.map(|e| format!("{}:{}", e.node.name, e.size)))
                    .collect::<Vec<_>>()
                    .join(";"),
                _ => String::new(),
            };

            writer.serialize((
                node.id,
                node.database_id,
                node.name,
                node.created_at,
                node.updated_at,
                node.is_fork,
                node.is_private,
                primary_language,
                languages,
                node.description,
            ))?;
        }
    }

    writer.flush()?;

    Ok(())
}
