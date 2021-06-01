use csv::WriterBuilder;
use serde::{Deserialize, Serialize};
use std::{error::Error, sync::mpsc::{self, Receiver}, thread};
use structopt::StructOpt;
use chrono::{Duration, Utc};
use evg_api_rs::EvgClient;
use graphql_client::GraphQLQuery;
use futures::{StreamExt, future::join_all, pin_mut};

#[derive(Debug, Serialize)]
pub struct Record {
    pub id: String,
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
    #[serde(rename = "variantsTasks")]
    pub variants_tasks: Vec<VariantsTasks>,
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

#[derive(Debug, StructOpt)]
#[structopt(about = "Generate a CSV of patches run on the given projects.")]
struct Opt {
    /// Evergreen project ID.
    #[structopt(long)]
    project_id: String,

    /// Number of weeks of data to gather.
    #[structopt(long, default_value = "1")]
    weeks_back: i64,

    /// Number of patches to query at a time.
    #[structopt(long, default_value = "50")]
    batch_size: usize,

    /// Name of output file to write.
    #[structopt(long, default_value = "patches.csv")]
    output_file: String,
}

#[tokio::main]
async fn main() {
    let opt = Opt::from_args();

    let evg_client = EvgClient::new().unwrap();
    let patch_stream = evg_client.stream_project_patches(&opt.project_id, None).await;
    pin_mut!(patch_stream);

    let (tx, rx) = mpsc::channel();

    let batch_size = opt.batch_size;
    let output_file = opt.output_file.to_string();
    let handle = thread::spawn(move || {
        patch_collector(rx, batch_size, &output_file);
    });

    let lookback = Utc::now() - Duration::weeks(opt.weeks_back);
    let mut count = 0;

    while let Some(patch) = patch_stream.next().await {
        if patch.create_time < lookback {
            println!("Out of patches: {}", count);
            tx.send(Action::End).unwrap();
            break;
        }

        if count % 25 == 0 {
            println!("{}: {}", count, &patch.create_time);
        }

        tx.send(Action::Patch(patch.patch_id)).unwrap();

        count += 1;
    }

    handle.join().unwrap();
}


enum Action {
    Patch(String),
    End,
}

#[tokio::main]
async fn patch_collector(rx: Receiver<Action>, batch_size: usize, output_file: &str) {
    let evg_client = EvgClient::new().unwrap();

    let mut patch_ids = vec![];
    let mut records: Vec<Record> = vec![];
    let mut count = 0;
    while let Ok(event) = rx.recv() {
        match event {
            Action::Patch(patch_id) => {
                patch_ids.push(patch_id);
                if patch_ids.len() >= batch_size {
                    count += 1;
                    println!("Sending batch: {}", count);
                    let patches = join_all(patch_ids.iter().map(|p| get_patch(&evg_client, p))).await;
                    records.append(&mut process_patches(&patches));
                    patch_ids = vec![];
                    println!("Batch completed: {}", count);
                }
            }
            Action::End => {
                if patch_ids.len() > 0 {
                    let patches = join_all(patch_ids.iter().map(|p| get_patch(&evg_client, p))).await;
                    records.append(&mut process_patches(&patches));
                }

                break;
            }
        }
    }

    let mut wtr = WriterBuilder::new().from_path(output_file).unwrap();
    records.iter().for_each(|r| wtr.serialize(r).unwrap());
    wtr.flush().unwrap();
}

fn process_patches(patches: &[Result<PatchResponse, Box<dyn Error>>]) -> Vec<Record> {
    let mut records = vec![];
    for p in patches {
        match p {
            Ok(patch) => {
                if patch.data.patch.alias == "__commit_queue" {
                    continue;
                }
                for bv in &patch.data.patch.variants_tasks {
                    records.push(Record {
                        id: patch.data.patch.id.to_string(),
                        author: patch.data.patch.author.to_string(),
                        alias: patch.data.patch.alias.to_string(),
                        build_variant: bv.name.to_string(),
                        tasks: bv.tasks.join("|"),
                        n_tasks: bv.tasks.len(),
                    });
                }
            }
            Err(e) => {
                println!("{}", e);
            }
        }
    }

    records
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
