use crate::allocator::Allocator;
use crate::backing_store::PartitionStore;
use crate::clock::Clock;
use crate::metrics::BlobdMetrics;
use crate::objects::CommittedObjects;
use crate::objects::IncompleteObjects;
use crate::pages::Pages;
use crate::tuples::Tuples;
use parking_lot::Mutex;

pub(crate) struct Ctx {
  pub clock: Clock,
  pub cache_eviction: bool,
  pub committed_objects: CommittedObjects,
  pub device: PartitionStore,
  pub heap_allocator: Mutex<Allocator>,
  pub heap_size: u64,
  pub incomplete_objects: IncompleteObjects,
  pub metrics: BlobdMetrics,
  pub pages: Pages,
  pub partition_idx: usize,
  pub tuples: Tuples,
  pub lifecycle: std::sync::Arc<tokio::sync::Mutex<()>>,
}
