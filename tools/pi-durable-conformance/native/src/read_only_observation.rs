//! Read-only harness observation oracle scenario.
//! Called by `recovery_traces` with shared SQLite and normalization helpers.
//! Checks the read-only session observes without changing persisted storage.
use super::*;
pub async fn read_only_observation() -> Result<Value, Box<dyn Error>> {
    let scratch = ScratchDatabase(scratch_sqlite_path()?);
    let path = scratch.0.as_str();
    let (models, _provider) = OracleProvider::new([message("visible", StopReason::Stop, None)]);
    let registry = Arc::new(RegistryImpl::with_builtins(builtin_tasks()));
    let storage: Arc<dyn Storage> =
        Arc::new(open_node_sqlite_storage(&path, SqliteDatabaseOptions::default()).await?);
    let harness = open_harness(
        Arc::clone(&storage),
        options(models.clone(), Arc::clone(&registry)),
        &BACKGROUND_CONTEXT,
    )
    .await?;
    let root = harness
        .root(
            &BACKGROUND_CONTEXT,
            rpi_durable::harness::RootOptions {
                agent: Some(AgentChange {
                    model: Some(Some(ModelRef {
                        provider: "faux".to_string(),
                        model_id: "faux-1".to_string(),
                    })),
                    ..AgentChange::default()
                }),
                ..Default::default()
            },
        )
        .await?;
    let submission = root
        .submit(input("observe", None), &BACKGROUND_CONTEXT)
        .await?;
    submission.wait(&BACKGROUND_CONTEXT).await?;
    let head_before = storage_head(&path).await?;
    let read_storage: Arc<dyn Storage> =
        Arc::new(open_node_sqlite_storage(&path, SqliteDatabaseOptions::default()).await?);
    let mut read_options = options(models, registry);
    read_options.read_only = true;
    let viewer = open_harness(Arc::clone(&read_storage), read_options, &BACKGROUND_CONTEXT).await?;
    let read_root = viewer.root(&BACKGROUND_CONTEXT, Default::default()).await?;
    let before_entries = entries(read_root.as_ref()).await?;
    let refusal = match read_root
        .submit(input("must not write", None), &BACKGROUND_CONTEXT)
        .await
    {
        Ok(_) => json!({ "accepted": true }),
        Err(error) => {
            let name = if matches!(&error, SessionError::ReadOnlySession(_)) {
                "ReadOnlySession"
            } else {
                "SessionError"
            };
            json!({ "accepted": false, "name": name, "message": error.to_string() })
        }
    };
    let head_after = storage_head(&path).await?;
    let live = viewer
        .session()
        .snapshot(
            &LIVE_DOC,
            DocOwner::Conversation(read_root.id()),
            &BACKGROUND_CONTEXT,
        )
        .await?
        .unwrap_or_default();
    let wake = viewer.wake()?;
    let trace = json!({
        "name": "read-only-observation",
        "headBeforeObservation": head_before,
        "headAfterObservation": head_after,
        "entries": serde_json::to_value(before_entries)?,
        "live": Value::Object(live),
        "wake": wake_json(wake),
        "refusal": refusal,
    });
    if refusal.get("accepted").and_then(Value::as_bool) != Some(false) {
        return Err("read-only harness accepted a mutation".into());
    }
    if wake.active {
        return Err("read-only harness scheduled tasks".into());
    }
    if head_before != head_after {
        return Err("read-only observation changed SQLite storage".into());
    }
    viewer.close(&BACKGROUND_CONTEXT).await?;
    read_storage.close(&BACKGROUND_CONTEXT).await?;
    harness.close(&BACKGROUND_CONTEXT).await?;
    storage.close(&BACKGROUND_CONTEXT).await?;
    drop(scratch);
    Ok(trace)
}

async fn storage_head(path: &str) -> Result<u64, Box<dyn Error>> {
    let database = open_node_sqlite_database(path, SqliteDatabaseOptions::default()).await?;
    let head = database
        .get_i64(
            "SELECT next_seq FROM durable_metadata WHERE singleton = 1",
            Vec::new(),
        )
        .await?
        .ok_or("durable metadata row is missing")?;
    database.close().await?;
    Ok(head as u64)
}

fn wake_json(wake: rpi_durable::harness::Wake) -> Value {
    let mut value = serde_json::Map::new();
    value.insert("active".to_string(), Value::Bool(wake.active));
    if let Some(at) = wake.at {
        value.insert("at".to_string(), json!(at));
    }
    Value::Object(value)
}
