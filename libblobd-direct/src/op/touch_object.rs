use super::OpError;
use super::OpResult;
use crate::ctx::Ctx;
use std::sync::Arc;

pub struct OpTouchObjectInput {
  pub key: Vec<u8>,
  pub id: Option<u128>,
}

pub(crate) async fn op_touch_object(ctx: Arc<Ctx>, req: OpTouchObjectInput) -> OpResult<()> {
  let Some(object) = ctx
    .committed_objects
    .get(&req.key)
    .filter(|object| req.id.is_none() || Some(object.id()) == req.id)
  else {
    return Err(OpError::ObjectNotFound);
  };
  object.touch();
  Ok(())
}
