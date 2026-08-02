use clap::Parser;
use partial_compact_codex::model_context::{run_model_turn, stored_session_cwd, ModelTurnConfig};
use partial_compact_codex::storage::{Error, Result, Store};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "pcodx-model-turn")]
#[command(about = "Run one real Codex model turn from a PCODX compacted context.")]
struct Cli {
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
    #[arg(long, value_name = "SESSION")]
    session: String,
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    #[arg(long, value_name = "BIN", default_value = "codex")]
    codex_bin: String,
    #[arg(long, value_name = "SECONDS", default_value_t = 180)]
    timeout_seconds: u64,
    #[arg(long, conflicts_with = "text_file")]
    text: Option<String>,
    #[arg(long, value_name = "PATH", conflicts_with = "text")]
    text_file: Option<PathBuf>,
    #[arg(long)]
    json: bool,
    #[arg(long, value_name = "PATH")]
    context_out: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    if cli.context_out.as_ref().is_some_and(|path| path.exists()) {
        return Err(Error::Invalid(
            "--context-out path already exists; choose a new audit path".to_owned(),
        ));
    }
    let prompt = read_prompt(cli.text, cli.text_file)?;
    let db_path = cli.db.unwrap_or_else(Store::default_path);
    let cwd = resolve_cwd(&db_path, &cli.session, cli.cwd)?;
    let config = ModelTurnConfig {
        codex_bin: cli.codex_bin,
        cwd,
        db_path,
        prompt,
        session_id: cli.session,
        timeout: Duration::from_secs(cli.timeout_seconds),
    };
    let result = run_model_turn(&config)?;
    let context_path = if let Some(path) = cli.context_out {
        write_new_file(&path, result.rendered_model_context.as_bytes())?;
        Some(path)
    } else {
        None
    };
    if cli.json {
        let mut value = serde_json::to_value(&result)
            .map_err(|error| Error::Invalid(format!("failed to encode result JSON: {error}")))?;
        if let (Some(object), Some(path)) = (value.as_object_mut(), context_path.as_ref()) {
            object.insert("rendered_model_context_path".to_owned(), json!(path));
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(|error| Error::Invalid(format!(
                "failed to encode result JSON: {error}"
            )))?
        );
    } else {
        println!("{}", result.assistant);
        eprintln!("upstream_thread_id={}", result.upstream_thread_id);
        eprintln!("upstream_turn_id={}", result.upstream_turn_id);
        eprintln!("model_input_tokens={}", result.token_usage.input_tokens);
        eprintln!("injected_context_chars={}", result.injected_context_chars);
        eprintln!("context_strategy={}", result.context_strategy);
        eprintln!("kv_cache_status={}", result.kv_cache_status);
        if let Some(path) = context_path {
            eprintln!("rendered_model_context_path={}", path.display());
        }
    }
    Ok(())
}

fn resolve_cwd(
    db_path: &std::path::Path,
    session: &str,
    explicit: Option<PathBuf>,
) -> Result<PathBuf> {
    match explicit {
        Some(cwd) => Ok(cwd),
        None => stored_session_cwd(db_path, session),
    }
}

fn read_prompt(text: Option<String>, text_file: Option<PathBuf>) -> Result<String> {
    let prompt = match (text, text_file) {
        (Some(text), None) => text,
        (None, Some(path)) if path.as_os_str() == "-" => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text
        }
        (None, Some(path)) => std::fs::read_to_string(path)?,
        (None, None) => {
            return Err(Error::Invalid(
                "pass exactly one of --text or --text-file".to_owned(),
            ))
        }
        (Some(_), Some(_)) => unreachable!("clap rejects conflicting prompt inputs"),
    };
    if prompt.trim().is_empty() {
        return Err(Error::Invalid(
            "model-turn prompt must be non-empty".to_owned(),
        ));
    }
    Ok(prompt)
}

fn write_new_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_cwd;
    use partial_compact_codex::storage::Store;
    use tempfile::tempdir;

    #[test]
    fn model_turn_defaults_to_the_session_working_directory() {
        let temp = tempdir().unwrap();
        let stored_cwd = temp.path().join("stored-cwd");
        let explicit_cwd = temp.path().join("explicit-cwd");
        std::fs::create_dir(&stored_cwd).unwrap();
        std::fs::create_dir(&explicit_cwd).unwrap();
        let db_path = temp.path().join("pcodx.sqlite3");
        let mut store = Store::open(&db_path).unwrap();
        store
            .create_session(Some("cwd-session"), &stored_cwd)
            .unwrap();
        drop(store);

        assert_eq!(
            resolve_cwd(&db_path, "cwd-session", None).unwrap(),
            stored_cwd
        );
        assert_eq!(
            resolve_cwd(&db_path, "cwd-session", Some(explicit_cwd.clone())).unwrap(),
            explicit_cwd
        );
    }
}
