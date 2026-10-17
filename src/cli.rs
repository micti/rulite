//! `rulite user add|passwd|delete|list`, `rulite keys` and `rulite gen types`:
//! one-shot commands that work while the server runs (`busy_timeout`).

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags};

use crate::auth;
use crate::config::{Auth, UserAction};
use crate::jwt::{self, Key};
use crate::{schema, typescript};

/// Prints the anon and service_role keys, one `role key` per line.
pub fn keys(auth: &Auth) -> Result<(), String> {
    let key = Key::new(auth.jwt_secret.as_deref().expect("checked by the config").as_bytes());
    let now = jwt::now();
    println!("anon          {}", jwt::api_key(&key, "anon", now));
    println!("service_role  {}", jwt::api_key(&key, "service_role", now));
    Ok(())
}

/// Prints the TypeScript types of the database, read from a read-only connection.
pub fn gen_types(db_path: &Path) -> Result<(), String> {
    let schema = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|c| {
            c.busy_timeout(Duration::from_secs(5))?;
            schema::introspect(&c)
        })
        .map_err(|e| format!("cannot read {}: {e}", db_path.display()))?;
    print!("{}", typescript::generate(&schema));
    Ok(())
}

pub fn user(db_path: &Path, action: &UserAction, auth: &Auth) -> Result<(), String> {
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .and_then(|c| {
            c.busy_timeout(Duration::from_secs(5))?;
            c.execute_batch("PRAGMA foreign_keys = ON;")?;
            auth::ensure_tables(&c)?;
            Ok(c)
        })
        .map_err(|e| format!("cannot open {}: {e}", db_path.display()))?;
    let sql = |e: rusqlite::Error| e.to_string();
    let mut prompt = Prompt::new();
    match action {
        UserAction::Add => {
            let email = prompt.line("Email: ")?.trim().to_lowercase();
            if !auth::valid_email(&email) {
                return Err(format!("invalid email {email}"));
            }
            let hash = auth::hash_password(&prompt.new_password(auth.password_min_length)?);
            let id = jwt::uuid();
            let added = conn.execute(
                concat!(
                    "INSERT INTO auth_users (id, email, encrypted_password, email_confirmed_at) ",
                    "VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))"
                ),
                (&id, &email, &hash),
            );
            match added {
                Err(rusqlite::Error::SqliteFailure(f, _)) if f.code == ErrorCode::ConstraintViolation => {
                    Err(format!("a user with email {email} already exists"))
                }
                r => {
                    r.map_err(sql)?;
                    println!("{id}  {email}");
                    Ok(())
                }
            }
        }
        UserAction::Passwd(email) => {
            let id = find(&conn, email)?;
            let hash = auth::hash_password(&prompt.new_password(auth.password_min_length)?);
            let tx = conn.unchecked_transaction().map_err(sql)?;
            tx.execute(
                "UPDATE auth_users SET encrypted_password = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
                (&id, &hash),
            )
            .map_err(sql)?;
            let signed_out = tx.execute("DELETE FROM auth_sessions WHERE user_id = ?1", [&id]).map_err(sql)?;
            tx.commit().map_err(sql)?;
            eprintln!("password changed, {signed_out} session(s) signed out");
            Ok(())
        }
        UserAction::Delete(email) => {
            let id = find(&conn, email)?;
            if prompt.tty {
                let answer = prompt.line(&format!("Delete {email} and their sessions? [y/N] "))?;
                if !matches!(answer.trim(), "y" | "Y" | "yes") {
                    return Err("not deleted".into());
                }
            }
            conn.execute("DELETE FROM auth_users WHERE id = ?1", [&id]).map_err(sql)?;
            eprintln!("deleted {email}");
            Ok(())
        }
        UserAction::List => {
            let mut stmt = conn
                .prepare("SELECT id, email, created_at, coalesce(last_sign_in_at, '-') FROM auth_users ORDER BY created_at, email")
                .map_err(sql)?;
            let rows: Vec<[String; 4]> = stmt
                .query_map([], |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?]))
                .and_then(|rows| rows.collect())
                .map_err(sql)?;
            let width = rows.iter().map(|r| r[1].len()).max().unwrap_or(0).max(5);
            println!("{:<36}  {:<width$}  {:<24}  last_sign_in_at", "id", "email", "created_at");
            for [id, email, created, last] in rows {
                println!("{id:<36}  {email:<width$}  {created:<24}  {last}");
            }
            Ok(())
        }
    }
}

fn find(conn: &Connection, email: &str) -> Result<String, String> {
    conn.query_row("SELECT id FROM auth_users WHERE email = ?1", [email], |r| r.get(0))
        .map_err(|_| format!("no user with email {email}"))
}

/// Answers from stdin: typed on a terminal (passwords hidden), or piped.
struct Prompt {
    lines: std::io::Lines<std::io::StdinLock<'static>>,
    tty: bool,
}

impl Prompt {
    fn new() -> Prompt {
        let stdin = std::io::stdin();
        Prompt { tty: stdin.is_terminal(), lines: stdin.lock().lines() }
    }

    /// Prompts only on a terminal; piped answers are read silently.
    fn line(&mut self, prompt: &str) -> Result<String, String> {
        if self.tty {
            eprint!("{prompt}");
            let _ = std::io::stderr().flush();
        }
        match self.lines.next() {
            Some(Ok(line)) => Ok(line.trim_end_matches('\r').to_string()),
            Some(Err(e)) => Err(e.to_string()),
            None => Err("no answer on stdin".into()),
        }
    }

    fn hidden(&mut self, prompt: &str) -> Result<String, String> {
        if !self.tty {
            return self.line(prompt);
        }
        let _ = Command::new("stty").arg("-echo").status();
        let line = self.line(prompt);
        let _ = Command::new("stty").arg("echo").status();
        eprintln!();
        line
    }

    /// A password asked twice, at least `min` characters.
    fn new_password(&mut self, min: usize) -> Result<String, String> {
        let password = self.hidden("Password: ")?;
        if password.chars().count() < min {
            return Err(format!("the password must be at least {min} characters"));
        }
        if self.hidden("Confirm password: ")? != password {
            return Err("the passwords don't match".into());
        }
        Ok(password)
    }
}
