use super::OpError;
use super::OpResult;
use crate::ctx::Ctx;
use crate::incomplete_token::IncompleteToken;
use crate::object::calc_object_layout;
use crate::object::Object;
use crate::object::ObjectMetadata;
use crate::object::ObjectState;
use crate::object::ObjectTuple;
use crate::objects::ObjectId;
use crate::op::key_debug_str;
use crate::util::ceil_pow2;
use chrono::Utc;
use off64::u64;
use off64::u8;
use off64::Off64WriteMut;
use rand::thread_rng;
use rand::Rng;
use serde::Serialize;
use std::cmp::max;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing::trace;

struct Reservation<'a> {
  ctx: &'a Ctx,
  pages: Vec<(u64, u8)>,
}

impl Reservation<'_> {
  fn allocate(&mut self, size: u64) -> OpResult<u64> {
    let pow2 = max(
      self.ctx.pages.spage_size_pow2,
      u8!(size.next_power_of_two().ilog2()),
    );
    let offset = self
      .ctx
      .heap_allocator
      .lock()
      .allocate(size)
      .map_err(|_| OpError::OutOfSpace)?;
    self.pages.push((offset, pow2));
    Ok(offset)
  }

  fn commit(&mut self) {
    self.pages.clear();
  }
}

impl Drop for Reservation<'_> {
  fn drop(&mut self) {
    let mut allocator = self.ctx.heap_allocator.lock();
    for (offset, pow2) in self.pages.drain(..) {
      allocator.release(offset, pow2);
    }
  }
}

pub struct OpCreateObjectInput {
  pub key: Vec<u8>,
  pub size: u64,
}

pub struct OpCreateObjectOutput {
  pub token: IncompleteToken,
}

pub(crate) async fn op_create_object(
  ctx: Arc<Ctx>,
  req: OpCreateObjectInput,
) -> OpResult<OpCreateObjectOutput> {
  tokio::spawn(async move { create_object_inner(ctx, req).await })
    .await
    .expect("create task failed")
}

async fn evict_one(ctx: &Ctx) -> bool {
  for _ in 0..ctx.clock.len().saturating_mul(2) {
    let Some(candidate) = ctx.clock.candidate() else {
      return false;
    };
    let removed = ctx
      .committed_objects
      .remove_if(&candidate.key, |_, current| {
        current.same_object(&candidate) && !current.is_pinned() && !current.take_reference()
      });
    if let Some((_, victim)) = removed {
      ctx.clock.remove(&victim);
      super::delete_object::reap_object(ctx, &victim).await;
      ctx
        .metrics
        .0
        .committed_object_count
        .fetch_sub(1, Ordering::Relaxed);
      ctx
        .metrics
        .0
        .evicted_object_count
        .fetch_add(1, Ordering::Relaxed);
      ctx
        .metrics
        .0
        .evicted_bytes
        .fetch_add(victim.size, Ordering::Relaxed);
      return true;
    }
  }
  false
}

async fn create_object_inner(
  ctx: Arc<Ctx>,
  req: OpCreateObjectInput,
) -> OpResult<OpCreateObjectOutput> {
  let _lifecycle = ctx.lifecycle.lock().await;
  trace!(
    key = key_debug_str(&req.key),
    size = req.size,
    "creating object"
  );

  if req.size > ctx.heap_size.saturating_sub(ctx.pages.spage_size())
    || req.size / ctx.pages.lpage_size() > u32::MAX as u64
  {
    return Err(OpError::OutOfSpace);
  }
  for _ in 0..=ctx.clock.len() {
    match try_create(&ctx, &req).await {
      Ok(output) => return Ok(output),
      Err(OpError::OutOfSpace) if ctx.cache_eviction && evict_one(&ctx).await => {}
      Err(err) => return Err(err),
    }
  }
  Err(OpError::OutOfSpace)
}

async fn try_create(ctx: &Ctx, req: &OpCreateObjectInput) -> OpResult<OpCreateObjectOutput> {
  let layout = calc_object_layout(&ctx.pages, req.size);
  let mut reservation = Reservation {
    ctx,
    pages: Vec::new(),
  };

  let mut lpage_dev_offsets = Vec::new();
  let mut tail_page_dev_offsets = Vec::new();
  for _ in 0..layout.lpage_count {
    lpage_dev_offsets.push(reservation.allocate(ctx.pages.lpage_size())?);
  }
  for (_, tail_page_size_pow2) in layout.tail_page_sizes_pow2 {
    tail_page_dev_offsets.push(reservation.allocate(1 << tail_page_size_pow2)?);
  }

  let metadata = ObjectMetadata {
    created: Utc::now(),
    key: req.key.clone(),
    size: req.size,
    lpage_dev_offsets,
    tail_page_dev_offsets,
  };

  let mut metadata_raw = Vec::new();
  metadata
    .serialize(&mut rmp_serde::Serializer::new(&mut metadata_raw))
    .unwrap();
  let metadata_size = u64!(metadata_raw.len());
  if metadata_size > ctx.pages.lpage_size() {
    return Err(OpError::ObjectMetadataTooLarge);
  }
  let metadata_page_size = max(
    ctx.pages.spage_size(),
    ceil_pow2(metadata_size, ctx.pages.spage_size_pow2),
  );
  let metadata_dev_offset = reservation.allocate(metadata_size)?;

  let object_id: ObjectId = thread_rng().gen();
  let tuple = ObjectTuple {
    id: object_id,
    state: ObjectState::Incomplete,
    metadata_dev_offset,
    metadata_page_size_pow2: u8!(metadata_page_size.ilog2()),
  };

  // NOTE: This is not the same as `allocate_from_data` as `metadata_page_size` may be much larger than the actual `metadata_size`.
  let mut write_page = ctx.pages.allocate_uninitialised(metadata_page_size);
  write_page.write_at(0, &metadata_raw);

  ctx.device.write_at(metadata_dev_offset, write_page).await;

  ctx
    .tuples
    .insert_object(tuple)
    .await
    .map_err(|_| OpError::OutOfSpace)?;
  reservation.commit();

  // Out of abundance of caution, insert AFTER tuple is certain to have persisted.
  let None = ctx.incomplete_objects.write().insert(
    object_id,
    Object::new(object_id, ObjectState::Incomplete, metadata, metadata_size),
  ) else {
    unreachable!();
  };

  ctx
    .metrics
    .0
    .create_op_count
    .fetch_add(1, Ordering::Relaxed);
  ctx
    .metrics
    .0
    .incomplete_object_count
    .fetch_add(1, Ordering::Relaxed);
  ctx
    .metrics
    .0
    .object_metadata_bytes
    .fetch_add(metadata_size, Ordering::Relaxed);
  ctx
    .metrics
    .0
    .object_data_bytes
    .fetch_add(req.size, Ordering::Relaxed);

  trace!(
    id = object_id,
    size = req.size,
    metadata_dev_offset = metadata_dev_offset,
    metadata_page_size = metadata_page_size,
    "created object"
  );

  Ok(OpCreateObjectOutput {
    token: IncompleteToken {
      partition_idx: ctx.partition_idx,
      object_id,
    },
  })
}
