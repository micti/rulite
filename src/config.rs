//! Runtime configuration: the subcommand, then CLI args, falling back to env
//! vars, then the JSON config file, then defaults.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::access::Access;
use crate::cors::Origins;

const USAGE: &str = "\
Usage: rulite [serve] <db-path> [options]
       rulite user add <db-path>              [--config <file>]
       rulite user passwd <db-path> <email>   [--config <file>]
       rulite user delete <db-path> <email>
       rulite user list <db-path>
       rulite keys                            [--config <file>]
       rulite gen types <db-path>             TypeScript types for supabase-js

Options (env var in brackets):
  --bind <addr>           listen address        [RULITE_BIND]           default 127.0.0.1:3000
  --readers <n>           reader threads, 1-2   [RULITE_READERS]        default 1
  --default-limit <n>     rows when no limit    [RULITE_DEFAULT_LIMIT]  default 100
  --max-limit <n>         upper bound on limit  [RULITE_MAX_LIMIT]      default 1000
  --config <file>         JSON config: auth, table permissions, CORS and TLS [RULITE_CONFIG]
  --cors-origins <list>   origins allowed by CORS, comma separated, or *  [RULITE_CORS_ORIGINS]
  --tls-cert <pem>        serve HTTPS with this certificate chain  [RULITE_TLS_CERT]
  --tls-key <pem>         and this private key (--features tls)    [RULITE_TLS_KEY]
  -h, --help              show this help

The database path may also come from RULITE_DB. Serving needs a jwt_secret,
from the config or RULITE_JWT_SECRET (which overrides the config's).";

/// What to run.
#[derive(Debug)]
pub enum Command {
    Serve(Config),
    User {
        db_path: PathBuf,
        action: UserAction,
        auth: Auth,
    },
    /// Print the anon and service_role keys.
    Keys(Auth),
    /// Print the TypeScript types of the database.
    GenTypes(PathBuf),
}

#[derive(Debug, PartialEq)]
pub enum UserAction {
    Add,
    /// Holds the email.
    Passwd(String),
    Delete(String),
    List,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub db_path: PathBuf,
    pub bind: SocketAddr,
    pub readers: usize,
    pub default_limit: u32,
    pub max_limit: u32,
    pub auth: Auth,
    /// No CORS headers when `None`.
    pub cors: Option<Origins>,
    /// Plain HTTP when `None`.
    pub tls: Option<Tls>,
}

/// PEM files for built-in HTTPS.
#[derive(Debug, Clone, PartialEq)]
pub struct Tls {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Server settings the config file may set besides auth; paths are already
/// relative to the config file's directory.
#[derive(Debug, Default)]
struct FileOptions {
    cors_origins: Option<Origins>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
}

/// Auth settings from the config file; see AUTH.md.
#[derive(Debug, Clone, PartialEq)]
pub struct Auth {
    /// HS256 secret, at least 32 bytes. Required to serve and for `rulite keys`.
    pub jwt_secret: Option<String>,
    /// Access token lifetime in seconds.
    pub jwt_exp: u64,
    /// Public base URL; the token `iss` is `<external_url>/auth/v1`.
    pub external_url: Option<String>,
    /// Seconds a used refresh token is still accepted.
    pub refresh_token_reuse_interval: u64,
    pub password_min_length: usize,
    pub access: Access,
}

impl Default for Auth {
    fn default() -> Auth {
        Auth {
            jwt_secret: None,
            jwt_exp: 3600,
            external_url: None,
            refresh_token_reuse_interval: 10,
            password_min_length: 6,
            access: Access::default(),
        }
    }
}

impl Command {
    pub fn from_env_and_args() -> Result<Command, String> {
        Self::parse(
            std::env::args().skip(1),
            |k| std::env::var(k).ok(),
            |path| std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}")),
        )
    }

    fn parse(
        args: impl Iterator<Item = String>,
        env: impl Fn(&str) -> Option<String>,
        read: impl Fn(&str) -> Result<String, String>,
    ) -> Result<Command, String> {
        let mut args: Vec<String> = args.collect();
        let command = match args.first().map(String::as_str) {
            Some("serve") => "serve",
            Some("user") => "user",
            Some("keys") => "keys",
            Some("gen") => "gen",
            _ => {
                args.insert(0, "serve".into());
                "serve"
            }
        };
        let mut opts = Options::default();
        let mut positional = Vec::new();
        let mut rest = args.into_iter().skip(1);
        while let Some(arg) = rest.next() {
            let slot = match arg.as_str() {
                "-h" | "--help" => return Err(USAGE.to_string()),
                "--config" => &mut opts.config,
                "--bind" if command == "serve" => &mut opts.bind,
                "--readers" if command == "serve" => &mut opts.readers,
                "--default-limit" if command == "serve" => &mut opts.default_limit,
                "--max-limit" if command == "serve" => &mut opts.max_limit,
                "--cors-origins" if command == "serve" => &mut opts.cors_origins,
                "--tls-cert" if command == "serve" => &mut opts.tls_cert,
                "--tls-key" if command == "serve" => &mut opts.tls_key,
                s if s.starts_with('-') => return Err(format!("unknown option {s}\n\n{USAGE}")),
                _ => {
                    positional.push(arg);
                    continue;
                }
            };
            *slot = Some(rest.next().ok_or_else(|| format!("{arg} needs a value"))?);
        }
        let (auth, file) = load_config(opts.config.take().or_else(|| env("RULITE_CONFIG")), &env, &read)?;

        let mut positional = positional.into_iter();
        let mut next = |what: &str| positional.next().ok_or_else(|| format!("missing {what}\n\n{USAGE}"));
        let command = match command {
            "keys" if auth.jwt_secret.is_none() => {
                return Err("rulite keys needs a jwt_secret (--config or RULITE_JWT_SECRET)".into());
            }
            "keys" => Command::Keys(auth),
            "gen" => match next("gen command")?.as_str() {
                "types" => Command::GenTypes(next("database path")?.into()),
                other => return Err(format!("unknown gen command {other}\n\n{USAGE}")),
            },
            "user" => {
                let action = next("user command")?;
                let db_path = next("database path")?.into();
                let action = match action.as_str() {
                    "add" => UserAction::Add,
                    "passwd" => UserAction::Passwd(next("email")?),
                    "delete" => UserAction::Delete(next("email")?),
                    "list" => UserAction::List,
                    other => return Err(format!("unknown user command {other}\n\n{USAGE}")),
                };
                Command::User { db_path, action, auth }
            }
            _ => {
                let db_path = positional.next().or_else(|| env("RULITE_DB"));
                Command::Serve(serve(db_path, opts, &env, auth, file)?)
            }
        };
        if positional.next().is_some() {
            return Err(format!("too many arguments\n\n{USAGE}"));
        }
        Ok(command)
    }
}

#[derive(Default)]
struct Options {
    config: Option<String>,
    bind: Option<String>,
    readers: Option<String>,
    default_limit: Option<String>,
    max_limit: Option<String>,
    cors_origins: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
}

fn serve(
    db_path: Option<String>,
    opts: Options,
    env: impl Fn(&str) -> Option<String>,
    auth: Auth,
    file: FileOptions,
) -> Result<Config, String> {
    let db_path = db_path.ok_or_else(|| format!("missing database path\n\n{USAGE}"))?;
    if auth.jwt_secret.is_none() {
        return Err("rulite: a jwt_secret is required (--config or RULITE_JWT_SECRET); see AUTH.md".into());
    }
    let bind = opts.bind.or_else(|| env("RULITE_BIND"));
    let bind = bind.as_deref().unwrap_or("127.0.0.1:3000");
    let bind = bind.parse().map_err(|_| format!("invalid bind address {bind}"))?;
    let readers = num("readers", opts.readers.or_else(|| env("RULITE_READERS")), 1)?;
    if !(1..=2).contains(&readers) {
        return Err("readers must be 1 or 2".into());
    }
    let default_limit = num("default-limit", opts.default_limit.or_else(|| env("RULITE_DEFAULT_LIMIT")), 100)?;
    let max_limit = num("max-limit", opts.max_limit.or_else(|| env("RULITE_MAX_LIMIT")), 1000)?;
    if default_limit == 0 || default_limit > max_limit {
        return Err("default-limit must be between 1 and max-limit".into());
    }
    let cors = match opts.cors_origins.or_else(|| env("RULITE_CORS_ORIGINS")) {
        Some(list) => Some(Origins::parse(&list)?),
        None => file.cors_origins,
    };
    let cert = opts.tls_cert.or_else(|| env("RULITE_TLS_CERT")).map(PathBuf::from).or(file.tls_cert);
    let key = opts.tls_key.or_else(|| env("RULITE_TLS_KEY")).map(PathBuf::from).or(file.tls_key);
    let tls = match (cert, key) {
        (None, None) => None,
        _ if !cfg!(feature = "tls") => {
            return Err("rulite: built without TLS; rebuild with --features tls or use a proxy".into());
        }
        (Some(cert), Some(key)) => Some(Tls { cert, key }),
        _ => return Err("tls_cert and tls_key (--tls-cert, --tls-key) must be set together".into()),
    };
    Ok(Config { db_path: db_path.into(), bind, readers: readers as usize, default_limit, max_limit, auth, cors, tls })
}

fn num(name: &str, value: Option<String>, default: u32) -> Result<u32, String> {
    match value {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| format!("invalid {name}: {v}")),
    }
}

/// The config file (if any) validated by hand, then `RULITE_JWT_SECRET`.
fn load_config(
    path: Option<String>,
    env: impl Fn(&str) -> Option<String>,
    read: impl Fn(&str) -> Result<String, String>,
) -> Result<(Auth, FileOptions), String> {
    let mut auth = Auth::default();
    let mut options = FileOptions::default();
    if let Some(path) = path {
        let dir = Path::new(&path).parent().unwrap_or(Path::new(""));
        let file: Value = serde_json::from_str(&read(&path)?).map_err(|e| format!("{path}: {e}"))?;
        let fields = file.as_object().ok_or_else(|| format!("{path}: expected a JSON object"))?;
        for (key, value) in fields {
            let bad = || format!("{path}: invalid {key}: {value}");
            let int = |min: u64| value.as_u64().filter(|n| *n >= min).ok_or_else(bad);
            let text = || value.as_str().map(str::to_string).ok_or_else(bad);
            match key.as_str() {
                "jwt_secret" => auth.jwt_secret = Some(text()?),
                "jwt_exp" => auth.jwt_exp = int(1)?,
                "external_url" => auth.external_url = Some(text()?.trim_end_matches('/').to_string()),
                "refresh_token_reuse_interval" => auth.refresh_token_reuse_interval = int(0)?,
                "password_min_length" => auth.password_min_length = int(1)? as usize,
                "tables" => auth.access = Access::parse(value).map_err(|e| format!("{path}: {e}"))?,
                "cors_origins" => {
                    let items = match value {
                        Value::String(s) if s == "*" => vec!["*"],
                        Value::Array(items) => {
                            items.iter().map(|v| v.as_str().ok_or_else(bad)).collect::<Result<_, _>>()?
                        }
                        _ => return Err(bad()),
                    };
                    let origins = Origins::from_items(items).map_err(|e| format!("{path}: {e}"))?;
                    options.cors_origins = Some(origins);
                }
                "tls_cert" => options.tls_cert = Some(dir.join(text()?)),
                "tls_key" => options.tls_key = Some(dir.join(text()?)),
                _ => return Err(format!("{path}: unknown field {key}")),
            }
        }
    }
    if let Some(secret) = env("RULITE_JWT_SECRET").filter(|s| !s.is_empty()) {
        auth.jwt_secret = Some(secret);
    }
    if auth.jwt_secret.as_ref().is_some_and(|s| s.len() < 32) {
        return Err("jwt_secret must be at least 32 characters".into());
    }
    Ok((auth, options))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn run(args: &[&str], env: &[(&str, &str)], files: &[(&str, &str)]) -> Result<Command, String> {
        Command::parse(
            args.iter().map(|s| s.to_string()),
            |k| env.iter().find(|(name, _)| *name == k).map(|(_, v)| v.to_string()),
            |p| files.iter().find(|(name, _)| *name == p).map(|(_, v)| v.to_string()).ok_or(format!("no {p}")),
        )
    }

    /// Serve options, with `RULITE_JWT_SECRET` set unless `env` sets it.
    fn parse(args: &[&str], env: &[(&str, &str)]) -> Result<Config, String> {
        let mut env = env.to_vec();
        if !env.iter().any(|(k, _)| *k == "RULITE_JWT_SECRET") {
            env.push(("RULITE_JWT_SECRET", SECRET));
        }
        match run(args, &env, &[])? {
            Command::Serve(c) => Ok(c),
            other => panic!("not serve: {other:?}"),
        }
    }

    #[test]
    fn defaults() {
        let c = parse(&["a.db"], &[]).unwrap();
        assert_eq!(c.db_path, PathBuf::from("a.db"));
        assert_eq!(c.bind, "127.0.0.1:3000".parse().unwrap());
        assert_eq!((c.readers, c.default_limit, c.max_limit), (1, 100, 1000));
        assert_eq!(c.auth, Auth { jwt_secret: Some(SECRET.into()), ..Auth::default() });
        assert_eq!(parse(&["serve", "b.db"], &[]).unwrap().db_path, PathBuf::from("b.db"));
    }

    #[test]
    fn args_override_env() {
        let env = [("RULITE_DB", "env.db"), ("RULITE_BIND", "0.0.0.0:1"), ("RULITE_READERS", "2")];
        let c = parse(&["--bind", "127.0.0.1:8080", "arg.db"], &env).unwrap();
        assert_eq!(c.db_path, PathBuf::from("arg.db"));
        assert_eq!(c.bind, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(c.readers, 2);
    }

    #[test]
    fn rejects_bad_values() {
        assert!(parse(&[], &[]).is_err());
        assert!(parse(&["a.db", "--readers", "3"], &[]).is_err());
        assert!(parse(&["a.db", "--default-limit", "5000"], &[]).is_err());
        assert!(parse(&["a.db", "--bind"], &[]).is_err());
        assert!(parse(&["a.db", "--nope"], &[]).is_err());
        assert!(parse(&["a.db", "b.db"], &[]).is_err());
    }

    #[test]
    fn config_file_and_precedence() {
        let file = format!(
            r#"{{"jwt_secret":"{SECRET}","jwt_exp":60,"external_url":"https://x.dev/","password_min_length":8,
                "refresh_token_reuse_interval":0,"tables":{{"t":{{"anon":"read"}}}}}}"#
        );
        let files = [("c.json", file.as_str()), ("env.json", r#"{"jwt_exp": 5}"#)];
        let Command::Serve(c) = run(&["a.db", "--config", "c.json"], &[("RULITE_CONFIG", "env.json")], &files).unwrap()
        else {
            panic!()
        };
        let a = c.auth;
        assert_eq!(
            (a.jwt_secret.as_deref(), a.jwt_exp, a.external_url.as_deref()),
            (Some(SECRET), 60, Some("https://x.dev"))
        );
        assert_eq!((a.password_min_length, a.refresh_token_reuse_interval), (8, 0));
        assert_eq!(a.access.anon.rule("t").perm, crate::sql::Perm::Read);

        // RULITE_CONFIG when --config is absent; RULITE_JWT_SECRET over the file.
        let other = "x".repeat(40);
        let env = [("RULITE_CONFIG", "c.json"), ("RULITE_JWT_SECRET", other.as_str())];
        let Command::Serve(c) = run(&["a.db"], &env, &files).unwrap() else { panic!() };
        assert_eq!((c.auth.jwt_secret.as_deref(), c.auth.jwt_exp), (Some(other.as_str()), 60));
        let Command::Keys(a) = run(&["keys"], &env, &files).unwrap() else { panic!() };
        assert_eq!(a.jwt_secret.as_deref(), Some(other.as_str()));
        let Command::User { auth, .. } =
            run(&["user", "list", "a.db"], &[("RULITE_CONFIG", "env.json")], &files).unwrap()
        else {
            panic!()
        };
        assert_eq!((auth.jwt_secret, auth.jwt_exp), (None, 5));
    }

    #[test]
    fn serve_needs_a_secret() {
        let e = run(&["a.db"], &[], &[]).unwrap_err();
        assert_eq!(e, "rulite: a jwt_secret is required (--config or RULITE_JWT_SECRET); see AUTH.md");
        assert!(run(&["a.db"], &[("RULITE_CONFIG", "c.json")], &[("c.json", r#"{"jwt_exp": 5}"#)]).is_err());
        assert!(run(&["a.db"], &[("RULITE_JWT_SECRET", "")], &[]).is_err());
        assert!(parse(&["a.db"], &[]).is_ok());
    }

    #[test]
    fn config_refusals() {
        for bad in [
            r#"{"jwt_secret":"short"}"#,
            r#"{"jwt_exp":0}"#,
            r#"{"jwt_exp":"1"}"#,
            r#"{"secret":"x"}"#,
            r#"{"tables":{"t":{"anon":"all"}}}"#,
            "[]",
            "{",
        ] {
            assert!(run(&["a.db", "--config", "c.json"], &[], &[("c.json", bad)]).is_err(), "{bad}");
        }
        assert!(run(&["a.db", "--config", "missing.json"], &[], &[]).is_err());
        assert!(run(&["a.db"], &[("RULITE_JWT_SECRET", "short")], &[]).is_err());
    }

    #[test]
    fn cors_origins() {
        assert_eq!(parse(&["a.db"], &[]).unwrap().cors, None);
        let list = |v: &[&str]| Some(Origins::List(v.iter().map(|s| s.to_string()).collect()));
        let c = parse(&["a.db", "--cors-origins", "http://localhost:5173,https://app.example.com"], &[]).unwrap();
        assert_eq!(c.cors, list(&["http://localhost:5173", "https://app.example.com"]));
        assert_eq!(parse(&["a.db"], &[("RULITE_CORS_ORIGINS", "*")]).unwrap().cors, Some(Origins::Any));
        let e = parse(&["a.db", "--cors-origins", "localhost:5173"], &[]).unwrap_err();
        assert!(e.starts_with("invalid cors origin"), "{e}");

        // flag > env > file
        let files = [
            ("c.json", format!(r#"{{"jwt_secret":"{SECRET}","cors_origins":["https://file.dev"]}}"#)),
            ("star.json", format!(r#"{{"jwt_secret":"{SECRET}","cors_origins":"*"}}"#)),
        ];
        let files: Vec<(&str, &str)> = files.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let serve = |args: &[&str], env: &[(&str, &str)]| match run(args, env, &files) {
            Ok(Command::Serve(c)) => c.cors,
            other => panic!("{other:?}"),
        };
        assert_eq!(serve(&["a.db", "--config", "c.json"], &[]), list(&["https://file.dev"]));
        assert_eq!(serve(&["a.db", "--config", "star.json"], &[]), Some(Origins::Any));
        let env = [("RULITE_CORS_ORIGINS", "https://env.dev")];
        assert_eq!(serve(&["a.db", "--config", "c.json"], &env), list(&["https://env.dev"]));
        assert_eq!(
            serve(&["a.db", "--config", "c.json", "--cors-origins", "https://flag.dev"], &env),
            list(&["https://flag.dev"])
        );

        for bad in [r#""https://a.dev""#, "[1]", "true", r#"["https://a.dev/"]"#, r#"["*", "https://a.dev"]"#] {
            let file = format!(r#"{{"jwt_secret":"{SECRET}","cors_origins":{bad}}}"#);
            assert!(run(&["a.db", "--config", "c.json"], &[], &[("c.json", &file)]).is_err(), "{bad}");
        }
    }

    #[test]
    fn tls_options() {
        let file = format!(r#"{{"jwt_secret":"{SECRET}","tls_cert":"cert.pem","tls_key":"/abs/key.pem"}}"#);
        let files = [("conf/app.json", file.as_str()), ("half.json", r#"{"tls_cert":"cert.pem"}"#)];
        let serve = |args: &[&str], env: &[(&str, &str)]| match run(args, env, &files) {
            Ok(Command::Serve(c)) => Ok(c.tls),
            Ok(other) => panic!("{other:?}"),
            Err(e) => Err(e),
        };
        let tls = |cert: &str, key: &str| Ok(Some(Tls { cert: cert.into(), key: key.into() }));
        let without = Err("rulite: built without TLS; rebuild with --features tls or use a proxy".to_string());
        let pair = Err("tls_cert and tls_key (--tls-cert, --tls-key) must be set together".to_string());
        let secret = [("RULITE_JWT_SECRET", SECRET)];

        assert_eq!(serve(&["a.db"], &secret), Ok(None));
        let flags = serve(&["a.db", "--tls-cert", "c.pem", "--tls-key", "k.pem"], &secret);
        let env = [("RULITE_JWT_SECRET", SECRET), ("RULITE_TLS_CERT", "env.pem"), ("RULITE_TLS_KEY", "envkey.pem")];
        // Paths in the config file are relative to its directory.
        let from_file = serve(&["a.db", "--config", "conf/app.json"], &[]);
        let flag_over_env = serve(&["a.db", "--tls-cert", "c.pem"], &env);
        let env_over_file = serve(&["a.db", "--config", "conf/app.json"], &env[1..2]);
        let half = serve(&["a.db", "--tls-cert", "c.pem"], &secret);
        let half_file = serve(&["a.db", "--config", "half.json"], &secret);
        if cfg!(feature = "tls") {
            assert_eq!(flags, tls("c.pem", "k.pem"));
            assert_eq!(from_file, tls("conf/cert.pem", "/abs/key.pem"));
            assert_eq!(flag_over_env, tls("c.pem", "envkey.pem"));
            assert_eq!(env_over_file, tls("env.pem", "/abs/key.pem"));
            assert_eq!((half, half_file), (pair.clone(), pair));
        } else {
            for r in [flags, from_file, flag_over_env, env_over_file, half, half_file] {
                assert_eq!(r, without);
            }
        }
        assert!(run(&["a.db", "--config", "c.json"], &[], &[("c.json", r#"{"tls_cert":1}"#)]).is_err());
        assert!(run(&["keys", "--tls-cert", "c.pem"], &secret, &[]).is_err());
    }

    #[test]
    fn subcommands() {
        let user = |args: &[&str]| match run(args, &[], &[]) {
            Ok(Command::User { db_path, action, .. }) => Ok((db_path, action)),
            Ok(other) => panic!("{other:?}"),
            Err(e) => Err(e),
        };
        assert_eq!(user(&["user", "add", "a.db"]).unwrap(), (PathBuf::from("a.db"), UserAction::Add));
        assert_eq!(user(&["user", "passwd", "a.db", "x@y.z"]).unwrap().1, UserAction::Passwd("x@y.z".into()));
        assert_eq!(user(&["user", "delete", "a.db", "x@y.z"]).unwrap().1, UserAction::Delete("x@y.z".into()));
        assert_eq!(user(&["user", "list", "a.db"]).unwrap().1, UserAction::List);
        assert!(user(&["user", "passwd", "a.db"]).is_err());
        assert!(user(&["user", "nope", "a.db"]).is_err());
        assert!(user(&["user", "add", "a.db", "--bind", "x"]).is_err());

        assert!(matches!(run(&["gen", "types", "a.db"], &[], &[]), Ok(Command::GenTypes(p)) if p == Path::new("a.db")));
        assert!(run(&["gen", "types"], &[], &[]).is_err());
        assert!(run(&["gen", "nope", "a.db"], &[], &[]).is_err());
        assert!(run(&["gen", "types", "a.db", "b.db"], &[], &[]).is_err());
        assert!(run(&["gen", "types", "a.db", "--bind", "x"], &[], &[]).is_err());

        assert!(run(&["keys"], &[], &[]).is_err());
        assert!(matches!(run(&["keys"], &[("RULITE_JWT_SECRET", SECRET)], &[]), Ok(Command::Keys(_))));
    }
}
