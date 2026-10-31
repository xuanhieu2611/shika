//! Which provider files belong to a target: Claude Code project directories
//! and their subagents, and Codex threads from its state index.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::{Connection, OpenFlags, params};

use super::BUSY;
use super::registry::Target;

pub(super) struct InputFile {
    pub(super) path: PathBuf,
    pub(super) cli: &'static str,
    pub(super) session: String,
}

pub(super) fn registered_jsonl(dir: &Path) -> std::io::Result<(Vec<PathBuf>, bool)> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), false)),
        Err(err) => return Err(err),
    };
    let mut files = Vec::new();
    let mut pending = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                pending = true;
                continue;
            }
        };
        if entry.path().extension().is_some_and(|ext| ext == "jsonl") {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                files.push(entry.path());
            } else {
                // Do not follow symlinks or let one unsafe/missing entry hide
                // other valid sessions in this registered directory.
                pending = true;
            }
        }
    }
    Ok((files, pending))
}

pub(super) fn input_files(
    store: &Connection,
    target: &Target,
) -> rusqlite::Result<(Vec<InputFile>, bool)> {
    let mut files = BTreeMap::new();
    let mut pending = false;
    let source = target.id();
    let cli = match target {
        Target::Claude { .. } => "claude",
        Target::Codex { .. } => "codex",
    };
    // Keep independently readable children even if their parent's log or the
    // provider's discovery index disappears after a previous successful read.
    let mut known =
        store.prepare("SELECT path,session FROM source_files WHERE source=?1 AND cli=?2")?;
    for row in known.query_map(params![source, cli], |r| {
        Ok((
            PathBuf::from(r.get::<_, String>(0)?),
            r.get::<_, String>(1)?,
        ))
    })? {
        let (path, session) = row?;
        files.insert(path.clone(), InputFile { path, cli, session });
    }
    match target {
        Target::Claude { directories } => {
            for dir in directories {
                let (paths, incomplete) = match registered_jsonl(dir) {
                    Ok(paths) => paths,
                    Err(_) => {
                        pending = true;
                        continue;
                    }
                };
                pending |= incomplete;
                for path in paths {
                    let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned)
                    else {
                        pending = true;
                        continue;
                    };
                    let children = path.with_extension("").join("subagents");
                    let (children, incomplete) =
                        registered_jsonl(&children).unwrap_or((Vec::new(), true));
                    pending |= incomplete;
                    for child in children {
                        files.insert(
                            child.clone(),
                            InputFile {
                                path: child,
                                cli: "claude",
                                session: name.clone(),
                            },
                        );
                    }
                    files.insert(
                        path.clone(),
                        InputFile {
                            path,
                            cli: "claude",
                            session: name,
                        },
                    );
                }
            }
        }
        Target::Codex { root, cwds } => {
            let discovered = (|| -> rusqlite::Result<()> {
                let mut newest = None;
                let entries = match fs::read_dir(root) {
                    Ok(entries) => entries,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound && files.is_empty() => {
                        return Ok(());
                    }
                    Err(_) => return Err(rusqlite::Error::InvalidQuery),
                };
                for entry in entries {
                    let entry = entry.map_err(|_| rusqlite::Error::InvalidQuery)?;
                    let name = entry.file_name();
                    let Some(number) = name
                        .to_str()
                        .and_then(|n| n.strip_prefix("state_"))
                        .and_then(|n| n.strip_suffix(".sqlite"))
                        .and_then(|n| n.parse::<u32>().ok())
                    else {
                        continue;
                    };
                    if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    if newest.as_ref().is_none_or(|(n, _)| number > *n) {
                        newest = Some((number, entry.path()));
                    }
                }
                let Some((_, db)) = newest else {
                    let seen: bool = store.query_row(
                        "SELECT EXISTS(SELECT 1 FROM indexes WHERE source=?1)",
                        [&source],
                        |r| r.get(0),
                    )?;
                    // An untouched home has no index yet. Once storage was
                    // observed, losing it must remain visible and retryable.
                    if seen
                        || !files.is_empty()
                        || root.join("sessions").exists()
                        || root.join("archived_sessions").exists()
                    {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    return Ok(());
                };
                store.execute("INSERT OR IGNORE INTO indexes VALUES (?1)", [&source])?;
                // SQLite NOFOLLOW rejects symlinks in parent components too.
                // Honor a linked provider home while still refusing a linked
                // final index filename, including a replacement racing open.
                let parent = db.parent().ok_or(rusqlite::Error::InvalidQuery)?;
                let name = db.file_name().ok_or(rusqlite::Error::InvalidQuery)?;
                let parent = parent
                    .canonicalize()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                let conn = Connection::open_with_flags(
                    parent.join(name),
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
                )?;
                conn.busy_timeout(BUSY)?;
                let start = Instant::now();
                conn.progress_handler(1000, Some(move || start.elapsed() > BUSY));
                let edges: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='thread_spawn_edges')", [], |r| r.get(0))?;
                let sql = if edges {
                    "WITH RECURSIVE owned(id) AS (
                   SELECT id FROM threads WHERE cwd=?1 AND source='cli'
                   UNION SELECT child_thread_id FROM thread_spawn_edges JOIN owned ON parent_thread_id=owned.id
                 ) SELECT threads.id,rollout_path FROM threads JOIN owned ON threads.id=owned.id"
                } else {
                    // A matching cwd alone does not establish child ownership.
                    // Keep valid CLI-root totals, but surface the discovery gap.
                    pending = true;
                    "SELECT id,rollout_path FROM threads WHERE cwd=?1 AND source='cli'"
                };
                for cwd in cwds {
                    let mut stmt = conn.prepare(sql)?;
                    let rows = stmt.query_map([cwd.to_string_lossy().as_ref()], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            PathBuf::from(r.get::<_, String>(1)?),
                        ))
                    })?;
                    for row in rows {
                        let (session, path) = row?;
                        files.insert(
                            path.clone(),
                            InputFile {
                                path,
                                cli: "codex",
                                session,
                            },
                        );
                    }
                }
                Ok(())
            })();
            pending |= discovered.is_err();
        }
    }
    Ok((files.into_values().collect(), pending))
}
