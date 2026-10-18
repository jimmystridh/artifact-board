use artifact_board::{auth::Config, cloud_store::CloudStore, server};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dev = std::env::args().any(|s| s == "--dev-fixtures");
    let storage = std::env::var("ARTIFACT_STORE").unwrap_or_else(|_| "local-reference".into());
    let cloud_run = std::env::var("K_SERVICE").is_ok() || std::env::var("CLOUD_RUN_JOB").is_ok();
    let bind = std::env::var("ARTIFACT_BIND").unwrap_or_else(|_| {
        if cloud_run {
            format!(
                "0.0.0.0:{}",
                std::env::var("PORT").unwrap_or_else(|_| "8080".into())
            )
        } else {
            "127.0.0.1:4317".into()
        }
    });
    let address: std::net::SocketAddr = bind.parse()?;
    if dev && (!address.ip().is_loopback() || cloud_run) {
        return Err("Synthetic development identities require local loopback and are prohibited on Cloud Run.".into());
    }
    if storage == "local-reference" && (!address.ip().is_loopback() || cloud_run) {
        return Err(
            "Local reference SQLite cannot run in production or bind externally. Select firestore."
                .into(),
        );
    }
    let origin = std::env::var("ARTIFACT_ORIGIN").unwrap_or_else(|_| format!("http://{bind}"));
    if cloud_run && !origin.starts_with("https://") {
        return Err("Cloud Run requires explicit HTTPS ARTIFACT_ORIGIN.".into());
    }
    let config = Config::from_env(dev, origin);
    let app = match storage.as_str() {
        "local-reference" => {
            let dir = std::path::PathBuf::from(
                std::env::var("ARTIFACT_DATA_DIR").unwrap_or_else(|_| ".local".into()),
            );
            std::fs::create_dir_all(&dir)?;
            server::app(
                &dir.join("local-fixture.sqlite"),
                dir.join("staging"),
                config,
            )?
        }
        "firestore" => {
            let project = std::env::var("GOOGLE_CLOUD_PROJECT")?;
            let bucket = std::env::var("ARTIFACT_BUCKET")?;
            let database =
                std::env::var("ARTIFACT_DATABASE").unwrap_or_else(|_| "(default)".into());
            let store = if let Ok(host) = std::env::var("ARTIFACT_FIRESTORE_EMULATOR") {
                if !dev {
                    return Err(
                        "Emulator endpoints require explicit local development fixtures.".into(),
                    );
                }
                CloudStore::emulator(
                    &project,
                    &database,
                    &bucket,
                    &host,
                    &std::env::var("ARTIFACT_STORAGE_EMULATOR")?,
                )?
            } else {
                CloudStore::new(&project, &database, &bucket)?
            };
            server::cloud_app(store, config)
        }
        _ => return Err("ARTIFACT_STORE must be local-reference or firestore".into()),
    };
    if let Ok(job_id) = std::env::var("ARTIFACT_BUILD_JOB_ID") {
        let artifact_board::store::Store::Firestore(core) = &app.core else {
            return Err("Build worker requires Firestore persistence".into());
        };
        let result = artifact_board::build_jobs::run_worker(
            core,
            &job_id,
            &std::env::var("ARTIFACT_BUILD_MODE").unwrap_or_else(|_| "container".into()),
        )
        .await?;
        println!(
            "Build {}: {}",
            job_id,
            result["state"].as_str().unwrap_or("unknown")
        );
        if matches!(result["state"].as_str(), Some("failed" | "timed_out")) {
            return Err(
                "Build did not succeed; owner-visible diagnostics are in durable job status."
                    .into(),
            );
        }
        return Ok(());
    }
    if std::env::var("ARTIFACT_NOTIFICATION_WORKER").as_deref() == Ok("1") {
        let artifact_board::store::Store::Firestore(core) = &app.core else {
            return Err("Notification worker requires Firestore persistence".into());
        };
        let enabled = std::env::var("ARTIFACT_SLACK_DELIVERY_ENABLED").as_deref() == Ok("true");
        let adapter = artifact_board::notifications::SlackAdapter::new(
            std::env::var("ARTIFACT_SLACK_TOKEN").unwrap_or_default(),
            std::env::var("ARTIFACT_SLACK_WORKSPACE").unwrap_or_default(),
            enabled,
        )?;
        let report = adapter
            .notify_once(core, artifact_board::auth::now() as i64)
            .await?;
        println!(
            "Notification worker completed; processed batches: {}",
            report.as_array().map(|a| a.len()).unwrap_or(0)
        );
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    eprintln!("Artifact Board at {bind}; development fixtures={dev}; storage={storage}");
    axum::serve(listener, server::router(app))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
