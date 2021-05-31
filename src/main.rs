use csv::WriterBuilder;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, error::Error};
use chrono::{Duration, Utc};
use evg_api_rs::EvgClient;
use graphql_client::GraphQLQuery;
use futures::{StreamExt, future::join_all, pin_mut};


#[derive(Debug, Serialize)]
pub struct Record {
    pub author: String,
    pub alias: String,
    pub build_variant: String,
    pub tasks: String,
    pub n_tasks: usize,
}

#[derive(Debug, Deserialize)]
pub struct VariantsTasks {
    pub name: String,
    pub tasks: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct APatch {
    pub id: String,
    pub description: String,
    pub author: String,
    pub alias: String,
    pub variantsTasks: Vec<VariantsTasks>,
}

#[derive(Debug, Deserialize)]
pub struct PatchDets {
    pub patch: APatch,
}


#[derive(Debug, Deserialize)]
pub struct PatchResponse {
    pub data: PatchDets,
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/graphql/schema.graphql",
    query_path = "src/graphql/query.graphql",
    response_derives = "Debug",
)]
pub struct PatchDetails;

#[tokio::main]
async fn main() {
    let project = std::env::args().nth(1).expect("Expected project id");
    let evg_client = EvgClient::new().unwrap();
    let patch_stream = evg_client.stream_project_patches(&project, None).await;
    pin_mut!(patch_stream);

    let lookback = Utc::now() - Duration::weeks(1);
    // let lookback = Utc::now() - Duration::days(1);
    let mut count = 0;
    let mut patches_per_person = HashMap::new();

    let mut patch_ids = vec![];
    while let Some(patch) = patch_stream.next().await {
        if patch.create_time < lookback {
            break;
        }

        if let Some(ppp) = patches_per_person.get_mut(&patch.author) {
            *ppp += 1;
        } else {
            patches_per_person.insert(patch.author.to_string(), 1);
        }
        patch_ids.push(patch.patch_id.to_string());

        println!("{}: {}: {}", patch.create_time.to_string(), patch.author, patch.description);
        count += 1;
    }

    println!("Patches: {}", count);
    patches_per_person.iter().for_each(|(a,c)| println!("{}: {}", a, c));
    println!("");

    display_patches(&evg_client, patch_ids).await;
}

async fn display_patches(evg_client: &EvgClient, patch_list: Vec<String>) {
    let patches = join_all(patch_list.iter().map(|p| get_patch(evg_client, p))).await;

    let mut build_variants = HashMap::new();
    let mut build_counts = vec![];
    let mut records: Vec<Record> = vec![];

    for p in patches {
        match p {
            Ok(patch) => {
                if patch.data.patch.alias == "__commit_queue" {
                    continue;
                }
                build_counts.push(patch.data.patch.variantsTasks.len());
                for bv in patch.data.patch.variantsTasks {
                    records.push(Record {
                        author: patch.data.patch.author.to_string(),
                        alias: patch.data.patch.alias.to_string(),
                        build_variant: bv.name.to_string(),
                        tasks: bv.tasks.join("|"),
                        n_tasks: bv.tasks.len(),
                    });
                    if let Some(b) = build_variants.get_mut(&bv.name) {
                        *b += 1;
                    } else {
                        build_variants.insert(bv.name.to_string(), 1);
                    }
                }
                // println!("{:#?}", &patch);
            }
            Err(e) => {
                println!("{}", e);
            }
        }
    }

    let total_builds: usize = build_counts.iter().sum();
    println!("Avg Build Variants: {}", total_builds / build_counts.len());
    build_variants.iter().for_each(|(a,c)| println!("{}: {}", a, c));

    let mut wtr = WriterBuilder::new().from_path("patches.csv").unwrap();
    records.iter().for_each(|r| wtr.serialize(r).unwrap());
    wtr.flush().unwrap();
}

async fn get_patch(client: &EvgClient, patch_id: &str) -> Result<PatchResponse, Box<dyn Error>> {
    let variables = patch_details::Variables {
        patch_id: patch_id.to_string(),
    };
    let request_body = PatchDetails::build_query(variables);

    let url = "https://evergreen.mongodb.com/graphql/query";
    let res = client.client.post(url).json(&request_body).send().await?;
    let response_body = res.json().await;
    Ok(response_body?)
}
