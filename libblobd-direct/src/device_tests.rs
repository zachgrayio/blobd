use crate::op::commit_object::OpCommitObjectInput;
use crate::op::create_object::OpCreateObjectInput;
use crate::op::delete_object::OpDeleteObjectInput;
use crate::op::inspect_object::OpInspectObjectInput;
use crate::op::read_object::OpReadObjectInput;
use crate::op::touch_object::OpTouchObjectInput;
use crate::op::write_object::OpWriteObjectInput;
use crate::op::OpError;
use crate::Blobd;
use crate::BlobdCfg;
use crate::BlobdCfgBackingStore;
use crate::BlobdCfgPartition;
use crate::BlobdLoader;
use futures::stream::iter;
use futures::StreamExt;
use std::path::PathBuf;

const LPAGE: u64 = 16384;
const PARTITION: u64 = 8 * LPAGE;

fn loader(path: &str) -> BlobdLoader {
  loader_with_len(path, PARTITION)
}

fn loader_with_len(path: &str, len: u64) -> BlobdLoader {
  BlobdLoader::new(
    vec![BlobdCfgPartition {
      path: PathBuf::from(path),
      offset: 0,
      len,
    }],
    BlobdCfg {
      backing_store: BlobdCfgBackingStore::Uring,
      cache_eviction: true,
      expire_incomplete_objects_after_secs: 3600,
      lpage_size_pow2: 14,
      object_tuples_area_reserved_space: LPAGE,
      spage_size_pow2: 9,
      uring_coop_taskrun: false,
      uring_defer_taskrun: false,
      uring_iopoll: false,
      uring_sqpoll: None,
    },
  )
}

async fn put(blobd: &Blobd, key: &[u8], byte: u8) {
  let created = blobd
    .create_object(OpCreateObjectInput {
      key: key.to_vec(),
      size: LPAGE,
    })
    .await
    .unwrap();
  blobd
    .write_object(OpWriteObjectInput {
      incomplete_token: created.token,
      offset: 0,
      data_len: LPAGE,
      data_stream: iter([Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
        vec![byte; LPAGE as usize],
      )]),
    })
    .await
    .unwrap();
  blobd
    .commit_object(OpCommitObjectInput {
      incomplete_token: created.token,
      if_not_exists: false,
    })
    .await
    .unwrap();
}

fn read(key: &[u8]) -> OpReadObjectInput {
  OpReadObjectInput {
    key: key.to_vec(),
    id: None,
    start: 0,
    end: None,
    stream_buffer_size: 512,
  }
}

#[tokio::test]
async fn clock_on_direct_device() {
  let Ok(path) = std::env::var("BLOBD_DESTRUCTIVE_TEST_DEVICE") else {
    return;
  };
  let device_loader = loader(&path);
  device_loader.format().await;
  let blobd = device_loader.load_and_start().await;

  put(&blobd, b"first", 1).await;
  put(&blobd, b"second", 2).await;
  put(&blobd, b"inspected-only", 3).await;
  let mut first_stream = blobd.read_object(read(b"first")).await.unwrap().data_stream;
  while let Some(chunk) = first_stream.next().await {
    assert!(chunk.unwrap().iter().all(|byte| *byte == 1));
  }
  blobd
    .touch_object(OpTouchObjectInput {
      key: b"second".to_vec(),
      id: None,
    })
    .await
    .unwrap();
  assert!(blobd
    .touch_object(OpTouchObjectInput {
      key: b"absent".to_vec(),
      id: None
    })
    .await
    .is_err());
  assert!(blobd
    .inspect_object(OpInspectObjectInput {
      key: b"inspected-only".to_vec(),
      id: None
    })
    .await
    .is_ok());
  for idx in 0..5 {
    put(&blobd, format!("next{idx}").as_bytes(), idx + 3).await;
  }
  assert!(blobd
    .inspect_object(OpInspectObjectInput {
      key: b"first".to_vec(),
      id: None
    })
    .await
    .is_ok());
  assert!(blobd
    .inspect_object(OpInspectObjectInput {
      key: b"second".to_vec(),
      id: None
    })
    .await
    .is_ok());
  assert!(blobd
    .inspect_object(OpInspectObjectInput {
      key: b"inspected-only".to_vec(),
      id: None
    })
    .await
    .is_err());

  let output = blobd.read_object(read(b"first")).await.unwrap();
  let mut stream = output.data_stream;
  assert!(stream
    .next()
    .await
    .unwrap()
    .unwrap()
    .iter()
    .all(|byte| *byte == 1));
  let created = blobd
    .create_object(OpCreateObjectInput {
      key: b"first".to_vec(),
      size: LPAGE,
    })
    .await
    .unwrap();
  blobd
    .write_object(OpWriteObjectInput {
      incomplete_token: created.token,
      offset: 0,
      data_len: LPAGE,
      data_stream: iter([Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
        vec![42; LPAGE as usize],
      )]),
    })
    .await
    .unwrap();
  let replacement = blobd.clone();
  let commit = tokio::spawn(async move {
    replacement
      .commit_object(OpCommitObjectInput {
        incomplete_token: created.token,
        if_not_exists: false,
      })
      .await
      .unwrap()
  });
  tokio::time::timeout(std::time::Duration::from_secs(2), commit)
    .await
    .unwrap()
    .unwrap();
  while let Some(chunk) = stream.next().await {
    assert!(chunk.unwrap().iter().all(|byte| *byte == 1));
  }
  let mut new_stream = blobd.read_object(read(b"first")).await.unwrap().data_stream;
  assert!(new_stream
    .next()
    .await
    .unwrap()
    .unwrap()
    .iter()
    .all(|byte| *byte == 42));
  drop(new_stream);
  drop(blobd);

  let reloaded = loader(&path).load_and_start().await;
  let mut stream = reloaded
    .read_object(read(b"first"))
    .await
    .unwrap()
    .data_stream;
  assert!(stream
    .next()
    .await
    .unwrap()
    .unwrap()
    .iter()
    .all(|byte| *byte == 42));
  assert!(reloaded
    .inspect_object(OpInspectObjectInput {
      key: b"second".to_vec(),
      id: None
    })
    .await
    .is_ok());
  assert!(reloaded
    .inspect_object(OpInspectObjectInput {
      key: b"inspected-only".to_vec(),
      id: None
    })
    .await
    .is_err());
  drop(stream);
  let mut old = reloaded
    .read_object(read(b"first"))
    .await
    .unwrap()
    .data_stream;
  assert!(old
    .next()
    .await
    .unwrap()
    .unwrap()
    .iter()
    .all(|byte| *byte == 42));
  let deletion = reloaded.clone();
  let deleting = tokio::spawn(async move {
    deletion
      .delete_object(OpDeleteObjectInput {
        key: b"first".to_vec(),
        id: None,
      })
      .await
      .unwrap()
  });
  tokio::time::timeout(std::time::Duration::from_secs(2), deleting)
    .await
    .unwrap()
    .unwrap();
  while let Some(chunk) = old.next().await {
    assert!(chunk.unwrap().iter().all(|byte| *byte == 42));
  }
  assert!(reloaded.read_object(read(b"first")).await.is_err());
  drop(reloaded);

  let device_loader = loader(&path);
  device_loader.format().await;
  let blobd = device_loader.load_and_start().await;
  put(&blobd, b"resident", 7).await;
  let mut pinned = blobd
    .read_object(read(b"resident"))
    .await
    .unwrap()
    .data_stream;
  assert!(pinned
    .next()
    .await
    .unwrap()
    .unwrap()
    .iter()
    .all(|byte| *byte == 7));
  let before = blobd.metrics().allocated_bytes();
  assert!(matches!(
    blobd
      .create_object(OpCreateObjectInput {
        key: b"oversized".to_vec(),
        size: 200 * LPAGE,
      })
      .await,
    Err(OpError::OutOfSpace)
  ));
  assert_eq!(blobd.metrics().allocated_bytes(), before);
  let oversized = blobd
    .create_object(OpCreateObjectInput {
      key: b"partial".to_vec(),
      size: 6 * LPAGE,
    })
    .await;
  assert!(matches!(oversized, Err(OpError::OutOfSpace)));
  assert_eq!(blobd.metrics().allocated_bytes(), before);
  let mut incomplete = Vec::new();
  for idx in 0..20 {
    match blobd
      .create_object(OpCreateObjectInput {
        key: format!("fill{idx}").into_bytes(),
        size: LPAGE,
      })
      .await
    {
      Ok(created) => incomplete.push(created.token),
      Err(OpError::OutOfSpace) => break,
      Err(err) => panic!("unexpected admission error: {err}"),
    }
  }
  assert!(!incomplete.is_empty());
  assert!(matches!(
    blobd
      .create_object(OpCreateObjectInput {
        key: b"blocked".to_vec(),
        size: LPAGE
      })
      .await,
    Err(OpError::OutOfSpace)
  ));
  drop(pinned);
  assert!(blobd
    .create_object(OpCreateObjectInput {
      key: b"unblocked".to_vec(),
      size: LPAGE
    })
    .await
    .is_ok());
  drop(blobd);

  let device_loader = loader_with_len(&path, 2 * 1024 * 1024);
  device_loader.format().await;
  let blobd = device_loader.load_and_start().await;
  for idx in 0..(LPAGE / 512 * (512 / 24)) {
    blobd
      .create_object(OpCreateObjectInput {
        key: format!("slot{idx}").into_bytes(),
        size: 0,
      })
      .await
      .unwrap();
  }
  let before = blobd.metrics().allocated_bytes();
  assert!(matches!(
    blobd
      .create_object(OpCreateObjectInput {
        key: b"full".to_vec(),
        size: 0
      })
      .await,
    Err(OpError::OutOfSpace)
  ));
  assert_eq!(blobd.metrics().allocated_bytes(), before);
  drop(blobd);
  let reloaded = loader_with_len(&path, 2 * 1024 * 1024)
    .load_and_start()
    .await;
  assert!(matches!(
    reloaded
      .create_object(OpCreateObjectInput {
        key: b"still-full".to_vec(),
        size: 0
      })
      .await,
    Err(OpError::OutOfSpace)
  ));
  drop(reloaded);

  let device_loader = loader_with_len(&path, 2 * 1024 * 1024);
  device_loader.format().await;
  let blobd = device_loader.load_and_start().await;
  for idx in 0..(LPAGE / 512 * (512 / 24)) {
    let created = blobd
      .create_object(OpCreateObjectInput {
        key: format!("committed{idx}").into_bytes(),
        size: 0,
      })
      .await
      .unwrap();
    blobd
      .commit_object(OpCommitObjectInput {
        incomplete_token: created.token,
        if_not_exists: false,
      })
      .await
      .unwrap();
  }
  let created = blobd
    .create_object(OpCreateObjectInput {
      key: b"new-slot".to_vec(),
      size: 0,
    })
    .await
    .unwrap();
  blobd
    .commit_object(OpCommitObjectInput {
      incomplete_token: created.token,
      if_not_exists: false,
    })
    .await
    .unwrap();
  assert_eq!(blobd.metrics().evicted_object_count(), 1);
  assert!(blobd
    .inspect_object(OpInspectObjectInput {
      key: b"committed0".to_vec(),
      id: None
    })
    .await
    .is_err());
  drop(blobd);
  let reloaded = loader_with_len(&path, 2 * 1024 * 1024)
    .load_and_start()
    .await;
  assert!(reloaded
    .inspect_object(OpInspectObjectInput {
      key: b"new-slot".to_vec(),
      id: None
    })
    .await
    .is_ok());
  drop(reloaded);

  let device_loader = loader(&path);
  device_loader.format().await;
  let blobd = device_loader.load_and_start().await;
  let mut workers = Vec::new();
  for worker in 0..4 {
    let blobd = blobd.clone();
    workers.push(tokio::spawn(async move {
      for index in 0..20 {
        let key = format!("worker{worker}-{index}");
        put(&blobd, key.as_bytes(), worker + 1).await;
        let _ = blobd
          .touch_object(OpTouchObjectInput {
            key: key.into_bytes(),
            id: None,
          })
          .await;
      }
    }));
  }
  for worker in workers {
    tokio::time::timeout(std::time::Duration::from_secs(20), worker)
      .await
      .unwrap()
      .unwrap();
  }
  assert!(blobd.metrics().evicted_object_count() > 0);
}
