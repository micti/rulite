mod access;
mod auth;
mod cli;
mod config;
mod cors;
mod db;
mod jwt;
mod openapi;
mod query;
mod rpc;
mod schema;
mod server;
mod sql;
#[cfg(feature = "tls")]
mod tls;
mod typescript;
mod write;

use std::process::ExitCode;

use config::{Command, Config};

fn main() -> ExitCode {
    let command = match Command::from_env_and_args() {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    let result = match command {
        Command::Serve(config) => return serve(&config),
        Command::User { db_path, action, auth } => cli::user(&db_path, &action, &auth),
        Command::Keys(auth) => cli::keys(&auth),
        Command::GenTypes { db_path, functions } => cli::gen_types(&db_path, functions.as_deref()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rulite: {e}");
            ExitCode::FAILURE
        }
    }
}

fn serve(config: &Config) -> ExitCode {
    let conn = rusqlite::Connection::open_with_flags(&config.db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE);
    let conn = match conn.and_then(|c| auth::ensure_tables(&c).map(|_| c)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("rulite: cannot create the auth tables in {}: {e}", config.db_path.display());
            return ExitCode::FAILURE;
        }
    };
    let functions = match functions(config, &conn) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("rulite: {e}");
            return ExitCode::FAILURE;
        }
    };
    drop(conn);

    let (db, threads) = match db::open(&config.db_path, config.readers) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("rulite: cannot open {}: {e}", config.db_path.display());
            return ExitCode::FAILURE;
        }
    };

    let mut runtime = tokio::runtime::Builder::new_current_thread();
    runtime.enable_io();
    // The TLS handshake timeout.
    #[cfg(feature = "tls")]
    runtime.enable_time();
    let result = runtime.build().and_then(|rt| rt.block_on(server::run(config, db, functions)));

    // The server (and with it the last `Db` handle) is gone: DB threads drain and exit.
    threads.join();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rulite: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The config's function files, checked against the database and the `rpc` grants.
fn functions(config: &Config, conn: &rusqlite::Connection) -> Result<rpc::Functions, String> {
    let functions = match &config.functions {
        Some(dir) => rpc::check(conn, rpc::load(dir)?)?,
        None => rpc::Functions::default(),
    };
    config.auth.access.check_functions(|name| functions.get(name).is_some())?;
    Ok(functions)
}
