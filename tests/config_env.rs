//! Integration test: loading config from a real on-disk `.env` (the binary
//! boundary the inline unit tests cannot cover). Exercises `load_env_file` +
//! `resolve` against a temp directory.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use xiuxiu::config::{load_env_file, resolve, Backend};

fn temp_dir(name: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("xiuxiu-test-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn loads_config_from_a_real_env_file() {
    let dir = temp_dir("loadcfg");
    let env_path = dir.join(".env");
    fs::write(&env_path, "BACKEND=groq\nGROQ_API_KEY=gsk_integration\n").expect("write .env");

    let exedir = load_env_file(&env_path);
    let cfg = resolve(&HashMap::new(), &HashMap::new(), &exedir).expect("resolve");

    assert_eq!(cfg.backend, Backend::Groq);
    assert_eq!(
        cfg.groq_api_key.expect("key present").expose(),
        "gsk_integration"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_env_file_yields_empty_map() {
    let dir = temp_dir("missing");
    let absent = dir.join("does-not-exist.env");
    assert!(load_env_file(&absent).is_empty());
    fs::remove_dir_all(&dir).ok();
}
