use csv::WriterBuilder;
use serde::Serialize;
use std::{error::Error, sync::mpsc::{self, Receiver}, thread};
use structopt::StructOpt;
use chrono::{Duration, Utc};
use evg_api_rs::EvgClient;
use futures::{StreamExt, future::join_all, pin_mut};
use cynic::{QueryBuilder, QueryFragment};

mod schema {
    cynic::use_schema!("src/graphql/schema.graphql");
}

#[derive(Debug, Serialize)]
pub struct Record {
    pub id: String,
    pub author: String,
    pub alias: String,
    pub build_variant: String,
    pub tasks: String,
    pub n_tasks: usize,
}

#[derive(QueryFragment, Debug)]
#[cynic(schema_path = "src/graphql/schema.graphql")]
pub struct VariantTask {
    pub name: String,
    pub tasks: Vec<String>,
}

#[derive(QueryFragment, Debug)]
#[cynic(
    schema_path = "src/graphql/schema.graphql"
)]
pub struct Patch {
    pub id: cynic::Id,
    pub description: String,
    pub author: String,
    pub alias: Option<String>,
    pub variants_tasks: Vec<Option<VariantTask>>,
}

#[derive(cynic::FragmentArguments)]
struct PatchArguments {
    id: String,
}

#[derive(QueryFragment, Debug)]
#[cynic(
    graphql_type = "Query",
    schema_path = "src/graphql/schema.graphql",
    argument_struct = "PatchArguments"
)]
struct PatchQuery {
    #[arguments(id = &args.id)]
    patch: Patch,
}

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

fn process_patches(patches: &[Result<PatchQuery, Box<dyn Error>>]) -> Vec<Record> {
    let mut records = vec![];
    for p in patches {
        match p {
            Ok(patch) => {
                if patch.patch.alias == Some("__commit_queue".to_string()) {
                    continue;
                }
                for bv in &patch.patch.variants_tasks {
                    if let Some(vt) = bv {
                        records.push(Record {
                            id: String::from(&patch.patch.id.clone().into_inner()),
                            author: patch.patch.author.to_string(),
                            alias: patch.patch.alias.clone().unwrap_or(String::from("")),
                            build_variant: vt.name.to_string(),
                            tasks: vt.tasks.join("|"),
                            n_tasks: vt.tasks.len(),
                        });
                    }
                }
            }
            Err(e) => {
                println!("{}", e);
            }
        }
    }

    records
}

async fn get_patch(client: &EvgClient, patch_id: &str) -> Result<PatchQuery, Box<dyn Error>> {
    let operation = PatchQuery::build(
        PatchArguments {
            id: patch_id.into(),
        }
    );
    let url = "https://evergreen.mongodb.com/graphql/query";
    let res = client.client.post(url).json(&operation).send().await?;
    let response_body = operation.decode_response(res.json().await?)?;
    Ok(response_body.data.unwrap())
}
