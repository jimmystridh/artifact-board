#[path = "../src/ingest.rs"]
mod ingest;

#[test]
#[ignore = "Explicit trusted-host npm install/build smoke test; network and arbitrary source execution"]
fn own_react_fixture_builds_on_trusted_host() {
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "package.json".into(),
        include_bytes!("fixtures/react/package.json").to_vec(),
    );
    files.insert(
        "index.html".into(),
        include_bytes!("fixtures/react/index.html").to_vec(),
    );
    files.insert(
        "src/main.jsx".into(),
        include_bytes!("fixtures/react/src/main.jsx").to_vec(),
    );
    let result = ingest::prepare_with_mode("react", files, "", "trusted-host")
        .expect("own React source fixture must build");
    assert_eq!(result.entrypoint, "index.html");
    assert!(
        result
            .files
            .keys()
            .any(|p| p.starts_with("assets/") && p.ends_with(".js"))
    );
    assert!(result.warnings.iter().any(|s| s.contains("TRUSTED-HOST")));
    if let Ok(destination) = std::env::var("ARTIFACT_TEST_EXPORT_DIR") {
        let root = std::path::Path::new(&destination);
        for (name, bytes) in &result.files {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
    }
    println!(
        "Real React fixture built: {} output files; {} bytes",
        result.files.len(),
        result.files.values().map(Vec::len).sum::<usize>()
    );
}
