use super::default_tokenizers::default_tokenizers;
use super::index_writer_holder::IndexWriterHolder;
use crate::configs::ConfigHolder;
use crate::configs::Persistable;
use crate::configs::{IndexConfig, IndexConfigHolder};
use crate::consumers::kafka::KafkaConsumer;
use crate::errors::{Error, SummaResult};
use crate::proto;
use crate::search_engine::index_layout::IndexLayout;
use crate::search_engine::IndexUpdater;
use crate::utils::sync::{Handler, OwningHandler};
use crate::utils::thread_handler::ThreadHandler;
use std::time::Duration;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::Schema;
use tantivy::{Index, IndexReader, IndexSettings, LeasedItem, Searcher};
use tokio::sync::oneshot;
use tokio::time;
use tracing::{info, instrument, warn};

pub struct IndexHolder {
    index_name: String,
    index_layout: IndexLayout,
    schema: Schema,
    index: Index,
    index_config: IndexConfigHolder,
    index_reader: IndexReader,
    /// All modifying operations are isolated inside `index_updater`
    index_updater: OwningHandler<IndexUpdater>,
    autocommit_thread: Option<ThreadHandler>,
}

impl IndexHolder {
    pub(crate) fn new(index_name: &str, index_config: IndexConfig, index_layout: &IndexLayout, schema: &Schema, consumers: Vec<KafkaConsumer>) -> SummaResult<IndexHolder> {
        IndexHolder::create(index_config, index_layout, schema)?;
        IndexHolder::open(index_name, index_layout, consumers)
    }

    #[instrument]
    fn create(index_config: IndexConfig, index_layout: &IndexLayout, schema: &Schema) -> SummaResult<()> {
        let index_config_holder = ConfigHolder::new(index_config.clone(), index_layout.config_filepath())?;
        index_config_holder.save()?;
        let settings = IndexSettings {
            docstore_compression: index_config.compression,
            sort_by_field: index_config.sort_by_field,
        };
        Index::builder().schema(schema.clone()).settings(settings).create_in_dir(index_layout.data_path())?;
        Ok(())
    }

    #[instrument]
    pub(crate) fn open(index_name: &str, index_layout: &IndexLayout, consumers: Vec<KafkaConsumer>) -> SummaResult<IndexHolder> {
        let index_config: IndexConfigHolder = ConfigHolder::from_file(index_layout.config_filepath(), None, true)?;

        let index = Index::open_in_dir(index_layout.data_path())?;
        let schema = index.schema();
        for (tokenizer_name, tokenizer) in &default_tokenizers() {
            index.tokenizers().register(tokenizer_name, tokenizer.clone())
        }
        let index_reader = index.reader()?;
        let index_writer_holder = IndexWriterHolder::new(index_name, &index, &index_config)?;

        let index_updater = IndexUpdater::new(index_writer_holder);
        index_updater.add_consumers(consumers)?;
        let index_updater = OwningHandler::new(index_updater);

        let autocommit_interval_ms = index_config.autocommit_interval_ms;

        let autocommit_thread = match autocommit_interval_ms {
            Some(interval_ms) => {
                let index_updater = index_updater.handler().clone();
                let (shutdown_trigger, shutdown_tripwire) = oneshot::channel();
                Some(ThreadHandler::new(
                    tokio::spawn(async move {
                        let inner_task = async move {
                            let mut interval = time::interval(Duration::from_millis(interval_ms));
                            loop {
                                interval.tick().await;
                                index_updater.try_commit_and_log(true).await;
                            }
                        };
                        tokio::select! {
                            _ = inner_task => {}
                            _ = shutdown_tripwire => {
                                info!(action = "shutdown");
                            }
                        }
                        Ok(())
                    }),
                    shutdown_trigger,
                ))
            }
            None => None,
        };

        Ok(IndexHolder {
            index_name: String::from(index_name),
            schema,
            index_layout: index_layout.clone(),
            index,
            index_reader,
            index_config,
            index_updater,
            autocommit_thread,
        })
    }

    pub(crate) async fn stop(self) -> SummaResult<()> {
        if let Some(autocommit_thread) = self.autocommit_thread {
            autocommit_thread.stop().await?;
        }
        self.index_updater.into_inner().last_commit().await?;
        Ok(())
    }

    #[instrument(skip(self))]
    pub(crate) async fn delete(self) -> SummaResult<()> {
        let index_layout = self.index_layout.clone();
        self.stop().await?;
        index_layout.delete().await?;
        Ok(())
    }

    pub(crate) fn index_name(&self) -> &str {
        &self.index_name
    }

    pub(crate) fn schema(&self) -> &Schema {
        &self.schema
    }

    pub(crate) fn searcher(&self) -> LeasedItem<Searcher> {
        self.index_reader.searcher()
    }

    pub(crate) fn index_updater(&self) -> Handler<IndexUpdater> {
        self.index_updater.handler()
    }

    #[instrument(skip(self))]
    pub(crate) async fn search(&self, query: &str, limit: usize, offset: usize) -> SummaResult<proto::SearchResponse> {
        let schema = self.schema.clone();
        let searcher = self.index_reader.searcher();

        let index_name = self.index_name.to_string();
        let default_fields = self.index_config.default_fields.clone();

        let query_parser = QueryParser::for_index(&self.index, default_fields);
        let parsed_query = query_parser.parse_query(&query);

        let parsed_query = match parsed_query {
            // ToDo: More clever processing
            Err(tantivy::query::QueryParserError::FieldDoesNotExist(_)) => {
                return Ok(proto::SearchResponse {
                    index_name: index_name.to_string(),
                    scored_documents: vec![],
                    has_next: false,
                })
            }
            Err(e) => Err(Error::InvalidSyntaxError((e, query.to_string()))),
            Ok(r) => Ok(r),
        }?;

        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            let top_docs = searcher.search(&parsed_query, &TopDocs::with_limit(limit + 1).and_offset(offset))?;
            let right_bound = std::cmp::min(limit, top_docs.len());
            let has_next = top_docs.len() > limit;

            info!(action = "search", index_name = ?index_name, query = ?query, limit = limit, offset = offset, count = ?right_bound);
            Ok(proto::SearchResponse {
                scored_documents: top_docs[..right_bound]
                    .iter()
                    .enumerate()
                    .map(|(position, (score, doc_address))| {
                        let document = searcher.doc(*doc_address).unwrap();
                        proto::ScoredDocument {
                            document: schema.to_json(&document),
                            score: *score,
                            position: position.try_into().unwrap(),
                        }
                    })
                    .collect(),
                has_next,
                index_name: index_name.to_string(),
            })
        })
        .await?
    }
}

impl std::fmt::Debug for IndexHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "IndexHolder({:?})", self.index)
    }
}
